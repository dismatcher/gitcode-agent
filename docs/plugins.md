# Local hook plugins

Plugins bundle trusted hook commands. They are disabled by default: create a package in `~/.gitcode/plugins/<name>/plugin.toml` and explicitly enable it in `~/.gitcode/config.toml`:

```toml
plugins = ["example"]
```

For `~/.gitcode/plugins/example/plugin.toml`:

```toml
schema_version = 1
name = "example"

[[hooks]]
event = "PreToolUse"
matcher = "Bash"
command = '"$GITCODE_PLUGIN_DIR/check.sh"'
timeout_ms = 5000
```

Plugin commands run as the current user, inherit its environment and receive `GITCODE_PLUGIN_DIR`; they can read private data or change files. Enable only packages you trust. Names contain letters, digits, hyphens or underscores; plugin paths and manifests cannot resolve outside the user plugin directory. Project configuration cannot enable plugins without the user's explicit `trust_project_config = true` setting. GitCode loads only named packages, and fails startup if an enabled package is invalid.

Version 1 exposes only the [pre/post tool hook API](hooks.md). It does not load OpenCode JavaScript plugins, custom tool schemas, GUI extensions or themes. Install and update package files separately; there is no package registry or automatic update mechanism.
