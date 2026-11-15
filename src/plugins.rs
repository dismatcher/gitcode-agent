//! Explicit, local plugin packages. No package runs unless selected by configuration.

use crate::extensibility::HookRule;
use serde::Deserialize;
use std::{fs, io, path::Path};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    name: String,
    #[serde(default)]
    hooks: Vec<HookRule>,
}

/// Names cannot address files outside the local plugin root.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// Load hooks from enabled local packages under `~/.gitcode/plugins`.
///
/// The manifest is bounded and packages resolving outside that directory are rejected.
/// Commands are trusted executable code and receive `GITCODE_PLUGIN_DIR`.
pub fn load(home: &Path, enabled: &[String]) -> io::Result<Vec<HookRule>> {
    if enabled.is_empty() {
        return Ok(Vec::new());
    }
    let root = fs::canonicalize(home.join(".gitcode/plugins"))?;
    let mut hooks = Vec::new();
    for name in enabled {
        if !valid_name(name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid plugin name",
            ));
        }
        let package = fs::canonicalize(root.join(name))?;
        if !package.starts_with(&root) || !package.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "plugin path escapes plugin directory",
            ));
        }
        let manifest = fs::canonicalize(package.join("plugin.toml"))?;
        if !manifest.starts_with(&package)
            || !manifest.is_file()
            || fs::metadata(&manifest)?.len() > 65_536
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid plugin manifest",
            ));
        }
        let raw = fs::read_to_string(&manifest)?;
        if raw.len() > 65_536 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "plugin manifest too large",
            ));
        }
        let manifest: Manifest = toml::from_str(&raw)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid plugin manifest"))?;
        if manifest.schema_version != 1 || manifest.name != *name {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "plugin name or schema version mismatch",
            ));
        }
        let package = package.to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "plugin path is not UTF-8")
        })?;
        let quoted = package.replace('\'', "'\\''");
        for mut hook in manifest.hooks {
            if hook.command.trim().is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "plugin hook command is empty",
                ));
            }
            hook.command = format!(
                "GITCODE_PLUGIN_DIR='{quoted}'; export GITCODE_PLUGIN_DIR; {}",
                hook.command
            );
            hooks.push(hook);
            if hooks.len() > 32 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "too many plugin hooks",
                ));
            }
        }
    }
    Ok(hooks)
}
