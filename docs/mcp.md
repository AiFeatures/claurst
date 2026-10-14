# Model Context Protocol (MCP)

The Model Context Protocol is a JSON-RPC 2.0 based protocol for connecting Claurst to external tool, resource, and prompt servers. MCP servers extend what the agent can do without modifying Claurst itself — they can expose file systems, databases, APIs, browser automation, and anything else that can be wrapped in a tool.

---

## What MCP Is

MCP defines three primitives a server can offer:

- **Tools** — callable functions the model can invoke (analogous to built-in tools like `Bash` or `Read`)
- **Resources** — URI-addressable data sources the model can read
- **Prompts** — reusable prompt templates the server exposes

Claurst discovers tools, resources, and prompts from connected MCP servers during the handshake phase and wraps them as native `Tool` instances (via `McpToolWrapper`), making them transparent to the query loop.

---

## Transports

MCP servers communicate over one of two transports:

### stdio (subprocess)

The default transport. Claurst spawns the server as a child process and communicates over its stdin/stdout using newline-delimited JSON-RPC 2.0.

```json
{
  "name": "filesystem",
  "command": "npx",
  "args": ["-y", "@modelcontextprotocol/server-filesystem", "/home/user/projects"],
  "type": "stdio"
}
```

### HTTP / SSE

For servers running as standalone HTTP services. The `url` field is required; `command` and `args` are omitted.

```json
{
  "name": "remote-tools",
  "url": "https://mcp.example.com/sse",
  "type": "http"
}
```

The `type` field defaults to `"stdio"` when omitted.

---

## McpServerConfig Fields

Each entry in the `mcpServers` / `mcp_servers` array (or the `config.mcp_servers` list) is an `McpServerConfig` object:

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | Yes | Unique identifier for the server in this session |
| `command` | string | Stdio only | Executable to launch (e.g. `"npx"`, `"uvx"`, `"python"`) |
| `args` | array of strings | No | Arguments passed to `command` |
| `env` | object | No | Extra environment variables set for the child process |
| `url` | string | HTTP only | Full URL of the SSE endpoint |
| `type` | string | No | Transport type: `"stdio"` (default) or `"http"` |
| `connect_timeout_ms` | integer | No | Bound on starting the transport and the `initialize` handshake. Default `10000` |
| `catalog_timeout_ms` | integer | No | Bound on each `tools/list`, `resources/list` and `prompts/list` request. Default `30000` |

---

## Environment Variable Expansion

All string fields in `McpServerConfig` (`command`, `args`, `env` values, `url`) support shell-style variable expansion before the server is launched.

**Supported syntax:**

| Pattern | Behaviour |
|---|---|
| `${VAR_NAME}` | Substituted with the value of `VAR_NAME` from the process environment. If the variable is not set, the placeholder is left unchanged. |
| `${VAR_NAME:-default}` | Substituted with `VAR_NAME` if set; falls back to `default` if not set. |

**Example:**

```json
{
  "name": "my-server",
  "command": "npx",
  "args": ["-y", "my-mcp-server", "--token", "${MY_API_TOKEN:-demo}"],
  "env": {
    "DATA_DIR": "${HOME:-/tmp}/my-server-data"
  }
}
```

If `MY_API_TOKEN` is not set, `"demo"` is passed as the token. If `HOME` is set, `DATA_DIR` resolves to e.g. `/home/user/my-server-data`.

---

## Configuring MCP Servers in settings.json

Add servers to the `config.mcp_servers` array in `~/.claurst/settings.json`:

```json
{
  "config": {
    "mcp_servers": [
      {
        "name": "filesystem",
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-filesystem", "${HOME}/projects"],
        "type": "stdio"
      },
      {
        "name": "github",
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-github"],
        "env": {
          "GITHUB_PERSONAL_ACCESS_TOKEN": "${GITHUB_TOKEN}"
        },
        "type": "stdio"
      },
      {
        "name": "remote-api",
        "url": "https://mcp.example.com/sse",
        "type": "http"
      }
    ]
  }
}
```

Project-level MCP servers can be added to a `.claurst/settings.json` in your project root. Project settings take precedence over global settings.

---

## The /mcp Command

Use `/mcp` inside an interactive session to inspect and manage MCP servers at runtime.

```
/mcp                     — show status of all configured servers
/mcp status              — same as above
/mcp connect <name>      — connect to a server by name
/mcp disconnect <name>   — disconnect a server
/mcp restart <name>      — disconnect then reconnect a server
```

The status display shows the connection state and discovered tool count for each server:

```
filesystem     connected (12 tools)
github         connected (8 tools)
remote-api     failed – connection refused (retry in 30s)
```

---

## MCP Tools Available to the Model

Two built-in tools let the model interact with MCP resources directly:

### ListMcpResources

Lists all resources available across connected MCP servers. Optionally filters by server name.

**Input schema:**

```json
{
  "server_name": "filesystem"   // optional — omit to list resources from all servers
}
```

### ReadMcpResource

Reads a specific resource by server name and URI.

**Input schema:**

```json
{
  "server_name": "filesystem",
  "uri": "file:///home/user/projects/README.md"
}
```

Use `ListMcpResources` to discover available URIs before calling `ReadMcpResource`.

In addition to these, every tool that an MCP server exposes is automatically available to the model under its declared name (wrapped transparently by `McpToolWrapper`).

---

## Startup, Timeouts and Retries

Claurst connects to all configured servers concurrently in the background. The TUI starts immediately; the footer shows `MCP connecting (N)` while servers start and `MCP N failed` if any could not start, and the `/mcp` panel shows each server's state and error. A server's tools become available to the model on the first turn after it connects. Headless runs (`--print`) wait for every server to connect or fail before sending the prompt.

Each server's startup is bounded:

- **Connect timeout** (`connect_timeout_ms`, default 10 s): spawning the process or opening the HTTP/SSE stream plus the `initialize` handshake. A hung stdio server is killed when it expires.
- **Catalog timeout** (`catalog_timeout_ms`, default 30 s): each `tools/list`, `resources/list` and `prompts/list` request. A failing `tools/list` fails the server; a failing `resources/list` or `prompts/list` is logged and the server stays usable.

Remote (`http`/`sse`) servers get up to three connect attempts, 0.5 s and 1 s apart, unless an attempt timed out. Stdio servers get one attempt. After that the server stays failed until the MCP runtime is rebuilt (for example after approving a project server or completing `/mcp auth`) or Claurst restarts.

Server statuses:

| Status | Meaning |
|---|---|
| `Connected` | Active connection; reports tool count |
| `Connecting` | Connection attempt in progress |
| `Failed` | A remote attempt failed; another attempt is scheduled |
| `Disconnected` | Startup failed for good (the error is shown), or not connected |

---

## Popular MCP Servers

### Official Anthropic / MCP reference servers

```json
{
  "name": "filesystem",
  "command": "npx",
  "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allow"],
  "type": "stdio"
}
```

```json
{
  "name": "github",
  "command": "npx",
  "args": ["-y", "@modelcontextprotocol/server-github"],
  "env": {
    "GITHUB_PERSONAL_ACCESS_TOKEN": "${GITHUB_TOKEN}"
  },
  "type": "stdio"
}
```

```json
{
  "name": "postgres",
  "command": "npx",
  "args": ["-y", "@modelcontextprotocol/server-postgres", "${DATABASE_URL}"],
  "type": "stdio"
}
```

```json
{
  "name": "brave-search",
  "command": "npx",
  "args": ["-y", "@modelcontextprotocol/server-brave-search"],
  "env": {
    "BRAVE_API_KEY": "${BRAVE_API_KEY}"
  },
  "type": "stdio"
}
```

### Python-based servers (via uvx)

```json
{
  "name": "git",
  "command": "uvx",
  "args": ["mcp-server-git", "--repository", "${PWD}"],
  "type": "stdio"
}
```

```json
{
  "name": "sqlite",
  "command": "uvx",
  "args": ["mcp-server-sqlite", "--db-path", "${HOME}/data.db"],
  "type": "stdio"
}
```

### Local HTTP server

```json
{
  "name": "my-local-mcp",
  "url": "http://localhost:3001/sse",
  "type": "http"
}
```

---

## Complete settings.json Example

```json
{
  "config": {
    "mcp_servers": [
      {
        "name": "filesystem",
        "command": "npx",
        "args": [
          "-y",
          "@modelcontextprotocol/server-filesystem",
          "${HOME}/projects",
          "${HOME}/documents"
        ],
        "type": "stdio"
      },
      {
        "name": "github",
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-github"],
        "env": {
          "GITHUB_PERSONAL_ACCESS_TOKEN": "${GITHUB_TOKEN}"
        },
        "type": "stdio"
      }
    ]
  }
}
```
