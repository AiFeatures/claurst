# Hooks

Hooks run shell commands when events happen in a Claurst session: before and after tool calls, when you submit a prompt, when the agent stops, when a session starts or ends, and a few more. A hook receives a JSON description of the event on stdin and answers through its exit code and, optionally, JSON on stdout. Hooks can log, notify, block a tool call, ask for confirmation, rewrite a tool's input, or add context for the model.

The contract follows Claude Code's hooks: the settings shape, event names, stdin fields, stdout fields and exit code 2 are the same, so most Claude Code hook scripts work unchanged. The differences are listed under [Differences from Claude Code](#differences-from-claude-code).

---

## Configuring hooks

Hooks live in the global settings file, `~/.claurst/settings.json`, under a top-level `hooks` key:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          { "type": "command", "command": "~/.claurst/hooks/check-bash.sh", "timeout": 10 }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [
          { "type": "command", "command": "~/.claurst/hooks/format.sh" }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          { "type": "command", "command": "notify-send 'Claurst finished'" }
        ]
      }
    ]
  }
}
```

Each event maps to a list of **matcher groups**. A group has an optional `matcher` and a list of `hooks`:

| Field | Description |
|---|---|
| `matcher` | Selects what the group runs for (see [Matchers](#matchers)). Absent, empty or `"*"` matches everything. |
| `hooks[].type` | `"command"`, the only hook type. May be omitted. |
| `hooks[].command` | Shell command. Run with `sh -c` (`cmd /C` on Windows) in the session's working directory. |
| `hooks[].timeout` | Seconds before the hook is killed. Default `60`. |

The same map is also read from `config.hooks` (the nested `config` block); hooks from there run before the top-level ones.

**Trust.** Hooks run with your privileges, so they are read only from the global settings file. A project's `.claurst/settings.json` cannot define hooks: Claurst ignores them and logs a warning ([configuration](configuration.md)). Hooks shipped by a plugin installed in `~/.claurst/plugins/` run like your own hooks; hooks of a plugin found in a project's `.claurst/plugins/` are ignored ([plugins](plugins.md)). `--bare` runs no hooks at all.

**Parsing.** An entry for an event Claurst does not implement, or a group that does not parse (for example a hook with `"type": "prompt"`), is skipped with a warning in the log; the rest of the file still applies.

The old claurst shape, a flat list of `{ "command", "tool_filter", "blocking" }` entries per event, is no longer read. Wrap each command in a matcher group: `tool_filter` becomes `matcher`; `blocking` has no replacement, because blocking is now decided by the hook itself (exit code 2 or a `deny` decision).

---

## Events

| Event | When it fires | Matcher tested against | Can block? |
|---|---|---|---|
| `PreToolUse` | Before a tool runs | Tool name | Yes: deny, ask, allow, rewrite the input |
| `PostToolUse` | After a tool returns | Tool name | No; can add text to the tool result |
| `UserPromptSubmit` | When you submit a prompt | (none) | Yes: drops the prompt; can add context |
| `Stop` | When the main agent finishes its turn | (none) | Yes: the agent continues with your reason |
| `SubagentStop` | When a sub-agent (the `Agent` tool) finishes its turn | (none) | Yes: the sub-agent continues with your reason |
| `SessionStart` | When an interactive session starts or resumes, and at the start of `--print` | `source` | No; can add context |
| `SessionEnd` | When you quit the interactive session, and at the end of `--print` | `reason` | No |
| `Notification` | When Claurst shows a permission prompt | (none) | No |
| `PreCompact` | Before the conversation is compacted | `trigger` | No |
| `PostModelTurn` | After the model responds, before its tool calls run (Claurst-specific) | (none) | Yes: ends the turn; can add context |

`Stop` fires exactly once per finished turn, whether the turn ended normally, on a stop sequence or on an unknown stop reason. It does not fire when the turn is cancelled, fails, runs out of output tokens or exceeds the budget.

### Event details

**`PreToolUse`** runs before every tool call, including the tool calls of sub-agents. Its decision feeds the permission system:

- `deny` (or exit 2): the tool does not run; the model receives `Blocked by PreToolUse hook: <reason>` as the tool result.
- `ask`: you are asked to approve the call even if your permission settings would allow it. In non-interactive mode (`--print`) nothing can answer, so the call is denied.
- `allow`: the call runs without a permission prompt. An explicit denial by your permission settings (for example plan mode, or a deny rule) still wins.
- `updatedInput`: replaces the tool's input. The model's original call stays in the conversation.

**`PostToolUse`** runs after a tool call completes (not for calls that were blocked, and not when the batch was cancelled). `additionalContext` is appended to the tool result the model sees; the `reason` of a `deny` decision is appended as feedback. The tool's effects are not undone.

**`UserPromptSubmit`** runs before your prompt is sent. `deny` (or exit 2) keeps the prompt out of the conversation and shows the reason (in the TUI the prompt text is restored for editing; in `--print` the run exits with status 1). `additionalContext`, or any plain-text stdout, is sent with the prompt.

**`Stop` / `SubagentStop`**: a `deny` (`block`) decision or exit 2 stops the agent from stopping. Your reason is sent to the model as `Stop hook feedback: <reason>` and the loop continues (still bounded by the turn limit). The next Stop payload has `stop_hook_active: true`; check it to avoid blocking forever.

**`SessionStart`**: `source` is `startup` or `resume` (`--resume`). `additionalContext`, or any plain-text stdout, is added to the session context sent with every request.

**`SessionEnd`**: `reason` is `prompt_input_exit` (you quit the TUI) or `other` (the end of a `--print` run). Claurst waits for the hooks before exiting.

**`Notification`**: fires when a permission prompt is shown, without waiting for the hook. Use it for desktop notifications.

**`PreCompact`**: `trigger` is `manual` (`/compact`) or `auto` (the context-window threshold, or a context-window error). Informational: the decision is ignored.

**`PostModelTurn`**: a `deny` decision or exit 2 ends the turn before the model's tool calls run. `additionalContext` is sent to the model with this turn's tool results (it is dropped on a turn without tool calls).

---

## Matchers

`matcher` selects which calls a group runs for. It is tested against the tool name for `PreToolUse`/`PostToolUse`, against `source` for `SessionStart`, `reason` for `SessionEnd` and `trigger` for `PreCompact`. Other events ignore it.

| Pattern | Meaning |
|---|---|
| absent, `""`, `"*"` | Everything |
| `Bash`, `Edit\|Write` | Exact names: a pattern made only of letters, digits, `_` and `\|` is a list of exact names |
| `Notebook.*`, `^mcp__` | Anything else is a regular expression, matched anywhere in the name (anchor with `^...$` to match the whole name) |
| `*Edit` | A pattern that is not a valid regular expression is used as a glob |

Built-in tool names include `Bash`, `Read`, `Edit`, `Write`, `Glob`, `Grep`, `Agent`, `WebFetch`, `WebSearch`, `TodoWrite`, `NotebookEdit`, `BatchEdit` and `ApplyPatch`.

---

## Input (stdin)

Every hook receives one JSON object on stdin with these common fields:

| Field | Description |
|---|---|
| `session_id` | The session ID |
| `cwd` | The session's working directory (also the hook's working directory) |
| `hook_event_name` | The event, e.g. `"PreToolUse"` |

Plus the event's own fields:

| Event | Fields |
|---|---|
| `PreToolUse` | `tool_name`, `tool_input` |
| `PostToolUse` | `tool_name`, `tool_input` (after any `updatedInput`), `tool_response`: `{ "content": string, "is_error": bool }` |
| `UserPromptSubmit` | `prompt` |
| `Stop`, `SubagentStop` | `stop_hook_active`, `last_assistant_message` |
| `SessionStart` | `source` |
| `SessionEnd` | `reason` |
| `Notification` | `message` |
| `PreCompact` | `trigger`, `custom_instructions` (the `/compact` arguments; empty for automatic compaction) |
| `PostModelTurn` | `last_assistant_message` |

Example `PreToolUse` input:

```json
{
  "session_id": "4f6c1b8e-...",
  "cwd": "/home/me/project",
  "hook_event_name": "PreToolUse",
  "tool_name": "Bash",
  "tool_input": { "command": "rm -rf build" }
}
```

Environment variables set for every hook: `CLAUDE_PROJECT_DIR` (the session's working directory). Plugin hooks also get `CLAUDE_PLUGIN_ROOT` and `CLAUDE_PLUGIN_NAME`.

---

## Output

### Exit codes

| Exit code | Effect |
|---|---|
| `0` | Success. stdout is parsed as described below. |
| `2` | Block: a `deny` decision with stderr as the reason. What blocking means depends on the event (table above). |
| anything else | Non-blocking error: logged, otherwise ignored. |

A hook that cannot be started, or that is still running when its timeout expires, is also a non-blocking error. A timed-out hook is killed, together with every process it started (its process group, on Unix).

### JSON on stdout

On exit 0, stdout that starts with `{` is parsed as JSON:

```json
{
  "decision": "deny",
  "reason": "Use the project's make targets instead of rm",
  "updatedInput": { "command": "make clean" },
  "additionalContext": "The build directory is managed by make."
}
```

| Field | Description |
|---|---|
| `decision` | `"allow"`, `"deny"` or `"ask"`. `"approve"` is accepted for `allow` and `"block"` for `deny`. |
| `reason` | Why; shown to the model or to you, depending on the event. |
| `updatedInput` | `PreToolUse` only: the replacement tool input. |
| `additionalContext` | Text for the model, where the event supports it. |

Claude Code's nested form is accepted too, and wins over the top-level fields:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "ask",
    "permissionDecisionReason": "Touches production config",
    "updatedInput": { "command": "ls" },
    "additionalContext": "..."
  }
}
```

Other Claude Code output fields (`continue`, `stopReason`, `suppressOutput`, `systemMessage`) are ignored. stdout that is not JSON is treated as text: for `UserPromptSubmit` and `SessionStart` it becomes context; for other events it is only logged.

### Several hooks for one event

All hooks whose matcher selects the event run, one at a time, in configuration order: `config.hooks` groups, then top-level `hooks` groups, then plugin hooks (plugins in name order). Each hook gets the same input. The results are merged:

- the strictest decision wins: `deny` over `ask` over `allow`;
- the reasons given with the winning decision are joined, one per line;
- `additionalContext` from every hook is concatenated, one per line;
- the last `updatedInput` wins.

A hook that denies does not stop the later hooks from running.

---

## Plugin hooks

A plugin can ship hooks in `hooks/hooks.json` or inline under `hooks` in its manifest, in the same shape as settings hooks (optionally wrapped as `{ "description": "...", "hooks": { ... } }`). They are registered when Claurst starts (restart to pick up changes; `/plugin reload` and `/reload-plugins` do not re-register hooks) and run through the same engine after settings hooks. See [plugins](plugins.md#hooks).

---

## `/hooks`

`/hooks` opens a read-only browser of the hooks that will run: event, matcher, command, timeout and source (`settings` or the plugin's name). Edit `~/.claurst/settings.json` to change them; hooks are read when Claurst starts.

---

## Examples

### Log every tool call

```json
{
  "hooks": {
    "PreToolUse": [
      { "hooks": [{ "type": "command", "command": "jq -c . >> ~/.claurst/tool-calls.jsonl" }] }
    ]
  }
}
```

### Block dangerous shell commands

```bash
#!/bin/sh
# ~/.claurst/hooks/check-bash.sh  (PreToolUse, matcher "Bash")
command=$(jq -r '.tool_input.command // ""')
case "$command" in
  *"rm -rf /"*|*"mkfs"*|*"dd if="*)
    echo "Refusing to run: $command" >&2
    exit 2
    ;;
esac
exit 0
```

### Ask before touching production files

```bash
#!/bin/sh
# PreToolUse, matcher "Edit|Write"
path=$(jq -r '.tool_input.file_path // ""')
case "$path" in
  */prod/*) echo '{"decision":"ask","reason":"This edits a production file"}' ;;
esac
```

### Format files after edits

```bash
#!/bin/sh
# PostToolUse, matcher "Edit|Write"
path=$(jq -r '.tool_input.file_path // ""')
case "$path" in
  *.rs) rustfmt --edition 2021 "$path" 2>/dev/null && echo '{"additionalContext":"rustfmt applied"}' ;;
esac
exit 0
```

### Keep working until the tests pass

```bash
#!/bin/sh
# Stop
input=$(cat)
# Do not block a stop that a previous block already caused.
[ "$(printf '%s' "$input" | jq -r .stop_hook_active)" = "true" ] && exit 0
if ! cargo test --quiet >/dev/null 2>&1; then
  echo "Tests are failing; fix them before finishing." >&2
  exit 2
fi
```

### Desktop notification on permission prompts

```json
{
  "hooks": {
    "Notification": [
      { "hooks": [{ "type": "command", "command": "jq -r .message | xargs -0 notify-send Claurst" }] }
    ]
  }
}
```

---

## Testing a hook

Hooks are plain programs, so test them by piping a payload in:

```bash
echo '{"session_id":"t","cwd":"'"$PWD"'","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"rm -rf /"}}' \
  | ~/.claurst/hooks/check-bash.sh; echo "exit=$?"
```

Hook failures, timeouts and skipped settings entries are logged as warnings (on stderr in `--print` mode, in the log file in the TUI). `--verbose` also logs every hook run.

---

## Differences from Claude Code

- Only `command` hooks exist. The `prompt`, `agent` and `http` hook types, and the `async`, `once`, `statusMessage`, `shell` and `if` fields, are not implemented.
- Implemented events: the ten above. Claude Code events such as `PermissionRequest`, `PostToolUseFailure`, `PostCompact`, `SubagentStart` and `Setup` are not; settings entries for them are skipped.
- No `transcript_path` in the input.
- `SessionStart` has no `clear` or `compact` source, and `SessionEnd` no `clear` or `logout` reason: `/clear` fires neither event.
- `UserPromptSubmit` does not fire for prompts sent over ACP or as `--input-format stream-json`.
- Project-level hooks are not read (Claude Code reads `.claude/settings.json` after a trust prompt).
- `PostModelTurn` is Claurst-specific.
