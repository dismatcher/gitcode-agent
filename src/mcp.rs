//! Model Context Protocol (MCP) client supporting stdio and streamable HTTP JSON-RPC 2.0.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::Path,
    process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// Maximum output bytes returned from an MCP tool or resource call before truncation.
pub const MAX_MCP_OUTPUT_BYTES: usize = 100_000;

/// Maximum number of descriptors, contents, or messages parsed from a single MCP list response.
pub const MAX_MCP_LIST_ITEMS: usize = 256;

/// Transport configuration for a single MCP server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum McpServerConfig {
    /// Child process speaking newline-delimited JSON-RPC 2.0 over stdin/stdout.
    Stdio {
        /// Executable name or path.
        command: String,
        /// Command-line arguments.
        #[serde(default)]
        args: Vec<String>,
        /// Environment variables passed to the server process.
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    /// Streamable HTTP or SSE JSON-RPC 2.0 endpoint.
    Http {
        /// HTTP or HTTPS endpoint URL.
        url: String,
        /// Optional HTTP request headers.
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

/// Persisted `.mcp.json` configuration file structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpConfigFile {
    /// Configured MCP servers keyed by server identifier.
    #[serde(default, rename = "mcpServers")]
    pub servers: BTreeMap<String, McpServerConfig>,
}

/// Discovered MCP tool descriptor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpToolInfo {
    /// Unqualified server tool name.
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// JSON Schema for tool input arguments.
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
}

/// Connection status entry for `/mcp` UI rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerStatus {
    /// Server name.
    pub name: String,
    /// Transport summary (`stdio` or `http`).
    pub transport: String,
    /// Whether the server responded to initialization.
    pub connected: bool,
    /// Number of exposed tools.
    pub tool_count: usize,
}

/// Discovered MCP resource descriptor returned by `resources/list`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpResourceInfo {
    /// Resource URI accepted by [`StdioMcpClient::read_resource`] and [`HttpMcpClient::read_resource`].
    pub uri: String,
    /// Human-readable resource name.
    #[serde(default)]
    pub name: String,
    /// Human-readable resource description.
    #[serde(default)]
    pub description: String,
    /// Declared MIME type of the resource contents, if the server reports one.
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
}

/// Argument accepted by an MCP prompt template.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpPromptArgument {
    /// Argument name.
    pub name: String,
    /// Human-readable argument description.
    #[serde(default)]
    pub description: String,
    /// Whether the caller must supply the argument.
    #[serde(default)]
    pub required: bool,
}

/// Discovered MCP prompt descriptor returned by `prompts/list`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpPromptInfo {
    /// Prompt name accepted by [`StdioMcpClient::get_prompt`] and [`HttpMcpClient::get_prompt`].
    pub name: String,
    /// Human-readable prompt description.
    #[serde(default)]
    pub description: String,
    /// Arguments the server accepts for this prompt.
    #[serde(default)]
    pub arguments: Vec<McpPromptArgument>,
}

/// One rendered prompt message returned by `prompts/get`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpPromptMessage {
    /// Message role reported by the server, for example `user` or `assistant`.
    #[serde(default)]
    pub role: String,
    /// Bounded plain-text rendering of the message content.
    #[serde(default)]
    pub text: String,
}

fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && !s.contains("__")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Format a qualified MCP tool name as `mcp__<server>__<tool>`.
pub fn qualify_tool_name(server: &str, tool: &str) -> Result<String, String> {
    if !valid_ident(server) || !valid_ident(tool) {
        return Err("invalid MCP server or tool identifier".into());
    }
    Ok(format!("mcp__{server}__{tool}"))
}

/// Parse a qualified `mcp__<server>__<tool>` name into `(server, tool)`.
pub fn parse_qualified_tool(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if valid_ident(server) && valid_ident(tool) {
        Some((server, tool))
    } else {
        None
    }
}

/// Load MCP configuration from a JSON file, returning an empty map if absent.
pub fn load_mcp_config(path: &Path) -> io::Result<McpConfigFile> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "MCP config must not be a symlink",
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(McpConfigFile::default()),
        Err(e) => return Err(e),
    }
    let text = fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Save MCP configuration atomically with private permissions on Unix.
pub fn save_mcp_config(path: &Path, config: &McpConfigFile) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let encoded = serde_json::to_string_pretty(config)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(encoded.as_bytes())?;
    file.sync_all()
}

/// Add or replace an MCP server entry in a configuration file.
pub fn add_mcp_server(path: &Path, name: &str, server: McpServerConfig) -> Result<(), String> {
    if !valid_ident(name) {
        return Err("invalid MCP server identifier".into());
    }
    let mut cfg = load_mcp_config(path).map_err(|e| e.to_string())?;
    cfg.servers.insert(name.to_owned(), server);
    save_mcp_config(path, &cfg).map_err(|e| e.to_string())
}

/// Remove an MCP server entry from a configuration file.
pub fn remove_mcp_server(path: &Path, name: &str) -> Result<bool, String> {
    let mut cfg = load_mcp_config(path).map_err(|e| e.to_string())?;
    let existed = cfg.servers.remove(name).is_some();
    save_mcp_config(path, &cfg).map_err(|e| e.to_string())?;
    Ok(existed)
}

/// List configured MCP servers in deterministic key order.
pub fn list_mcp_servers(path: &Path) -> Result<Vec<(String, McpServerConfig)>, String> {
    let cfg = load_mcp_config(path).map_err(|e| e.to_string())?;
    Ok(cfg.servers.into_iter().collect())
}

fn bound_output(mut text: String) -> String {
    if text.len() > MAX_MCP_OUTPUT_BYTES {
        let mut end = MAX_MCP_OUTPUT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[mcp output truncated]");
    }
    text
}

/// Deserialize the `items_field` array of a `*/list` result into bounded descriptors.
fn parse_list<T>(result: &Value, items_field: &str, what: &str) -> Result<Vec<T>, String>
where
    T: for<'de> Deserialize<'de>,
{
    let items = result[items_field]
        .as_array()
        .ok_or_else(|| format!("invalid MCP {what} list"))?;
    let bounded = Value::Array(items.iter().take(MAX_MCP_LIST_ITEMS).cloned().collect());
    serde_json::from_value(bounded).map_err(|_| format!("invalid MCP {what} list"))
}

/// Parse the `contents` array of a `resources/read` result into bounded text.
fn parse_resource_contents(result: &Value) -> Result<String, String> {
    let contents = result["contents"]
        .as_array()
        .ok_or_else(|| "invalid MCP resource contents".to_owned())?;
    if contents.is_empty() {
        return Err("MCP resource has no contents".into());
    }
    let text = contents
        .iter()
        .take(MAX_MCP_LIST_ITEMS)
        .map(|item| match item["text"].as_str() {
            Some(text) => text.to_owned(),
            None => match item["blob"].as_str() {
                Some(_) => "[binary resource content omitted]".to_owned(),
                None => format!(
                    "[resource without text or blob: {}]",
                    bound_output(item.to_string())
                ),
            },
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(bound_output(text))
}

/// Parse the `messages` array of a `prompts/get` result into bounded text messages.
fn parse_prompt_messages(result: &Value) -> Result<Vec<McpPromptMessage>, String> {
    let messages = result["messages"]
        .as_array()
        .ok_or_else(|| "invalid MCP prompt messages".to_owned())?;
    let mut parsed = Vec::with_capacity(messages.len().min(MAX_MCP_LIST_ITEMS));
    for message in messages.iter().take(MAX_MCP_LIST_ITEMS) {
        let text = match message["content"].as_array() {
            Some(items) => items
                .iter()
                .filter_map(|item| item["text"].as_str())
                .map(|text| bound_output(text.to_owned()))
                .collect::<Vec<_>>()
                .join("\n"),
            None => match message["content"]["text"].as_str() {
                Some(text) => bound_output(text.to_owned()),
                None => continue,
            },
        };
        parsed.push(McpPromptMessage {
            role: message["role"].as_str().unwrap_or_default().to_owned(),
            text,
        });
    }
    Ok(parsed)
}

/// Stdio JSON-RPC 2.0 MCP client.
pub struct StdioMcpClient {
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    timeout: Duration,
}

impl StdioMcpClient {
    /// Construct a stdio MCP client with an explicit per-request timeout.
    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Self {
        Self {
            command: command.into(),
            args,
            env,
            timeout,
        }
    }

    /// Perform an `initialize` + request roundtrip against the stdio MCP process.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let work = async {
            let mut cmd = crate::platform::mcp_command(&self.command, &self.args)?;
            cmd.envs(&self.env)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            let mut child = cmd.spawn().map_err(|_| "cannot spawn stdio MCP server")?;
            let mut stdin = child.stdin.take().ok_or("missing MCP stdin")?;
            let stdout = child.stdout.take().ok_or("missing MCP stdout")?;
            let mut reader = BufReader::new(stdout).lines();

            let init_req = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "gitcode", "version": "0.1.0" }
                }
            });
            let notif = json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            });
            let call_req = json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": method,
                "params": params
            });
            let payload = format!("{init_req}\n{notif}\n{call_req}\n");
            stdin
                .write_all(payload.as_bytes())
                .await
                .map_err(|_| "cannot write to MCP server")?;
            stdin.flush().await.map_err(|_| "cannot flush MCP stdin")?;

            while let Some(line) = reader
                .next_line()
                .await
                .map_err(|_| "cannot read MCP stdout")?
            {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let parsed: Value =
                    serde_json::from_str(trimmed).map_err(|_| "invalid MCP JSON-RPC line")?;
                if parsed["id"].as_i64() == Some(2) {
                    if let Some(err) = parsed.get("error") {
                        return Err(format!("MCP error: {err}"));
                    }
                    return Ok(parsed["result"].clone());
                }
            }
            Err("MCP server closed stream before responding".into())
        };
        tokio::time::timeout(self.timeout, work)
            .await
            .map_err(|_| "MCP stdio request timed out".to_owned())?
    }

    /// List tools exposed by the stdio MCP server.
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, String> {
        let result = self.request("tools/list", json!({})).await?;
        serde_json::from_value(result["tools"].clone()).map_err(|_| "invalid MCP tools list".into())
    }

    /// List resources exposed by the stdio MCP server.
    pub async fn list_resources(&self) -> Result<Vec<McpResourceInfo>, String> {
        let result = self.request("resources/list", json!({})).await?;
        parse_list(&result, "resources", "resources")
    }

    /// Read a resource by URI from the stdio MCP server and return bounded text.
    pub async fn read_resource(&self, uri: &str) -> Result<String, String> {
        let result = self
            .request("resources/read", json!({ "uri": uri }))
            .await?;
        parse_resource_contents(&result)
    }

    /// List prompt templates exposed by the stdio MCP server.
    pub async fn list_prompts(&self) -> Result<Vec<McpPromptInfo>, String> {
        let result = self.request("prompts/list", json!({})).await?;
        parse_list(&result, "prompts", "prompts")
    }

    /// Render a prompt template on the stdio MCP server into bounded messages.
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<Vec<McpPromptMessage>, String> {
        let arguments = match arguments {
            Value::Null => json!({}),
            other => other,
        };
        if !arguments.is_object() {
            return Err("MCP prompt arguments must be a JSON object".into());
        }
        let result = self
            .request(
                "prompts/get",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        parse_prompt_messages(&result)
    }

    /// Invoke a tool on the stdio MCP server and return bounded text.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, String> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        let text = result["content"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|| result.to_string());
        Ok(bound_output(text))
    }
}

/// Reusable stdio connection owned by one workspace executor.
pub(crate) struct StdioMcpSession {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    next_id: u64,
    timeout: Duration,
}

impl StdioMcpSession {
    pub(crate) async fn connect(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<Self, String> {
        let mut cmd = crate::platform::mcp_command(command, args)?;
        cmd.envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|_| "cannot spawn stdio MCP server")?;
        let stdin = child.stdin.take().ok_or("missing MCP stdin")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("missing MCP stdout")?);
        let mut session = Self {
            child,
            stdin,
            stdout,
            next_id: 2,
            timeout,
        };
        tokio::time::timeout(timeout, async {
            session
                .send(
                    &json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params": {
                        "protocolVersion":"2024-11-05", "capabilities":{},
                        "clientInfo":{"name":"gitcode", "version":env!("CARGO_PKG_VERSION")}
                    }}),
                )
                .await?;
            let initialized = session.receive(1).await?;
            if initialized["protocolVersion"].as_str() != Some("2024-11-05") {
                return Err("unsupported MCP protocol version".into());
            }
            session
                .send(&json!({"jsonrpc":"2.0", "method":"notifications/initialized"}))
                .await
        })
        .await
        .map_err(|_| "MCP stdio initialization timed out".to_owned())??;
        Ok(session)
    }

    async fn send(&mut self, value: &Value) -> Result<(), String> {
        let mut data = serde_json::to_vec(value).map_err(|_| "cannot encode MCP request")?;
        data.push(b'\n');
        self.stdin
            .write_all(&data)
            .await
            .map_err(|_| "cannot write to MCP server")?;
        self.stdin
            .flush()
            .await
            .map_err(|_| "cannot flush MCP stdin".into())
    }

    async fn receive(&mut self, id: u64) -> Result<Value, String> {
        const MAX_LINE: u64 = 2 * 1024 * 1024;
        for _ in 0..1024 {
            let mut bytes = Vec::new();
            let size = (&mut self.stdout)
                .take(MAX_LINE + 1)
                .read_until(b'\n', &mut bytes)
                .await
                .map_err(|_| "cannot read MCP stdout")?;
            if size == 0 {
                return Err("MCP server closed stream before responding".into());
            }
            if size as u64 > MAX_LINE || !bytes.ends_with(b"\n") {
                return Err("MCP response line too large or incomplete".into());
            }
            if bytes.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let response: Value =
                serde_json::from_slice(&bytes).map_err(|_| "invalid MCP JSON-RPC line")?;
            if response["jsonrpc"].as_str() != Some("2.0") {
                return Err("invalid MCP JSON-RPC version".into());
            }
            if response["id"].as_u64() == Some(id) {
                if response.get("error").is_some() {
                    return Err("MCP server returned an error".into());
                }
                return response
                    .get("result")
                    .cloned()
                    .ok_or_else(|| "MCP response has no result".into());
            }
        }
        Err("too many unrelated MCP messages".into())
    }

    pub(crate) async fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> Result<String, String> {
        let result = self
            .roundtrip("tools/call", json!({"name": name, "arguments": arguments}))
            .await?;
        if result["isError"].as_bool() == Some(true) {
            return Err("MCP tool returned an error".into());
        }
        let text = result["content"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|| result.to_string());
        Ok(bound_output(text))
    }

    async fn roundtrip(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("MCP request ID exhausted")?;
        let timeout = self.timeout;
        tokio::time::timeout(timeout, async {
            self.send(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
                .await?;
            self.receive(id).await
        })
        .await
        .map_err(|_| "MCP stdio request timed out".to_owned())?
    }

    pub(crate) async fn close(mut self) {
        let _ = self.child.kill().await;
    }
}

/// Streamable HTTP and SSE JSON-RPC 2.0 MCP client.
pub struct HttpMcpClient {
    url: String,
    headers: BTreeMap<String, String>,
    client: reqwest::Client,
}

impl HttpMcpClient {
    /// Construct an HTTP MCP client with a bounded request timeout.
    pub fn new(
        url: impl Into<String>,
        headers: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<Self, String> {
        let url = url.into();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("MCP HTTP URL must start with http:// or https://".into());
        }
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "cannot build MCP HTTP client")?;
        Ok(Self {
            url,
            headers,
            client,
        })
    }

    /// Send a JSON-RPC 2.0 request over HTTP, accepting either JSON or `text/event-stream`.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params
        });
        let mut req = self
            .client
            .post(&self.url)
            .header("Accept", "application/json, text/event-stream")
            .json(&body);
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|_| "MCP HTTP request failed")?;
        if !resp.status().is_success() {
            return Err(format!("MCP HTTP status {}", resp.status().as_u16()));
        }
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let text = resp
            .text()
            .await
            .map_err(|_| "cannot read MCP HTTP response")?;
        let payload = if content_type.contains("text/event-stream") {
            text.lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim)
                .find(|data| !data.is_empty() && *data != "[DONE]")
                .ok_or("empty MCP SSE response")?
        } else {
            text.trim()
        };
        let parsed: Value =
            serde_json::from_str(payload).map_err(|_| "invalid MCP HTTP JSON-RPC response")?;
        if let Some(err) = parsed.get("error") {
            return Err(format!("MCP error: {err}"));
        }
        Ok(parsed["result"].clone())
    }

    /// Invoke a tool over HTTP/SSE MCP and return bounded text.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, String> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        let text = result["content"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|| result.to_string());
        Ok(bound_output(text))
    }

    /// List resources exposed by the HTTP MCP server.
    pub async fn list_resources(&self) -> Result<Vec<McpResourceInfo>, String> {
        let result = self.request("resources/list", json!({})).await?;
        parse_list(&result, "resources", "resources")
    }

    /// Read a resource by URI over HTTP/SSE MCP and return bounded text.
    pub async fn read_resource(&self, uri: &str) -> Result<String, String> {
        let result = self
            .request("resources/read", json!({ "uri": uri }))
            .await?;
        parse_resource_contents(&result)
    }

    /// List prompt templates exposed by the HTTP MCP server.
    pub async fn list_prompts(&self) -> Result<Vec<McpPromptInfo>, String> {
        let result = self.request("prompts/list", json!({})).await?;
        parse_list(&result, "prompts", "prompts")
    }

    /// Render a prompt template over HTTP/SSE MCP into bounded messages.
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<Vec<McpPromptMessage>, String> {
        let arguments = match arguments {
            Value::Null => json!({}),
            other => other,
        };
        if !arguments.is_object() {
            return Err("MCP prompt arguments must be a JSON object".into());
        }
        let result = self
            .request(
                "prompts/get",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        parse_prompt_messages(&result)
    }
}

/// Render `/mcp` server status lines for the terminal UI.
pub fn render_mcp_status(servers: &[McpServerStatus]) -> Vec<String> {
    if servers.is_empty() {
        return vec!["No MCP servers configured.".into()];
    }
    servers
        .iter()
        .map(|s| {
            let state = if s.connected {
                "connected"
            } else {
                "disconnected"
            };
            format!(
                "• {} [{}] — {} ({} tools)",
                s.name, s.transport, state, s.tool_count
            )
        })
        .collect()
}
