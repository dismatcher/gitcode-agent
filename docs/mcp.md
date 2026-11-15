# MCP

The native `McpCall` tool takes `server`, `tool`, and object `arguments`. It checks permission for every call before loading workspace `.mcp.json`, with a 30-second timeout. Configuration parsing and transport examples are covered by `tests/mcp.rs`.

A stdio server is initialized on its first permitted call and reused by the same workspace tool executor. A changed server configuration replaces the connection. A failed or interrupted call discards it; the next call starts a new server, and GitCode never automatically retries a potentially side-effecting invocation. Servers must implement the MCP initialize handshake and newline-delimited JSON-RPC. Separate subagents and separate GitCode processes own separate connections. HTTP calls do not yet reuse an MCP session. There is no cross-process connection pooling or event subscription.

Default-mode execution requires approval. Never configure an untrusted stdio executable: launching it executes local code. Server credentials belong in private local configuration, never repository history. Dynamic tool discovery and resources UI remain unsupported.
