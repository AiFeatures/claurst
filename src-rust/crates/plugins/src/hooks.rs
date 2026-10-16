/// Plugin hooks: parsing `hooks/hooks.json` (or a manifest's inline `hooks`).
///
/// Plugin hooks use the same shape as settings hooks and run through the
/// core hook engine (`claurst_core::hooks`); `set_global_hooks` registers
/// them at startup.
use crate::manifest::PluginHooksConfig;

/// Parse a hooks value that is either the events map itself
/// (`{ "PreToolUse": [...] }`) or wraps it under `hooks`
/// (`{ "description": "...", "hooks": { "PreToolUse": [...] } }`).
///
/// Unsupported events and malformed entries are skipped with a warning.
/// Returns `None` when the value is not an object.
pub fn parse_hooks_value(value: &serde_json::Value) -> Option<PluginHooksConfig> {
    let (events_value, description) = match value.get("hooks") {
        Some(inner) => (
            inner,
            value.get("description").and_then(|d| d.as_str()).map(String::from),
        ),
        None => (value, None),
    };
    if !events_value.is_object() {
        tracing::warn!("Plugin hooks: expected an object of event name to matcher list");
        return None;
    }
    let (events, warnings) = claurst_core::config::parse_hook_map(events_value);
    for warning in warnings {
        tracing::warn!("Plugin hooks: {warning}");
    }
    Some(PluginHooksConfig { description, events })
}

/// Number of hook commands in a plugin's hooks config.
pub fn hook_count(config: &PluginHooksConfig) -> usize {
    config.events.values().flatten().map(|group| group.hooks.len()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use claurst_core::config::HookEvent;

    #[test]
    fn parse_wrapped_hooks_value() {
        let raw = serde_json::json!({
            "description": "test hooks",
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo pre", "timeout": 5 }] }
                ]
            }
        });
        let config = parse_hooks_value(&raw).unwrap();
        assert_eq!(config.description.as_deref(), Some("test hooks"));
        let groups = &config.events[&HookEvent::PreToolUse];
        assert_eq!(groups[0].matcher.as_deref(), Some("Bash"));
        assert_eq!(groups[0].hooks[0].command, "echo pre");
        assert_eq!(groups[0].hooks[0].timeout, Some(5));
        assert_eq!(hook_count(&config), 1);
    }

    #[test]
    fn parse_flat_hooks_value_skips_unsupported_events() {
        let raw = serde_json::json!({
            "Stop": [ { "hooks": [{ "type": "command", "command": "echo stop" }] } ],
            "WorktreeCreate": [ { "hooks": [{ "type": "command", "command": "echo wt" }] } ]
        });
        let config = parse_hooks_value(&raw).unwrap();
        assert!(config.events.contains_key(&HookEvent::Stop));
        assert_eq!(config.events.len(), 1);
    }

    #[test]
    fn non_object_hooks_value_is_rejected() {
        assert!(parse_hooks_value(&serde_json::json!(["Stop"])).is_none());
    }
}
