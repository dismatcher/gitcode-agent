# Tool lifecycle hooks

GitCode can run explicitly configured shell commands at tool and turn lifecycle points in interactive and headless sessions, including calls made by subagents. Configure `[[hooks]]` in the user's `~/.gitcode/config.toml`:

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

[[hooks]]
event = "Stop"
command = "my-turn-report"

[[hooks]]
event = "PreCompact"
command = "my-context-guard"
```

`event` is one of `PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `Stop`, `SubagentStop` or `PreCompact`. `SessionStart`, `SessionEnd` and `Notification` are declared but not dispatched; configuring them is rejected rather than silently ignored. `matcher` is optional, a tool name or a regular expression; omission or `*` matches all. It is only consulted for the two tool events. `timeout_ms` defaults to 5000 and is limited to 1–60000. Up to 32 rules run in order. Project hook configuration is rejected unless the user has set `trust_project_config = true` in their own configuration. Hooks execute as the current user, with its environment and workspace as the working directory; configure only commands you trust. `/hooks` reports the number of configured rules without displaying commands.

The command receives a JSON object on standard input with `event`, `cwd`, `tool_use_id`, `tool_name` and `tool_input`. `PostToolUse` also receives `tool_output` and `is_error`, which can contain sensitive data. Hooks can return a JSON object with `decision`, `reason`, and `additionalContext`, or plain stdout as additional context for the model. A `PreToolUse` hook may block a call by returning `{"decision":"deny","reason":"..."}` or exiting with status 2 (stderr becomes the reason). Additional context is appended to the tool result. A `PostToolUse` denial is reported as feedback; it cannot undo an operation already performed. Native permissions still apply independently.

The non-tool events carry their own payload instead of tool fields:

- `UserPromptSubmit` receives `prompt`, the latest operator message. It fires once at the start of a turn.
- `Stop` receives `text` and `iterations` after the main agent produces its final answer.
- `SubagentStop` receives `subagent`, `text` and `iterations` when a delegated child finishes.
- `PreCompact` receives `estimated_tokens` and `window` before context summarization runs.

For these four events the hook can add context but cannot deny the event, because it has already happened by the time the hook runs. Returned `additionalContext` is appended to the conversation as a `[Hook context]` message so the model sees it on the next step.

Output retained from each stream is limited to 64 KiB. Timeouts, startup failures and exits other than 2 are non-blocking and produce a diagnostic in the tool result; hooks are not a security boundary or reliable audit log. Hook execution may be interrupted with the parent turn, and external commands can have side effects.
