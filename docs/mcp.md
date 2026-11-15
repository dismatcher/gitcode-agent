# MCP

The native `McpCall` tool takes `server`, `tool`, and object `arguments`. It checks permission for every call before loading workspace `.mcp.json`, with a 30-second timeout. Configuration parsing and transport examples are covered by `tests/mcp.rs`.

A stdio server is initialized on its first permitted call and reused by the same workspace tool executor. A changed server configuration replaces the connection. A failed or interrupted call discards it; the next call starts a new server, and GitCode never automatically retries a potentially side-effecting invocation. Servers must implement the MCP initialize handshake and newline-delimited JSON-RPC. Separate subagents and separate GitCode processes own separate connections. HTTP calls do not yet reuse an MCP session. There is no cross-process connection pooling or event subscription.

## Client API beyond tools

Both transports (`StdioMcpClient` and `HttpMcpClient`) expose the same four methods in addition to `list_tools`/`call_tool`:

| Method | JSON-RPC request | Returns |
| --- | --- | --- |
| `list_resources()` | `resources/list` | `Vec<McpResourceInfo>` (`uri`, `name`, `description`, `mimeType`) |
| `read_resource(uri)` | `resources/read` | bounded text of the resource contents |
| `list_prompts()` | `prompts/list` | `Vec<McpPromptInfo>` (`name`, `description`, `arguments`) |
| `get_prompt(name, arguments)` | `prompts/get` | `Vec<McpPromptMessage>` (`role`, `text`) |

Responses are treated as untrusted server input: text is truncated by the same `MAX_MCP_OUTPUT_BYTES` limit as tool output, lists are capped at `MAX_MCP_LIST_ITEMS`, binary resource contents (`blob`) are summarized rather than dumped, and any malformed, missing, or error-shaped payload produces `Err(String)` instead of a panic. Prompt arguments must be a JSON object; `null` is treated as `{}`.

## What is still missing

The MCP client library supports resources and prompts, but GitCode itself does not use them yet:

- there is no `McpCall`-style native tool, no model-visible prompt for resources or prompts, and no `/mcp` command to browse them;
- the TUI shows only server name, transport, connection state, and tool count — resources and prompts are not rendered, and tool discovery itself is still not dynamic beyond `/mcp` status output;
- resource and prompt calls are not covered by the permission engine, because nothing in the agent invokes them yet.

Until a caller exists, treat resources and prompts as a client-library capability only.

Default-mode execution requires approval. Never configure an untrusted stdio executable: launching it executes local code. Server credentials belong in private local configuration, never repository history.
