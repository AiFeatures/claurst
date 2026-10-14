// Hook call sites for run_query_loop (#447): PreToolUse / PostToolUse around
// each tool call, PostModelTurn after sampling, Stop / SubagentStop once per
// finished turn, and PreCompact before automatic compaction. The engine
// itself lives in `claurst_core::hooks`.

use crate::*;
use claurst_core::config::HookEvent;
use claurst_core::hooks::{
    has_hooks, run_hooks, CompactTrigger, HookDecision, HookEventData, HookPayload, HookResult, ToolResponse,
};
use claurst_core::permissions::{PermissionDecision, PermissionHandler, PermissionRequest};

fn payload<'a>(tool_ctx: &'a ToolContext, data: HookEventData<'a>) -> HookPayload<'a> {
    HookPayload::new(&tool_ctx.session_id, &tool_ctx.working_dir, data)
}

/// What the PreToolUse hooks decided for one tool call.
pub(crate) enum PreToolUse {
    /// Run the tool with `input` (the hooks' `updatedInput` when one was
    /// returned). `ctx` is set when a hook allowed the call, or the user
    /// approved a hook's `ask`: it skips this call's permission prompts.
    Run { input: Value, ctx: Option<Box<ToolContext>> },
    /// Do not run the tool; answer the call with this result.
    Blocked(ToolResult),
}

/// Run the PreToolUse hooks for one call and apply their decision: `deny`
/// blocks the call, `ask` prompts the user even where policy would allow it,
/// `allow` skips the prompts, and `updatedInput` replaces the input. An
/// explicit denial by the permission policy is never overridden.
pub(crate) async fn pre_tool_use(name: &str, input: Value, tool_ctx: &ToolContext) -> PreToolUse {
    let mut result = run_hooks(
        &tool_ctx.config.hooks,
        &payload(tool_ctx, HookEventData::PreToolUse { tool_name: name, tool_input: &input }),
    )
    .await;
    let input = result.updated_input.take().unwrap_or(input);
    match result.decision {
        None => PreToolUse::Run { input, ctx: None },
        Some(HookDecision::Deny) => {
            warn!(tool = name, reason = result.reason_or_default(), "PreToolUse hook denied the call");
            PreToolUse::Blocked(ToolResult::error(format!(
                "Blocked by PreToolUse hook: {}",
                result.reason_or_default()
            )))
        }
        Some(HookDecision::Allow) => PreToolUse::Run { input, ctx: Some(hook_allowed_ctx(tool_ctx)) },
        Some(HookDecision::Ask) => {
            let mut ask_ctx = tool_ctx.clone();
            ask_ctx.permission_handler = Arc::new(HookAskPermissions {
                inner: tool_ctx.permission_handler.clone(),
                reason: format!("PreToolUse hook: {}", result.reason_or_default()),
            });
            let description = synthesize_permission_description(name, &input);
            match ask_ctx.check_permission(name, &description, false) {
                Ok(()) => PreToolUse::Run { input, ctx: Some(hook_allowed_ctx(tool_ctx)) },
                Err(e) => PreToolUse::Blocked(ToolResult::error(e.to_string())),
            }
        }
    }
}

fn hook_allowed_ctx(tool_ctx: &ToolContext) -> Box<ToolContext> {
    let mut ctx = Box::new(tool_ctx.clone());
    ctx.permission_handler = Arc::new(HookAllowedPermissions(tool_ctx.permission_handler.clone()));
    ctx
}

/// Permissions for a call a PreToolUse hook allowed: what would have
/// prompted is allowed; denials stand.
struct HookAllowedPermissions(Arc<dyn PermissionHandler>);

impl HookAllowedPermissions {
    fn resolve(decision: PermissionDecision) -> PermissionDecision {
        match decision {
            PermissionDecision::Ask { .. } => PermissionDecision::Allow,
            other => other,
        }
    }
}

impl PermissionHandler for HookAllowedPermissions {
    fn check_permission(&self, request: &PermissionRequest) -> PermissionDecision {
        Self::resolve(self.0.check_permission(request))
    }

    fn request_permission(&self, request: &PermissionRequest) -> PermissionDecision {
        Self::resolve(self.0.request_permission(request))
    }
}

/// Permissions for a PreToolUse hook's `ask`: anything not denied prompts
/// the user, with the hook's reason.
struct HookAskPermissions {
    inner: Arc<dyn PermissionHandler>,
    reason: String,
}

impl HookAskPermissions {
    fn resolve(&self, decision: PermissionDecision) -> PermissionDecision {
        match decision {
            PermissionDecision::Deny | PermissionDecision::DenyPermanently => decision,
            _ => PermissionDecision::Ask { reason: self.reason.clone() },
        }
    }
}

impl PermissionHandler for HookAskPermissions {
    fn check_permission(&self, request: &PermissionRequest) -> PermissionDecision {
        self.resolve(self.inner.check_permission(request))
    }

    fn request_permission(&self, request: &PermissionRequest) -> PermissionDecision {
        self.resolve(self.inner.request_permission(request))
    }
}

/// Run the PostToolUse hooks for a finished call. Their `additionalContext`,
/// and the reason of a `deny`/`block`, are appended to the tool result the
/// model sees.
pub(crate) async fn post_tool_use(name: &str, input: &Value, result: &mut ToolResult, tool_ctx: &ToolContext) {
    let hook = run_hooks(
        &tool_ctx.config.hooks,
        &payload(
            tool_ctx,
            HookEventData::PostToolUse {
                tool_name: name,
                tool_input: input,
                tool_response: ToolResponse { content: &result.content, is_error: result.is_error },
            },
        ),
    )
    .await;
    if hook.is_deny() {
        result.content.push_str(&format!("\n\nPostToolUse hook feedback: {}", hook.reason_or_default()));
    }
    if let Some(context) = hook.additional_context {
        result.content.push_str(&format!("\n\nPostToolUse hook context:\n{context}"));
    }
}

/// One tool call end to end: PreToolUse hooks, the tool, PostToolUse hooks.
pub(crate) async fn run_tool_with_hooks(
    name: &str,
    input: Value,
    tools: &[Box<dyn Tool>],
    tool_ctx: &ToolContext,
) -> ToolResult {
    match pre_tool_use(name, input, tool_ctx).await {
        PreToolUse::Blocked(result) => result,
        PreToolUse::Run { input, ctx } => {
            let mut result = execute_tool(name, &input, tools, ctx.as_deref().unwrap_or(tool_ctx)).await;
            post_tool_use(name, &input, &mut result, tool_ctx).await;
            result
        }
    }
}

/// Run the PostModelTurn hooks after the model sampled `assistant_msg`. A
/// `deny` (or exit 2) vetoes continuing the loop; `additionalContext` is
/// sent with this turn's tool results.
pub(crate) async fn post_model_turn(assistant_msg: &Message, tool_ctx: &ToolContext) -> HookResult {
    if !has_hooks(&tool_ctx.config.hooks, HookEvent::PostModelTurn) {
        return HookResult::default();
    }
    let text = assistant_msg.get_all_text();
    run_hooks(
        &tool_ctx.config.hooks,
        &payload(tool_ctx, HookEventData::PostModelTurn { last_assistant_message: &text }),
    )
    .await
}

/// Fire Stop (SubagentStop in a sub-agent loop) for a finished turn. Returns
/// the reason when a hook blocked the stop; the loop then sends it to the
/// model and continues.
pub(crate) async fn stop_hooks(
    assistant_msg: &Message,
    stop_hook_active: bool,
    is_subagent: bool,
    tool_ctx: &ToolContext,
) -> Option<String> {
    let event = if is_subagent { HookEvent::SubagentStop } else { HookEvent::Stop };
    if !has_hooks(&tool_ctx.config.hooks, event) {
        return None;
    }
    let text = assistant_msg.get_all_text();
    let data = if is_subagent {
        HookEventData::SubagentStop { stop_hook_active, last_assistant_message: &text }
    } else {
        HookEventData::Stop { stop_hook_active, last_assistant_message: &text }
    };
    let result = run_hooks(&tool_ctx.config.hooks, &payload(tool_ctx, data)).await;
    result.is_deny().then(|| result.reason_or_default().to_string())
}

/// Fire PreCompact before an automatic compaction. Informational: the
/// decision is ignored.
pub(crate) async fn pre_compact(tool_ctx: &ToolContext) {
    run_hooks(
        &tool_ctx.config.hooks,
        &payload(tool_ctx, HookEventData::PreCompact { trigger: CompactTrigger::Auto, custom_instructions: "" }),
    )
    .await;
}
