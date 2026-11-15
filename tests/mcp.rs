//! Tests for Milestone M11 MCP stdio, streamable HTTP/SSE, tool qualification, and config.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use gitcode::mcp;

use mcp::{
    add_mcp_server, list_mcp_servers, parse_qualified_tool, qualify_tool_name, remove_mcp_server,
    render_mcp_status, HttpMcpClient, McpServerConfig, McpServerStatus, StdioMcpClient,
};
use serde_json::json;
use std::{collections::BTreeMap, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[test]
fn qualify_and_parse_mcp_tool_names_and_persist_config() {
    let q = qualify_tool_name("github_srv", "search-issues").unwrap();
    assert_eq!(q, "mcp__github_srv__search-issues");
    assert_eq!(
        parse_qualified_tool(&q),
        Some(("github_srv", "search-issues"))
    );
    assert!(qualify_tool_name("bad__name", "tool").is_err());
    assert_eq!(parse_qualified_tool("Read"), None);

    let path = std::env::temp_dir().join(format!("gitcode-mcp-{}.json", std::process::id()));
    add_mcp_server(
        &path,
        "local",
        McpServerConfig::Stdio {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), "true".into()],
            env: BTreeMap::new(),
        },
    )
    .unwrap();
    let list = list_mcp_servers(&path).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].0, "local");
    assert!(remove_mcp_server(&path, "local").unwrap());
    assert!(list_mcp_servers(&path).unwrap().is_empty());
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn stdio_and_http_sse_mcp_clients_execute_tools() {
    let script = concat!(
        "read line1; read line2; read line3; ",
        "printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2024-11-05\"}}\\n'; ",
        "printf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"stdio-ok\"}]}}\\n'"
    );
    #[cfg(windows)]
    let executable = gitcode::platform::detect_shell("bash").unwrap().executable;
    #[cfg(not(windows))]
    let executable = std::path::PathBuf::from("/bin/sh");
    let stdio = StdioMcpClient::new(
        executable.to_str().unwrap(),
        vec!["-c".into(), script.into()],
        BTreeMap::new(),
        Duration::from_secs(5),
    );
    let out = stdio.call_tool("echo", json!({"msg": "hi"})).await.unwrap();
    assert_eq!(out, "stdio-ok");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"sse-ok\"}]}}\n\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
    });

    let http = HttpMcpClient::new(
        format!("http://{addr}/mcp"),
        BTreeMap::new(),
        Duration::from_secs(5),
    )
    .unwrap();
    let sse_out = http.call_tool("ping", json!({})).await.unwrap();
    assert_eq!(sse_out, "sse-ok");

    let lines = render_mcp_status(&[McpServerStatus {
        name: "local".into(),
        transport: "stdio".into(),
        connected: true,
        tool_count: 2,
    }]);
    assert!(lines[0].contains("local [stdio] — connected (2 tools)"));
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_tools_reuse_stdio_session_and_recheck_permissions() {
    use gitcode::{
        perm::{Engine, Mode, Rule},
        tools::Tools,
    };
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let log = root.join("starts.log");
    let script = r#"
printf 'start\n' >> "$MCP_TEST_LOG"
IFS= read -r init || exit 1
printf '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05"}}\n'
IFS= read -r initialized || exit 1
n=2
while IFS= read -r request; do
  printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"call-%s"}]}}\n' "$n" "$n"
  n=$((n+1))
done
"#;
    let make_config = |variant: &str| McpServerConfig::Stdio {
        command: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        env: BTreeMap::from([
            ("MCP_TEST_LOG".into(), log.to_string_lossy().into_owned()),
            ("MCP_TEST_VARIANT".into(), variant.into()),
        ]),
    };
    let path = root.join(".mcp.json");
    add_mcp_server(&path, "local", make_config("one")).unwrap();
    let mut tools = Tools::new(Engine::new(&root, Mode::Bypass).unwrap());
    let input = json!({"server":"local","tool":"echo","arguments":{}});
    assert_eq!(tools.execute("McpCall", &input).await.unwrap(), "call-2");
    assert_eq!(tools.execute("McpCall", &input).await.unwrap(), "call-3");
    tools
        .permissions
        .deny
        .push(Rule::parse("McpCall(*)").unwrap());
    assert!(tools.execute("McpCall", &input).await.is_err());
    tools.permissions.deny.clear();
    assert_eq!(tools.execute("McpCall", &input).await.unwrap(), "call-4");
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 1);
    add_mcp_server(&path, "local", make_config("two")).unwrap();
    assert_eq!(tools.execute("McpCall", &input).await.unwrap(), "call-2");
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 2);
    tools.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn cancelled_stdio_request_does_not_reuse_an_unfinished_connection() {
    use gitcode::{
        perm::{Engine, Mode},
        tools::Tools,
    };
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let log = root.join("starts.log");
    let marker = root.join("blocked-once");
    let script = r#"
printf 'start\n' >> "$MCP_TEST_LOG"
IFS= read -r init || exit 1
printf '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05"}}\n'
IFS= read -r initialized || exit 1
while IFS= read -r request; do
  if [ ! -e "$MCP_TEST_MARKER" ]; then
    : > "$MCP_TEST_MARKER"
    sleep 10
  fi
  printf '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"fresh"}]}}\n'
done
"#;
    let server = McpServerConfig::Stdio {
        command: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        env: BTreeMap::from([
            ("MCP_TEST_LOG".into(), log.to_string_lossy().into_owned()),
            (
                "MCP_TEST_MARKER".into(),
                marker.to_string_lossy().into_owned(),
            ),
        ]),
    };
    add_mcp_server(&root.join(".mcp.json"), "local", server).unwrap();
    let mut tools = Tools::new(Engine::new(&root, Mode::Bypass).unwrap());
    let input = json!({"server":"local","tool":"echo","arguments":{}});
    assert!(
        tokio::time::timeout(Duration::from_millis(300), tools.execute("McpCall", &input))
            .await
            .is_err()
    );
    assert!(marker.exists());
    assert_eq!(tools.execute("McpCall", &input).await.unwrap(), "fresh");
    assert_eq!(std::fs::read_to_string(log).unwrap().lines().count(), 2);
    tools.shutdown().await.unwrap();
}
