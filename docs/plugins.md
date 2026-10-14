# Plugins

Claurst's plugin system lets you extend the agent with additional slash commands, agents, skills, MCP servers, LSP servers, and lifecycle hooks — all packaged in a single directory.

---

## Plugin Discovery

Plugins are loaded from the `~/.claurst/plugins/` directory. Each subdirectory that contains a valid `plugin.toml` or `plugin.json` manifest is treated as a plugin.

```
~/.claurst/plugins/
├── my-plugin/
│   ├── plugin.toml          <- manifest
│   ├── commands/            <- *.md slash command definitions
│   ├── agents/              <- *.md agent definitions
│   ├── skills/              <- subdirectories with SKILL.md
│   ├── hooks/               <- hooks.json (optional)
│   └── output-styles/       <- *.md or *.json style definitions
└── another-plugin/
    └── plugin.json
```

Both `plugin.toml` (TOML format) and `plugin.json` (JSON format) are supported. The loader normalises camelCase and snake_case field names, so manifests written in either convention are accepted.

---

## Plugin Manifest Format

### plugin.toml

```toml
name        = "my-plugin"
version     = "1.0.0"
description = "Adds custom commands and hooks for my workflow"
license     = "MIT"
keywords    = ["formatting", "git"]

[author]
name  = "Your Name"
email = "you@example.com"
url   = "https://example.com"

homepage   = "https://example.com/my-plugin"
repository = "https://github.com/you/my-plugin"

# Extra command files beyond the commands/ directory
commands = ["./extra/review.md"]

# Extra agent markdown files beyond the agents/ directory
agents = ["./agents/reviewer.md"]

# Extra skill directories beyond the skills/ directory
skills = ["./extra-skills/"]

# Inline MCP server definitions
[[mcp_servers]]
name    = "my-tool-server"
command = "npx"
args    = ["-y", "my-mcp-server"]
type    = "stdio"

[mcp_servers.env]
API_TOKEN = "${MY_SERVICE_TOKEN}"

# Inline LSP server definitions
[[lsp_servers]]
name    = "pyright"
command = "pyright-langserver"
args    = ["--stdio"]
transport = "stdio"
restart_on_crash = true

[lsp_servers.extension_to_language]
".py" = "python"

# User-configurable options (surfaced in /plugin info)
[user_config.api_token]
type        = "string"
title       = "API Token"
description = "Token for the upstream service"
required    = true
sensitive   = true

[user_config.max_results]
type        = "number"
title       = "Max Results"
description = "Maximum items to return per query"
default     = 20

# Capability grants (omit to allow all)
capabilities = ["read_files", "network", "shell"]

# Marketplace identifier
marketplace_id = "you/my-plugin"
```

### plugin.json

```json
{
  "name": "my-plugin",
  "version": "1.0.0",
  "description": "Adds custom commands and hooks for my workflow",
  "author": {
    "name": "Your Name",
    "email": "you@example.com"
  },
  "homepage": "https://example.com/my-plugin",
  "repository": "https://github.com/you/my-plugin",
  "license": "MIT",
  "keywords": ["formatting", "git"],
  "commands": ["./extra/review.md"],
  "agents": ["./agents/reviewer.md"],
  "skills": ["./extra-skills/"],
  "mcpServers": {
    "my-tool-server": {
      "command": "npx",
      "args": ["-y", "my-mcp-server"],
      "type": "stdio",
      "env": {
        "API_TOKEN": "${MY_SERVICE_TOKEN}"
      }
    }
  },
  "userConfig": {
    "api_token": {
      "type": "string",
      "title": "API Token",
      "description": "Token for the upstream service",
      "required": true,
      "sensitive": true
    },
    "max_results": {
      "type": "number",
      "title": "Max Results",
      "description": "Maximum items to return per query",
      "default": 20
    }
  },
  "capabilities": ["read_files", "network", "shell"],
  "marketplaceId": "you/my-plugin"
}
```

---

## Manifest Fields Reference

### Required

| Field | Type | Description |
|---|---|---|
| `name` | string | Plugin name. Must be non-empty and contain no spaces (use kebab-case). |

### Metadata (optional)

| Field | Type | Description |
|---|---|---|
| `version` | string | Plugin version string |
| `description` | string | Human-readable description |
| `author` | object | `name`, `email` (optional), `url` (optional) |
| `homepage` | string | URL for the plugin's home page |
| `repository` | string | URL for the source repository |
| `license` | string | SPDX license identifier (e.g. `"MIT"`) |
| `keywords` | array of strings | Tags used in marketplace search |
| `marketplace_id` | string | Unique identifier in the plugin marketplace (e.g. `"author/plugin-name"`) |

### Content Declarations

| Field | Type | Description |
|---|---|---|
| `commands` | array of strings | Paths to extra slash command `.md` files or directories, relative to the plugin root. Supplements the `commands/` directory. |
| `agents` | array of strings | Paths to extra agent `.md` files. Supplements the `agents/` directory. |
| `skills` | array of strings | Paths to extra skill directories (each must contain a `SKILL.md`). Supplements the `skills/` directory. |
| `output_styles` | array of strings | Paths to extra output style definitions. |

### mcp_servers

An array of inline MCP server definitions. Each entry is identical to an `McpServerConfig` (see the MCP documentation). In `plugin.json` the field can also be written as `"mcpServers"` with an object mapping (the loader converts it to the array form automatically).

### lsp_servers

An array of LSP server definitions for language-aware editing support:

| Field | Type | Description |
|---|---|---|
| `name` | string | Server identifier |
| `command` | string | Executable to launch |
| `args` | array | Command-line arguments |
| `extension_to_language` | object | Map of file extension → LSP language ID |
| `transport` | string | `"stdio"` (default) |
| `env` | object | Extra environment variables |
| `workspace_folder` | string | Optional workspace path |
| `startup_timeout` | number | Milliseconds to wait for server readiness |
| `shutdown_timeout` | number | Milliseconds to wait for clean shutdown |
| `restart_on_crash` | bool | Automatically restart on unexpected exit |
| `max_restarts` | number | Maximum restart attempts |

### hooks

An inline hooks configuration object (see [Hooks](#hooks) below). A `hooks/hooks.json` file in the plugin directory takes precedence over it.

### user_config

A map of option keys to `PluginUserConfigOption` objects, allowing the plugin to declare user-configurable settings that are surfaced by `/plugin info`:

| Field | Type | Description |
|---|---|---|
| `type` | enum | Value type: `"string"`, `"number"`, `"boolean"`, `"directory"`, `"file"` |
| `title` | string | Display label |
| `description` | string | Explanation of the option |
| `required` | bool | Whether the user must supply a value |
| `default` | any | Default value (optional) |
| `sensitive` | bool | When `true`, the value is masked in UI output |

### capabilities

An optional array of capability category strings. When present, the plugin is restricted to only those categories. Omit the field entirely to allow all capabilities (backwards compatibility behaviour). An empty array (`[]`) grants no capabilities.

Known categories: `"read_files"`, `"write_files"`, `"network"`, `"shell"`, `"browser"`, `"mcp"`.

---

## Hooks

Plugins can ship hooks: shell commands that run on session events. Plugin hooks use exactly the same format, events, input and output as settings hooks, and run through the same engine, after the user's settings hooks. See [Hooks](hooks.md) for the events (`PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `Stop`, `SubagentStop`, `SessionStart`, `SessionEnd`, `Notification`, `PreCompact`, `PostModelTurn`), the JSON on stdin, exit code 2 and the stdout JSON fields.

Hooks are read from `hooks/hooks.json` in the plugin directory or, if there is none, from the manifest's inline `hooks` object. Both the plain events map and a wrapped form with a description are accepted:

```json
{
  "description": "Audit and notification hooks",
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [
          { "type": "command", "command": "\"$CLAUDE_PLUGIN_ROOT\"/scripts/lint.sh", "timeout": 30 }
        ]
      }
    ],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "notify-send 'Claurst finished'" }] }
    ]
  }
}
```

Entries for events Claurst does not implement are skipped with a warning.

**Trust.** Hooks from plugins in `~/.claurst/plugins/` run like your own hooks. Hooks from plugins found in a project's `.claurst/plugins/` are ignored (a warning is logged and `/plugin info` marks them as not run), for the same reason project settings cannot define hooks: they come from the repository, not from you.

**Environment variables available to plugin hook processes:**

| Variable | Value |
|---|---|
| `CLAUDE_PLUGIN_ROOT` | Absolute path to the plugin directory |
| `CLAUDE_PLUGIN_NAME` | Plugin name from the manifest |
| `CLAUDE_PROJECT_DIR` | The session's working directory |

Plugin hooks are registered when Claurst starts; restart to pick up changes (`/plugin reload` and `/reload-plugins` do not re-register hooks). `/hooks` lists them with the plugin's name.

---

## Managing Plugins with /plugin

The `/plugin` slash command manages plugins from within an interactive session:

```
/plugin                      — list all installed plugins
/plugin list                 — list all installed plugins with status
/plugin info <name>          — show detailed info about a plugin
/plugin enable <name>        — enable a plugin (persisted to settings)
/plugin disable <name>       — disable a plugin (persisted to settings)
/plugin install <path>       — install a plugin from a local directory
/plugin reload               — reload all plugins from disk
```

After enabling or disabling a plugin, run `/plugin reload` or use `/reload-plugins` to apply changes in the current session without restarting.

### /reload-plugins

```
/reload-plugins
```

Rescans `~/.claurst/plugins/`, re-reads all manifests, and reports what changed. Plugin hooks are not re-registered; restart Claurst for hook changes. Use this after making changes to a plugin directory or after installing a new plugin.

---

## Plugin Marketplace Integration

Plugins published to the Claurst marketplace have a `marketplace_id` field in their manifest (e.g. `"author/plugin-name"`). The marketplace integration allows:

- Browsing available plugins
- Installing plugins by ID
- Updating installed plugins to newer versions

```
/plugin install author/plugin-name     — install from the marketplace
```

Locally installed plugins (via a file path) do not require a `marketplace_id`.

---

## Example: A Complete Plugin

```toml
# ~/.claurst/plugins/code-quality/plugin.toml

name        = "code-quality"
version     = "0.3.1"
description = "Runs linters and formatters after file edits"
license     = "MIT"
keywords    = ["lint", "format", "quality"]

[author]
name = "Dev Team"

capabilities = ["shell", "read_files"]

[user_config.fail_on_warning]
type        = "boolean"
title       = "Fail on Warnings"
description = "Treat linter warnings as errors"
default     = false
```

```json
// ~/.claurst/plugins/code-quality/hooks/hooks.json
{
  "description": "Lint and format on file edits",
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [
          {
            "type": "command",
            "command": "f=$(jq -r .tool_input.file_path); eslint --fix \"$f\" >/dev/null 2>&1; prettier --write \"$f\" >/dev/null 2>&1; exit 0"
          }
        ]
      }
    ]
  }
}
```
