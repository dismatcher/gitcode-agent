# Tool lifecycle hooks

GitCode can run explicitly configured shell commands before and after model tool calls in interactive and headless sessions, including calls by subagents. Configure `[[hooks]]` in the user's `~/.gitcode/config.toml`:

```toml
[[hooks]]
event = "PreToolUse"
matcher = "Bash"
command = "my-hook-check"
timeout_ms = 5000

[[hooks]]
event = "PostToolUse"
matcher = "Read|Write"
command = "my-hook-audit"
```

`event` is `PreToolUse` or `PostToolUse`. `matcher` is optional, a tool name or a regular expression; omission or `*` matches all. `timeout_ms` defaults to 5000 and is limited to 1–60000. Up to 32 rules run in order. Project hook configuration is rejected unless the user has set `trust_project_config = true` in their own configuration. Hooks execute as the current user, with its environment and workspace as the working directory; configure only commands you trust. `/hooks` reports the number of configured rules without displaying commands.

The command receives a JSON object on standard input with `event`, `cwd`, `tool_use_id`, `tool_name` and `tool_input`. `PostToolUse` also receives `tool_output` and `is_error`, which can contain sensitive data. Hooks can return a JSON object with `decision`, `reason`, and `additionalContext`, or plain stdout as additional context for the model. A `PreToolUse` hook may block a call by returning `{"decision":"deny","reason":"..."}` or exiting with status 2 (stderr becomes the reason). Additional context is appended to the tool result. A `PostToolUse` denial is reported as feedback; it cannot undo an operation already performed. Native permissions still apply independently.

Output retained from each stream is limited to 64 KiB. Timeouts, startup failures and exits other than 2 are non-blocking and produce a diagnostic in the tool result; hooks are not a security boundary or reliable audit log. Hook execution may be interrupted with the parent turn, and external commands can have side effects. Other lifecycle events such as session start, prompt submission and compaction are not yet dispatched; configuring them is rejected rather than silently ignored.
