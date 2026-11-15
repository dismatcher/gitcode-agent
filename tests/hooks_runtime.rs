//! End-to-end lifecycle hooks with a local scripted provider.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use gitcode::config::{self, Overrides};
use std::collections::BTreeMap;

#[cfg(unix)]
use gitcode::{
    agent::{context::Compaction, Agent},
    extensibility::{HookEvent, HookRule},
    perm::{Engine, Mode},
    provider::{
        canonical::{Content, Message, Role},
        transport::Client,
    },
    tools::Tools,
};
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[test]
fn project_hook_commands_require_explicit_user_trust() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let project = dir.path().join("project");
    std::fs::create_dir_all(home.join(".gitcode")).unwrap();
    std::fs::create_dir_all(project.join(".gitcode")).unwrap();
    let hook = "[[hooks]]\nevent = 'PreToolUse'\nmatcher = 'Bash'\ncommand = 'exit 2'\n";
    std::fs::write(project.join(".gitcode/config.toml"), hook).unwrap();
    let error = config::load(&home, &project, &BTreeMap::new(), &Overrides::new()).unwrap_err();
    assert!(error
        .to_string()
        .contains("project hooks require explicit user trust"));
    std::fs::write(
        home.join(".gitcode/config.toml"),
        "trust_project_config = true\n",
    )
    .unwrap();
    let config = config::load(&home, &project, &BTreeMap::new(), &Overrides::new()).unwrap();
    assert_eq!(config.hooks.len(), 1);
    assert_eq!(config.hooks[0].timeout_ms, 5_000);
    std::fs::write(home.join(".gitcode/config.toml"), hook).unwrap();
    std::fs::write(project.join(".gitcode/config.toml"), "").unwrap();
    assert_eq!(
        config::load(&home, &project, &BTreeMap::new(), &Overrides::new())
            .unwrap()
            .hooks
            .len(),
        1
    );
}

#[cfg(unix)]
#[tokio::test]
async fn hooks_intercept_real_agent_calls_and_add_context() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::write(root.join("example.txt"), "verified contents").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for turn in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 8192];
            let body = loop {
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                    let len = String::from_utf8_lossy(&bytes[..end])
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|v| v.parse::<usize>().ok())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + len {
                        break serde_json::from_slice::<serde_json::Value>(
                            &bytes[end + 4..end + 4 + len],
                        )
                        .unwrap();
                    }
                }
            };
            let response = if turn == 0 {
                concat!("data: {\"type\":\"message_start\",\"message\":{}}\n\n", "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"read-1\",\"name\":\"Read\",\"input\":{\"path\":\"example.txt\"}}}\n\n", "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n", "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"bash-1\",\"name\":\"Bash\",\"input\":{\"command\":\"touch forbidden\"}}}\n\n", "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n", "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n", "data: {\"type\":\"message_stop\"}\n\n").to_owned()
            } else {
                let messages = body["messages"].to_string();
                assert!(messages.contains("verified contents"));
                assert!(messages.contains("read hook context"));
                assert!(messages.contains("blocked by fixture"));
                concat!("data: {\"type\":\"message_start\",\"message\":{}}\n\n", "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n", "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n", "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n", "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n", "data: {\"type\":\"message_stop\"}\n\n").to_owned()
            };
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
        }
    });
    let rule = |event, matcher: &str, command: &str| HookRule {
        event,
        matcher: Some(matcher.into()),
        command: command.into(),
        timeout_ms: 3000,
    };
    let mut tools = Tools::new(Engine::new(&root, Mode::Bypass).unwrap());
    tools.set_hooks(vec![
        rule(
            HookEvent::PreToolUse,
            "Read",
            "cat > pre.json; printf '{\"additionalContext\":\"read hook context\"}'",
        ),
        rule(HookEvent::PostToolUse, "Read", "cat > post.json"),
        rule(
            HookEvent::PreToolUse,
            "Bash",
            "cat > denied.json; echo 'blocked by fixture' >&2; exit 2",
        ),
    ]);
    let mut agent = Agent {
        client: Client::new(
            &format!("http://{address}"),
            None,
            false,
            Duration::from_secs(5),
        )
        .unwrap(),
        tools,
        model: "mock".into(),
        system: "test".into(),
        max_turns: 4,
        max_tokens: 128,
        compaction: Compaction::new(200_000, 0.9),
    };
    let outcome = agent
        .run(vec![Message::text(Role::User, "test hooks")], None)
        .await
        .unwrap();
    assert_eq!(outcome.text, "done");
    assert!(!root.join("forbidden").exists());
    for (file, event) in [
        ("pre.json", "PreToolUse"),
        ("post.json", "PostToolUse"),
        ("denied.json", "PreToolUse"),
    ] {
        let payload: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join(file)).unwrap()).unwrap();
        assert_eq!(payload["event"], event);
        assert_eq!(payload["cwd"], root.to_string_lossy().as_ref());
    }
    let post: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("post.json")).unwrap()).unwrap();
    assert!(post["tool_output"]
        .as_str()
        .unwrap()
        .contains("verified contents"));
    assert!(outcome.messages.iter().flat_map(|m| &m.content).any(|c| matches!(c, Content::ToolResult { tool_use_id, is_error: true, .. } if tool_use_id == "bash-1")));
    server.await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn hook_drains_both_pipes_and_bounds_retained_output() {
    use gitcode::extensibility::{run_hook, HookDecision};
    let rule = HookRule {
        event: HookEvent::PreToolUse,
        matcher: None,
        command: "head -c 200000 /dev/zero >&2; head -c 200000 /dev/zero; cat >/dev/null".into(),
        timeout_ms: 5000,
    };
    let outcome = run_hook(&rule, &serde_json::json!({"input":"x".repeat(300_000)})).await;
    assert_eq!(outcome.decision, HookDecision::Allow);
    assert!(outcome.diagnostic.is_none(), "{:?}", outcome.diagnostic);
    assert!(outcome.additional_context.unwrap().len() <= 64 * 1024);
}

#[cfg(unix)]
#[tokio::test]
async fn enabled_plugin_loads_inside_user_root_and_runs_with_its_directory() {
    use gitcode::extensibility::{run_matching_hooks_in, HookDecision};
    let dir = tempfile::Builder::new()
        .prefix("gitcode plugin's ")
        .tempdir()
        .unwrap();
    let home = dir.path();
    let package = home.join(".gitcode/plugins/example");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("plugin.toml"), "schema_version = 1\nname = 'example'\n[[hooks]]\nevent = 'PreToolUse'\nmatcher = 'Read'\ncommand = '''printf '%s' \"$GITCODE_PLUGIN_DIR\"'''\n").unwrap();
    let rules = gitcode::plugins::load(home, &["example".into()]).unwrap();
    let result = run_matching_hooks_in(
        &rules,
        HookEvent::PreToolUse,
        Some("Read"),
        &serde_json::json!({}),
        Some(home),
    )
    .await;
    assert_eq!(result.decision, HookDecision::Allow);
    assert_eq!(
        result.additional_context.as_deref(),
        package.canonicalize().unwrap().to_str()
    );
    assert!(gitcode::plugins::load(home, &["../example".into()]).is_err());
    std::os::unix::fs::symlink(home, home.join(".gitcode/plugins/escape")).unwrap();
    assert!(gitcode::plugins::load(home, &["escape".into()]).is_err());
    std::fs::write(
        package.join("plugin.toml"),
        "schema_version = 2\nname = 'example'\n",
    )
    .unwrap();
    assert!(gitcode::plugins::load(home, &["example".into()]).is_err());
}

#[test]
fn projects_cannot_enable_plugins_without_user_trust() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let project = dir.path().join("project");
    std::fs::create_dir_all(home.join(".gitcode")).unwrap();
    std::fs::create_dir_all(project.join(".gitcode")).unwrap();
    std::fs::write(
        project.join(".gitcode/config.toml"),
        "plugins = ['example']\n",
    )
    .unwrap();
    let error = config::load(&home, &project, &BTreeMap::new(), &Overrides::new()).unwrap_err();
    assert!(error
        .to_string()
        .contains("project plugins require explicit user trust"));
    std::fs::write(
        home.join(".gitcode/config.toml"),
        "trust_project_config = true\n",
    )
    .unwrap();
    assert_eq!(
        config::load(&home, &project, &BTreeMap::new(), &Overrides::new())
            .unwrap()
            .plugins,
        ["example"]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn executable_loads_enabled_plugin_and_dispatches_its_hook() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let root = dir.path().join("workspace");
    let package = home.join(".gitcode/plugins/fixture");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::set_permissions(
        home.join(".gitcode"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("example.txt"), "runtime plugin fixture").unwrap();
    std::fs::write(home.join(".gitcode/config.toml"), "plugins = ['fixture']\n").unwrap();
    std::fs::write(
        package.join("plugin.toml"),
        r#"schema_version = 1
name = 'fixture'
[[hooks]]
event = 'PreToolUse'
matcher = 'Read'
command = "printf '{\"additionalContext\":\"plugin loaded in executable\"}'"
"#,
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for turn in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 8192];
            let body = loop {
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                    let len = String::from_utf8_lossy(&bytes[..end])
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|v| v.parse::<usize>().ok())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + len {
                        break serde_json::from_slice::<serde_json::Value>(
                            &bytes[end + 4..end + 4 + len],
                        )
                        .unwrap();
                    }
                }
            };
            let response = if turn == 0 {
                concat!("data: {\"type\":\"message_start\",\"message\":{}}\n\n", "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"read-1\",\"name\":\"Read\",\"input\":{\"path\":\"example.txt\"}}}\n\n", "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n", "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n", "data: {\"type\":\"message_stop\"}\n\n")
            } else {
                assert!(body["messages"]
                    .to_string()
                    .contains("plugin loaded in executable"));
                concat!("data: {\"type\":\"message_start\",\"message\":{}}\n\n", "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n", "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"plugin test complete\"}}\n\n", "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n", "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n", "data: {\"type\":\"message_stop\"}\n\n")
            };
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
        }
    });
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_gitcode"))
            .current_dir(root)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("GITCODE_MODEL")
            .args([
                "--base-url",
                &format!("http://{address}"),
                "--model",
                "mock",
                "-p",
                "read example",
            ])
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("plugin test complete"));
    server.await.unwrap();
}
