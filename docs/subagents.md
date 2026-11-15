# Subagents

The `Agent` tool delegates a self-contained task to a separate model conversation. During an interactive turn, the parent receives a launch result immediately and can continue independent work while up to four children run concurrently. Completed child reports are delivered back to the parent in that turn; the parent waits for remaining children before its final answer. Headless execution waits for each batch of independent children. Interrupting the parent cancels its active children; their operations may already have changed files.

Built-in profiles are `explore` and `plan` (read-only) and `general-purpose` (workspace tools). Each child inherits the parent's workspace and permission rules. Tool restrictions are enforced both in the advertised schemas and at execution; children cannot start further subagents. Custom profiles are loaded from `~/.gitcode/agents/*.md` and `.gitcode/agents/*.md`, with project profiles taking precedence. Use `/agents` to list them.

Active and recently finished subagents appear under the prompt with their current tool and reported output tokens. Their independent conversation is not added to the parent; only their final report is returned. Background agents that continue after the parent turn, agent resumption and interactive permission prompts for child tools are not yet supported.
