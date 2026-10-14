//! Hook engine (#447).
//!
//! Runs the command hooks configured for an event, following Claude Code's
//! hook contract:
//!
//! - stdin: one JSON object with `session_id`, `cwd`, `hook_event_name` and
//!   the event's own fields (`tool_name`, `tool_input`, `tool_response`,
//!   `prompt`, ...); see [`HookEventData`].
//! - exit 0: stdout may be a JSON object with `decision` (`allow`, `deny` or
//!   `ask`; `approve` and `block` are accepted as aliases), `reason`,
//!   `updatedInput` and `additionalContext`, top-level or under
//!   `hookSpecificOutput` (`permissionDecision`, `permissionDecisionReason`,
//!   `updatedInput`, `additionalContext`). Plain-text stdout from
//!   `UserPromptSubmit` and `SessionStart` hooks is added as context; for
//!   other events it is only logged.
//! - exit 2: deny, with stderr as the reason.
//! - any other exit status, a spawn failure or a timeout: a non-blocking
//!   error that is logged and otherwise ignored.
//!
//! The hooks selected for an event run one at a time in configuration order:
//! settings hooks first, then plugin hooks. Their results merge into one
//! [`HookResult`]: deny beats ask beats allow, the reasons given with the
//! winning decision are joined, `additionalContext` is concatenated and the
//! last `updatedInput` wins. A hook still running when its timeout expires
//! (default [`DEFAULT_HOOK_TIMEOUT_SECS`]) is killed, together with its
//! process group on Unix.

use crate::config::{HookCommand, HookEvent, HookMap};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

/// Seconds a hook may run before it is killed, unless it sets `timeout`.
pub const DEFAULT_HOOK_TIMEOUT_SECS: u64 = 60;

/// Bytes of a hook's stdout (and of its stderr) that are kept; the rest is
/// read and discarded.
const MAX_HOOK_OUTPUT_BYTES: u64 = 1024 * 1024;

/// How the session started: `SessionStart`'s `source` and matcher target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStartSource {
    /// A new session.
    Startup,
    /// A session continued with `--resume`.
    Resume,
}

impl SessionStartSource {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionStartSource::Startup => "startup",
            SessionStartSource::Resume => "resume",
        }
    }
}

/// Why the session ended: `SessionEnd`'s `reason` and matcher target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEndReason {
    /// The user quit the interactive session.
    PromptInputExit,
    /// Any other end, such as a headless run finishing.
    Other,
}

impl SessionEndReason {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionEndReason::PromptInputExit => "prompt_input_exit",
            SessionEndReason::Other => "other",
        }
    }
}

/// What started a compaction: `PreCompact`'s `trigger` and matcher target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactTrigger {
    /// `/compact`.
    Manual,
    /// The context-window threshold or a context-window error.
    Auto,
}

impl CompactTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            CompactTrigger::Manual => "manual",
            CompactTrigger::Auto => "auto",
        }
    }
}

/// A tool's result as sent to `PostToolUse` hooks in `tool_response`.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ToolResponse<'a> {
    pub content: &'a str,
    pub is_error: bool,
}

/// The event-specific fields of a hook's stdin payload. The variant decides
/// the event.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(untagged)]
pub enum HookEventData<'a> {
    PreToolUse {
        tool_name: &'a str,
        tool_input: &'a Value,
    },
    PostToolUse {
        tool_name: &'a str,
        tool_input: &'a Value,
        tool_response: ToolResponse<'a>,
    },
    UserPromptSubmit {
        prompt: &'a str,
    },
    /// `stop_hook_active` is true when this stop follows a continuation that
    /// a Stop hook forced, so a hook can avoid blocking forever.
    Stop {
        stop_hook_active: bool,
        last_assistant_message: &'a str,
    },
    SubagentStop {
        stop_hook_active: bool,
        last_assistant_message: &'a str,
    },
    SessionStart {
        source: SessionStartSource,
    },
    SessionEnd {
        reason: SessionEndReason,
    },
    Notification {
        message: &'a str,
    },
    PreCompact {
        trigger: CompactTrigger,
        /// The `/compact` instructions; empty for automatic compaction.
        custom_instructions: &'a str,
    },
    PostModelTurn {
        last_assistant_message: &'a str,
    },
}

impl HookEventData<'_> {
    pub fn event(&self) -> HookEvent {
        match self {
            HookEventData::PreToolUse { .. } => HookEvent::PreToolUse,
            HookEventData::PostToolUse { .. } => HookEvent::PostToolUse,
            HookEventData::UserPromptSubmit { .. } => HookEvent::UserPromptSubmit,
            HookEventData::Stop { .. } => HookEvent::Stop,
            HookEventData::SubagentStop { .. } => HookEvent::SubagentStop,
            HookEventData::SessionStart { .. } => HookEvent::SessionStart,
            HookEventData::SessionEnd { .. } => HookEvent::SessionEnd,
            HookEventData::Notification { .. } => HookEvent::Notification,
            HookEventData::PreCompact { .. } => HookEvent::PreCompact,
            HookEventData::PostModelTurn { .. } => HookEvent::PostModelTurn,
        }
    }

    /// The value a matcher is tested against, or `None` when the event
    /// ignores matchers.
    fn matcher_target(&self) -> Option<&str> {
        match self {
            HookEventData::PreToolUse { tool_name, .. }
            | HookEventData::PostToolUse { tool_name, .. } => Some(tool_name),
            HookEventData::SessionStart { source } => Some(source.as_str()),
            HookEventData::SessionEnd { reason } => Some(reason.as_str()),
            HookEventData::PreCompact { trigger, .. } => Some(trigger.as_str()),
            _ => None,
        }
    }
}

/// The JSON object written to a hook's stdin.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct HookPayload<'a> {
    pub session_id: &'a str,
    pub cwd: &'a Path,
    pub hook_event_name: HookEvent,
    #[serde(flatten)]
    pub data: HookEventData<'a>,
}

impl<'a> HookPayload<'a> {
    pub fn new(session_id: &'a str, cwd: &'a Path, data: HookEventData<'a>) -> Self {
        Self { session_id, cwd, hook_event_name: data.event(), data }
    }
}

/// A hook's verdict. Ordered so that the strictest decision is the largest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookDecision {
    #[serde(alias = "approve")]
    Allow,
    Ask,
    #[serde(alias = "block")]
    Deny,
}

/// The merged result of every hook that ran for one event.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HookResult {
    /// The strictest decision any hook returned; `None` when none decided.
    pub decision: Option<HookDecision>,
    /// The reasons given with `decision`, one per line.
    pub reason: Option<String>,
    /// Replacement tool input (`PreToolUse`); the last hook to set it wins.
    pub updated_input: Option<Value>,
    /// Context from every hook, one block per line, in run order.
    pub additional_context: Option<String>,
}

impl HookResult {
    pub fn is_deny(&self) -> bool {
        self.decision == Some(HookDecision::Deny)
    }

    /// The reason to report for a deny (or ask) decision.
    pub fn reason_or_default(&self) -> &str {
        self.reason.as_deref().unwrap_or("blocked by hook")
    }

    fn absorb(&mut self, verdict: HookVerdict) {
        if let Some(decision) = verdict.decision {
            match self.decision {
                Some(current) if current > decision => {}
                Some(current) if current == decision => {
                    append_line(&mut self.reason, verdict.reason);
                }
                _ => {
                    self.decision = Some(decision);
                    self.reason = verdict.reason;
                }
            }
        }
        if verdict.updated_input.is_some() {
            self.updated_input = verdict.updated_input;
        }
        append_line(
            &mut self.additional_context,
            verdict.additional_context.filter(|context| !context.trim().is_empty()),
        );
    }
}

fn append_line(target: &mut Option<String>, addition: Option<String>) {
    let Some(addition) = addition else { return };
    match target {
        Some(existing) => {
            existing.push('\n');
            existing.push_str(&addition);
        }
        None => *target = Some(addition),
    }
}

/// What one hook returned.
#[derive(Debug, Default)]
struct HookVerdict {
    decision: Option<HookDecision>,
    reason: Option<String>,
    updated_input: Option<Value>,
    additional_context: Option<String>,
}

/// A hook's stdout JSON.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookOutput {
    decision: Option<HookDecision>,
    reason: Option<String>,
    updated_input: Option<Value>,
    additional_context: Option<String>,
    hook_specific_output: Option<HookSpecificOutput>,
}

/// Claude Code's nested `hookSpecificOutput` form; wins over top-level fields.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookSpecificOutput {
    permission_decision: Option<HookDecision>,
    permission_decision_reason: Option<String>,
    updated_input: Option<Value>,
    additional_context: Option<String>,
}

impl From<HookOutput> for HookVerdict {
    fn from(output: HookOutput) -> Self {
        let specific = output.hook_specific_output.unwrap_or_default();
        HookVerdict {
            decision: specific.permission_decision.or(output.decision),
            reason: specific.permission_decision_reason.or(output.reason),
            updated_input: specific.updated_input.or(output.updated_input),
            additional_context: specific.additional_context.or(output.additional_context),
        }
    }
}

fn parse_stdout(event: HookEvent, command: &str, stdout: &str) -> HookVerdict {
    let text = stdout.trim();
    if text.is_empty() {
        return HookVerdict::default();
    }
    if text.starts_with('{') {
        match serde_json::from_str::<HookOutput>(text) {
            Ok(output) => return output.into(),
            Err(e) => debug!(command, error = %e, "Hook stdout is not valid hook JSON; treating it as text"),
        }
    }
    if matches!(event, HookEvent::UserPromptSubmit | HookEvent::SessionStart) {
        HookVerdict { additional_context: Some(text.to_string()), ..Default::default() }
    } else {
        debug!(command, stdout = text, "Hook output ignored");
        HookVerdict::default()
    }
}

/// `true` when a matcher selects `target`. A missing target (events without
/// matchers) and an absent, empty or `*` pattern match everything; a plain
/// name or `A|B` list matches exactly; anything else is an unanchored regex,
/// or a glob when it is not a valid regex.
pub fn matcher_matches(pattern: Option<&str>, target: Option<&str>) -> bool {
    let pattern = pattern.map(str::trim).unwrap_or_default();
    let Some(target) = target else { return true };
    if pattern.is_empty() || pattern == "*" {
        return true;
    }
    if pattern.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '|') {
        return pattern.split('|').any(|alternative| alternative == target);
    }
    match regex::Regex::new(pattern) {
        Ok(re) => re.is_match(target),
        Err(_) => glob::Pattern::new(pattern).is_ok_and(|glob| glob.matches(target)),
    }
}

// ---------------------------------------------------------------------------
// Plugin hooks
// ---------------------------------------------------------------------------

static PLUGIN_HOOKS: parking_lot::RwLock<Option<Arc<HookMap>>> = parking_lot::const_rwlock(None);
static PLUGIN_HOOKS_DISABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Replace the hooks contributed by plugins. They run after settings hooks
/// for every event. Called by the plugin loader at startup and on reload.
/// A no-op after [`disable_plugin_hooks`].
pub fn set_plugin_hooks(hooks: HookMap) {
    if PLUGIN_HOOKS_DISABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    *PLUGIN_HOOKS.write() = (!hooks.is_empty()).then(|| Arc::new(hooks));
}

/// Clear plugin hooks for the rest of the process and ignore later
/// registrations (`--bare`, so a plugin reload cannot add hooks back).
pub fn disable_plugin_hooks() {
    PLUGIN_HOOKS_DISABLED.store(true, std::sync::atomic::Ordering::Relaxed);
    *PLUGIN_HOOKS.write() = None;
}

/// The hooks contributed by plugins, if any.
pub fn plugin_hooks() -> Option<Arc<HookMap>> {
    PLUGIN_HOOKS.read().clone()
}

/// `true` when settings or plugins define any hook for `event`.
pub fn has_hooks(hooks: &HookMap, event: HookEvent) -> bool {
    let defined = |map: &HookMap| map.get(&event).is_some_and(|groups| groups.iter().any(|g| !g.hooks.is_empty()));
    defined(hooks) || plugin_hooks().is_some_and(|plugin| defined(plugin.as_ref()))
}

// ---------------------------------------------------------------------------
// Running hooks
// ---------------------------------------------------------------------------

/// Run every hook selected for `payload`'s event, in order, and merge their
/// results. `hooks` are the settings hooks; plugin hooks run after them.
pub async fn run_hooks(hooks: &HookMap, payload: &HookPayload<'_>) -> HookResult {
    let event = payload.hook_event_name;
    let target = payload.data.matcher_target();
    let plugin = plugin_hooks();
    // Collected before the first await: iterator adaptors with closures held
    // across an await would make this future non-`Send`.
    let selected: Vec<&HookCommand> = hooks
        .get(&event)
        .into_iter()
        .flatten()
        .chain(plugin.as_deref().and_then(|p| p.get(&event)).into_iter().flatten())
        .filter(|group| matcher_matches(group.matcher.as_deref(), target))
        .flat_map(|group| group.hooks.iter())
        .collect();

    let mut result = HookResult::default();
    if selected.is_empty() {
        return result;
    }
    let input = match serde_json::to_vec(payload) {
        Ok(input) => input,
        Err(e) => {
            warn!(event = event.as_str(), error = %e, "Failed to serialize hook payload");
            return result;
        }
    };
    for hook in selected {
        debug!(command = %hook.command, event = event.as_str(), "Running hook");
        result.absorb(run_command(hook, event, &input, payload.cwd).await);
    }
    result
}

/// Fire `Notification` hooks without waiting for them. Callable from
/// synchronous code running inside a Tokio runtime; a no-op outside one.
pub fn notify(hooks: &HookMap, session_id: &str, cwd: &Path, message: &str) {
    if !has_hooks(hooks, HookEvent::Notification) {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
    let mut notification_hooks = HookMap::new();
    if let Some(groups) = hooks.get(&HookEvent::Notification) {
        notification_hooks.insert(HookEvent::Notification, groups.clone());
    }
    let (session_id, cwd, message) = (session_id.to_owned(), cwd.to_owned(), message.to_owned());
    runtime.spawn(async move {
        let data = HookEventData::Notification { message: &message };
        run_hooks(&notification_hooks, &HookPayload::new(&session_id, &cwd, data)).await;
    });
}

/// Run one hook command with `input` on stdin and interpret its result.
async fn run_command(hook: &HookCommand, event: HookEvent, input: &[u8], cwd: &Path) -> HookVerdict {
    use tokio::io::AsyncWriteExt;

    let (shell, flag) = if cfg!(windows) { ("cmd", "/C") } else { ("sh", "-c") };
    let mut command = tokio::process::Command::new(shell);
    command
        .arg(flag)
        .arg(&hook.command)
        .current_dir(cwd)
        .env("CLAUDE_PROJECT_DIR", cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(plugin) = &hook.plugin {
        command
            .env("CLAUDE_PLUGIN_ROOT", &plugin.root)
            .env("CLAUDE_PLUGIN_NAME", &plugin.name);
    }
    // Own process group, so a timeout can kill everything the hook started.
    #[cfg(unix)]
    command.process_group(0);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            warn!(command = %hook.command, error = %e, "Failed to spawn hook");
            return HookVerdict::default();
        }
    };
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    // `timeout: 0` would kill every hook at once; treat it as unset.
    let timeout_secs = hook.timeout.filter(|&secs| secs > 0).unwrap_or(DEFAULT_HOOK_TIMEOUT_SECS);
    let timeout = Duration::from_secs(timeout_secs);

    let finished = tokio::time::timeout(timeout, async {
        let feed = async {
            if let Some(mut stdin) = stdin {
                // A hook that never reads stdin closes the pipe early; not an error.
                let _ = stdin.write_all(input).await;
            }
        };
        // Wait only after the pipes close: reaping `sh` first would drop its
        // pid, and a timeout could then no longer kill a grandchild that
        // still holds stdout.
        let (_, out, err) = tokio::join!(feed, read_capped(stdout), read_capped(stderr));
        (out, err, child.wait().await)
    })
    .await;

    let (stdout, stderr, status) = match finished {
        Ok((stdout, stderr, Ok(status))) => (stdout, stderr, status),
        Ok((_, _, Err(e))) => {
            warn!(command = %hook.command, error = %e, "Hook wait failed");
            return HookVerdict::default();
        }
        Err(_) => {
            kill_tree(&mut child);
            let _ = child.wait().await;
            warn!(
                command = %hook.command,
                event = event.as_str(),
                timeout_secs = timeout.as_secs(),
                "Hook timed out and was killed"
            );
            return HookVerdict::default();
        }
    };

    let stdout = String::from_utf8_lossy(&stdout);
    let stderr = String::from_utf8_lossy(&stderr);
    match status.code() {
        Some(0) => parse_stdout(event, &hook.command, &stdout),
        Some(2) => HookVerdict {
            decision: Some(HookDecision::Deny),
            reason: Some(match stderr.trim() {
                "" => format!("blocked by hook `{}`", hook.command),
                reason => reason.to_string(),
            }),
            ..Default::default()
        },
        code => {
            warn!(
                command = %hook.command,
                event = event.as_str(),
                exit_code = ?code,
                stderr = %stderr.trim(),
                "Hook failed (non-blocking)"
            );
            HookVerdict::default()
        }
    }
}

/// Read a hook's output pipe, keeping the first [`MAX_HOOK_OUTPUT_BYTES`]
/// and draining the rest so the hook never blocks on a full pipe.
async fn read_capped<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let Some(pipe) = pipe else { return buf };
    let mut limited = pipe.take(MAX_HOOK_OUTPUT_BYTES);
    let _ = limited.read_to_end(&mut buf).await;
    let _ = tokio::io::copy(&mut limited.into_inner(), &mut tokio::io::sink()).await;
    buf
}

/// Kill a timed-out hook and, on Unix, its whole process group.
fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pgid) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pgid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    let _ = child.start_kill();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{parse_hook_map, HookMatcher, HookType};
    use serde_json::json;

    fn command(command: &str) -> HookCommand {
        HookCommand { kind: HookType::Command, command: command.to_string(), timeout: None, plugin: None }
    }

    fn hooks_for(event: HookEvent, matcher: Option<&str>, commands: Vec<HookCommand>) -> HookMap {
        let mut map = HookMap::new();
        map.insert(event, vec![HookMatcher { matcher: matcher.map(str::to_string), hooks: commands }]);
        map
    }

    fn payload_json(data: HookEventData<'_>) -> Value {
        serde_json::to_value(HookPayload::new("sess-1", Path::new("/work"), data)).unwrap()
    }

    #[test]
    fn payload_carries_common_and_event_fields() {
        let input = json!({"command": "ls"});
        let pre = payload_json(HookEventData::PreToolUse { tool_name: "Bash", tool_input: &input });
        assert_eq!(
            pre,
            json!({"session_id": "sess-1", "cwd": "/work", "hook_event_name": "PreToolUse",
                   "tool_name": "Bash", "tool_input": {"command": "ls"}})
        );

        let post = payload_json(HookEventData::PostToolUse {
            tool_name: "Bash",
            tool_input: &input,
            tool_response: ToolResponse { content: "out", is_error: true },
        });
        assert_eq!(post["hook_event_name"], "PostToolUse");
        assert_eq!(post["tool_response"], json!({"content": "out", "is_error": true}));

        let prompt = payload_json(HookEventData::UserPromptSubmit { prompt: "hi" });
        assert_eq!(prompt["hook_event_name"], "UserPromptSubmit");
        assert_eq!(prompt["prompt"], "hi");

        let stop = payload_json(HookEventData::Stop { stop_hook_active: true, last_assistant_message: "done" });
        assert_eq!(stop["hook_event_name"], "Stop");
        assert_eq!(stop["stop_hook_active"], true);
        assert_eq!(stop["last_assistant_message"], "done");

        let sub = payload_json(HookEventData::SubagentStop { stop_hook_active: false, last_assistant_message: "x" });
        assert_eq!(sub["hook_event_name"], "SubagentStop");

        let start = payload_json(HookEventData::SessionStart { source: SessionStartSource::Resume });
        assert_eq!(start["hook_event_name"], "SessionStart");
        assert_eq!(start["source"], "resume");

        let end = payload_json(HookEventData::SessionEnd { reason: SessionEndReason::PromptInputExit });
        assert_eq!(end["hook_event_name"], "SessionEnd");
        assert_eq!(end["reason"], "prompt_input_exit");

        let note = payload_json(HookEventData::Notification { message: "needs permission" });
        assert_eq!(note["hook_event_name"], "Notification");
        assert_eq!(note["message"], "needs permission");

        let compact = payload_json(HookEventData::PreCompact {
            trigger: CompactTrigger::Manual,
            custom_instructions: "keep tests",
        });
        assert_eq!(compact["hook_event_name"], "PreCompact");
        assert_eq!(compact["trigger"], "manual");
        assert_eq!(compact["custom_instructions"], "keep tests");

        let turn = payload_json(HookEventData::PostModelTurn { last_assistant_message: "m" });
        assert_eq!(turn["hook_event_name"], "PostModelTurn");
    }

    #[test]
    fn matcher_exact_list_regex_and_glob() {
        assert!(matcher_matches(None, Some("Bash")));
        assert!(matcher_matches(Some(""), Some("Bash")));
        assert!(matcher_matches(Some("*"), Some("Bash")));
        assert!(matcher_matches(Some("Bash"), Some("Bash")));
        assert!(!matcher_matches(Some("Bash"), Some("BashOutput")), "plain names match exactly");
        assert!(matcher_matches(Some("Edit|Write"), Some("Write")));
        assert!(!matcher_matches(Some("Edit|Write"), Some("Read")));
        assert!(matcher_matches(Some("Notebook.*"), Some("NotebookEdit")));
        assert!(matcher_matches(Some("^mcp__.*"), Some("mcp__github__search")));
        assert!(!matcher_matches(Some("^mcp__.*"), Some("Bash")));
        assert!(matcher_matches(Some("*Edit"), Some("NotebookEdit")), "invalid regex falls back to glob");
        assert!(!matcher_matches(Some("*Edit"), Some("Read")));
        assert!(matcher_matches(Some("Bash"), None), "events without a target ignore matchers");
    }

    #[test]
    fn decision_merge_deny_wins_context_concatenates_input_last_wins() {
        let mut result = HookResult::default();
        result.absorb(HookVerdict {
            decision: Some(HookDecision::Allow),
            reason: Some("fine".into()),
            updated_input: Some(json!({"a": 1})),
            additional_context: Some("one".into()),
        });
        result.absorb(HookVerdict {
            decision: Some(HookDecision::Deny),
            reason: Some("no".into()),
            updated_input: None,
            additional_context: Some("two".into()),
        });
        result.absorb(HookVerdict {
            decision: Some(HookDecision::Ask),
            reason: Some("maybe".into()),
            updated_input: Some(json!({"a": 2})),
            additional_context: None,
        });
        result.absorb(HookVerdict { decision: Some(HookDecision::Deny), reason: Some("never".into()), ..Default::default() });
        assert_eq!(result.decision, Some(HookDecision::Deny));
        assert_eq!(result.reason.as_deref(), Some("no\nnever"));
        assert_eq!(result.updated_input, Some(json!({"a": 2})));
        assert_eq!(result.additional_context.as_deref(), Some("one\ntwo"));
    }

    #[test]
    fn stdout_json_forms() {
        let top = parse_stdout(
            HookEvent::PreToolUse,
            "h",
            r#"{"decision":"block","reason":"r","updatedInput":{"x":1},"additionalContext":"c"}"#,
        );
        assert_eq!(top.decision, Some(HookDecision::Deny));
        assert_eq!(top.reason.as_deref(), Some("r"));
        assert_eq!(top.updated_input, Some(json!({"x": 1})));
        assert_eq!(top.additional_context.as_deref(), Some("c"));

        let nested = parse_stdout(
            HookEvent::PreToolUse,
            "h",
            r#"{"decision":"deny","hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask","permissionDecisionReason":"check"}}"#,
        );
        assert_eq!(nested.decision, Some(HookDecision::Ask), "hookSpecificOutput wins");
        assert_eq!(nested.reason.as_deref(), Some("check"));

        let approve = parse_stdout(HookEvent::PreToolUse, "h", r#"{"decision":"approve"}"#);
        assert_eq!(approve.decision, Some(HookDecision::Allow));

        let text_prompt = parse_stdout(HookEvent::UserPromptSubmit, "h", "  remember X \n");
        assert_eq!(text_prompt.additional_context.as_deref(), Some("remember X"));
        let text_start = parse_stdout(HookEvent::SessionStart, "h", "ctx");
        assert_eq!(text_start.additional_context.as_deref(), Some("ctx"));
        let text_tool = parse_stdout(HookEvent::PostToolUse, "h", "logged");
        assert!(text_tool.additional_context.is_none(), "plain stdout is not context for tool events");
    }

    #[test]
    fn parse_hook_map_skips_unknown_events_and_bad_entries() {
        let (hooks, warnings) = parse_hook_map(&json!({
            "PreToolUse": [
                {"matcher": "Bash", "hooks": [{"type": "command", "command": "a", "timeout": 5}]},
                {"command": "old-flat-shape", "blocking": true}
            ],
            "PostCompact": [{"hooks": [{"command": "b"}]}],
            "Stop": [{"hooks": [{"type": "prompt", "prompt": "x"}]}]
        }));
        let pre = &hooks[&HookEvent::PreToolUse];
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0].matcher.as_deref(), Some("Bash"));
        assert_eq!(pre[0].hooks[0].timeout, Some(5));
        assert!(!hooks.contains_key(&HookEvent::Stop), "unimplemented hook types are skipped");
        assert_eq!(warnings.len(), 3, "{warnings:?}");
    }

    #[cfg(unix)]
    mod process {
        use super::*;
        use std::time::Instant;

        fn script(dir: &Path, name: &str, body: &str) -> String {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            format!("sh {}", path.display())
        }

        async fn run(hooks: &HookMap, data: HookEventData<'_>, cwd: &Path) -> HookResult {
            run_hooks(hooks, &HookPayload::new("sess-1", cwd, data)).await
        }

        #[tokio::test]
        async fn hook_receives_payload_on_stdin_in_cwd() {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("payload.json");
            let cmd = script(dir.path(), "cat.sh", &format!("cat > '{}'; pwd > '{}.cwd'", out.display(), out.display()));
            let hooks = hooks_for(HookEvent::UserPromptSubmit, None, vec![command(&cmd)]);
            let result = run(&hooks, HookEventData::UserPromptSubmit { prompt: "hello" }, dir.path()).await;
            assert_eq!(result, HookResult::default());
            let payload: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
            assert_eq!(payload["hook_event_name"], "UserPromptSubmit");
            assert_eq!(payload["prompt"], "hello");
            assert_eq!(payload["session_id"], "sess-1");
            let cwd = std::fs::read_to_string(format!("{}.cwd", out.display())).unwrap();
            assert_eq!(
                std::fs::canonicalize(cwd.trim()).unwrap(),
                std::fs::canonicalize(dir.path()).unwrap()
            );
        }

        #[tokio::test]
        async fn allow_deny_ask_and_updated_input() {
            let dir = tempfile::tempdir().unwrap();
            let input = json!({"command": "rm -rf /"});
            let tool = |hooks: &HookMap| {
                let hooks = hooks.clone();
                let input = input.clone();
                let cwd = dir.path().to_path_buf();
                async move {
                    run(&hooks, HookEventData::PreToolUse { tool_name: "Bash", tool_input: &input }, &cwd).await
                }
            };

            let allow = hooks_for(HookEvent::PreToolUse, Some("Bash"), vec![command(r#"echo '{"decision":"allow","reason":"ok"}'"#)]);
            assert_eq!(tool(&allow).await.decision, Some(HookDecision::Allow));

            let deny = hooks_for(HookEvent::PreToolUse, Some("Bash"), vec![command(r#"echo '{"decision":"deny","reason":"dangerous"}'"#)]);
            let denied = tool(&deny).await;
            assert!(denied.is_deny());
            assert_eq!(denied.reason_or_default(), "dangerous");

            let ask = hooks_for(HookEvent::PreToolUse, Some("Bash"), vec![command(r#"echo '{"decision":"ask","reason":"confirm"}'"#)]);
            assert_eq!(tool(&ask).await.decision, Some(HookDecision::Ask));

            let modify = hooks_for(
                HookEvent::PreToolUse,
                Some("Bash"),
                vec![command(r#"echo '{"updatedInput":{"command":"ls"}}'"#)],
            );
            let modified = tool(&modify).await;
            assert_eq!(modified.decision, None);
            assert_eq!(modified.updated_input, Some(json!({"command": "ls"})));

            let other_tool = hooks_for(HookEvent::PreToolUse, Some("Edit|Write"), vec![command("exit 2")]);
            assert_eq!(tool(&other_tool).await, HookResult::default(), "matcher must not select Bash");
        }

        #[tokio::test]
        async fn exit_two_blocks_with_stderr_and_other_failures_do_not() {
            let dir = tempfile::tempdir().unwrap();
            let block = hooks_for(HookEvent::UserPromptSubmit, None, vec![command("echo 'no secrets' >&2; exit 2")]);
            let result = run(&block, HookEventData::UserPromptSubmit { prompt: "p" }, dir.path()).await;
            assert!(result.is_deny());
            assert_eq!(result.reason.as_deref(), Some("no secrets"));

            let fail = hooks_for(HookEvent::UserPromptSubmit, None, vec![command("echo boom >&2; exit 1")]);
            let result = run(&fail, HookEventData::UserPromptSubmit { prompt: "p" }, dir.path()).await;
            assert_eq!(result, HookResult::default());

            let missing = hooks_for(HookEvent::UserPromptSubmit, None, vec![command("/nonexistent/hook-binary")]);
            let result = run(&missing, HookEventData::UserPromptSubmit { prompt: "p" }, dir.path()).await;
            assert_eq!(result, HookResult::default());
        }

        #[tokio::test]
        async fn timeout_kills_hook_and_its_children() {
            let dir = tempfile::tempdir().unwrap();
            let marker = dir.path().join("survived");
            // The background child would create the marker after 3 s if the
            // process group were not killed.
            let mut slow = command(&format!("(sleep 3; touch '{}') & sleep 30", marker.display()));
            slow.timeout = Some(1);
            let hooks = hooks_for(HookEvent::Stop, None, vec![slow, command(r#"echo '{"decision":"block","reason":"after"}'"#)]);
            let started = Instant::now();
            let result = run(&hooks, HookEventData::Stop { stop_hook_active: false, last_assistant_message: "" }, dir.path()).await;
            assert!(started.elapsed() < Duration::from_secs(10), "timed-out hook must not block");
            assert!(result.is_deny(), "hooks after a timed-out one still run");
            tokio::time::sleep(Duration::from_secs(4)).await;
            assert!(!marker.exists(), "the hook's children must be killed with it");
        }

        /// A hook whose shell exits at once while a background child holds
        /// stdout: the timeout must still kill the child's process group.
        #[tokio::test]
        async fn timeout_kills_children_of_an_exited_shell() {
            let dir = tempfile::tempdir().unwrap();
            let marker = dir.path().join("survived");
            let mut hook = command(&format!("(sleep 3; touch '{}') & exit 0", marker.display()));
            hook.timeout = Some(1);
            let hooks = hooks_for(HookEvent::Stop, None, vec![hook]);
            let started = Instant::now();
            run(&hooks, HookEventData::Stop { stop_hook_active: false, last_assistant_message: "" }, dir.path()).await;
            assert!(started.elapsed() < Duration::from_secs(3));
            tokio::time::sleep(Duration::from_secs(4)).await;
            assert!(!marker.exists(), "the background child must be killed");
        }

        #[tokio::test]
        async fn large_output_is_capped_and_the_exit_code_still_counts() {
            let dir = tempfile::tempdir().unwrap();
            // 3 MiB on stdout and stderr, then exit 2.
            let hooks = hooks_for(
                HookEvent::UserPromptSubmit,
                None,
                vec![command("head -c 3145728 /dev/zero; head -c 3145728 /dev/zero >&2; exit 2")],
            );
            let result = run(&hooks, HookEventData::UserPromptSubmit { prompt: "p" }, dir.path()).await;
            assert!(result.is_deny());
            assert_eq!(result.reason_or_default().len(), MAX_HOOK_OUTPUT_BYTES as usize);
        }

        #[tokio::test]
        async fn zero_timeout_means_the_default() {
            let dir = tempfile::tempdir().unwrap();
            let mut hook = command(r#"sleep 1; echo '{"decision":"deny"}'"#);
            hook.timeout = Some(0);
            let hooks = hooks_for(HookEvent::Stop, None, vec![hook]);
            let result = run(&hooks, HookEventData::Stop { stop_hook_active: false, last_assistant_message: "" }, dir.path()).await;
            assert!(result.is_deny());
        }

        #[tokio::test]
        async fn hooks_run_in_order_and_merge() {
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("order.log");
            let step = |n: u32, out: &str| {
                command(&format!("echo {n} >> '{}'; echo '{out}'", log.display()))
            };
            let mut hooks = HookMap::new();
            hooks.insert(
                HookEvent::PostToolUse,
                vec![
                    HookMatcher {
                        matcher: Some("Read".into()),
                        hooks: vec![step(1, r#"{"additionalContext":"first"}"#), step(2, r#"{"decision":"allow"}"#)],
                    },
                    HookMatcher { matcher: Some("Bash".into()), hooks: vec![step(9, "{}")] },
                    HookMatcher {
                        matcher: None,
                        hooks: vec![step(3, r#"{"decision":"deny","reason":"bad","additionalContext":"third"}"#)],
                    },
                ],
            );
            let input = json!({});
            let result = run(
                &hooks,
                HookEventData::PostToolUse {
                    tool_name: "Read",
                    tool_input: &input,
                    tool_response: ToolResponse { content: "x", is_error: false },
                },
                dir.path(),
            )
            .await;
            assert_eq!(std::fs::read_to_string(&log).unwrap(), "1\n2\n3\n");
            assert!(result.is_deny());
            assert_eq!(result.reason.as_deref(), Some("bad"));
            assert_eq!(result.additional_context.as_deref(), Some("first\nthird"));
        }

        #[tokio::test]
        async fn session_and_compact_matchers_use_source_reason_and_trigger() {
            let dir = tempfile::tempdir().unwrap();
            let start = hooks_for(HookEvent::SessionStart, Some("resume"), vec![command("echo resumed")]);
            let startup = run(&start, HookEventData::SessionStart { source: SessionStartSource::Startup }, dir.path()).await;
            assert_eq!(startup.additional_context, None);
            let resumed = run(&start, HookEventData::SessionStart { source: SessionStartSource::Resume }, dir.path()).await;
            assert_eq!(resumed.additional_context.as_deref(), Some("resumed"));

            let compact = hooks_for(HookEvent::PreCompact, Some("manual"), vec![command("exit 2")]);
            let auto = run(&compact, HookEventData::PreCompact { trigger: CompactTrigger::Auto, custom_instructions: "" }, dir.path()).await;
            assert_eq!(auto, HookResult::default());
            let manual = run(&compact, HookEventData::PreCompact { trigger: CompactTrigger::Manual, custom_instructions: "" }, dir.path()).await;
            assert!(manual.is_deny());
        }

        /// Plugin hooks run through the same engine, after settings hooks,
        /// with the plugin's root exported. SessionEnd is used only by this
        /// test in this crate, so the process-global registry cannot leak
        /// into other tests run in parallel.
        #[tokio::test]
        async fn plugin_hooks_run_after_settings_hooks() {
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("order.log");
            let mut plugin_hook = command(&format!("echo \"plugin $CLAUDE_PLUGIN_NAME $CLAUDE_PLUGIN_ROOT\" >> '{}'", log.display()));
            plugin_hook.plugin = Some(crate::config::HookPluginSource { name: "demo".into(), root: "/plugins/demo".into() });
            set_plugin_hooks(hooks_for(HookEvent::SessionEnd, None, vec![plugin_hook]));
            assert!(has_hooks(&HookMap::new(), HookEvent::SessionEnd));

            let settings = hooks_for(HookEvent::SessionEnd, None, vec![command(&format!("echo settings >> '{}'", log.display()))]);
            run(&settings, HookEventData::SessionEnd { reason: SessionEndReason::Other }, dir.path()).await;
            set_plugin_hooks(HookMap::new());

            assert_eq!(std::fs::read_to_string(&log).unwrap(), "settings\nplugin demo /plugins/demo\n");
            assert!(!has_hooks(&HookMap::new(), HookEvent::SessionEnd));
        }
    }
}
