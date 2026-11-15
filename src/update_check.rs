//! Cached release lookup that feeds the startup update banner.
//!
//! The lookup is deliberately decoupled from installation: the UI reads the
//! previously cached answer and a background task refreshes it, so startup is
//! never blocked by a network call.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Cached lookup result, persisted between runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionInfo {
    /// Newest published version, without a leading `v`.
    pub latest_version: String,
    /// Unix timestamp of the last successful lookup.
    pub last_checked_at: u64,
    /// Version the operator asked not to be reminded about again.
    #[serde(default)]
    pub dismissed_version: Option<String>,
}

impl VersionInfo {
    /// Whether the cached answer is old enough to refresh in the background.
    pub fn stale(&self, now: u64) -> bool {
        now.saturating_sub(self.last_checked_at) >= REFRESH_HOURS * 3600
    }

    /// Whether a newer version should be shown, honouring a dismissal.
    pub fn banner(&self, installed: &str) -> Option<&str> {
        if self.dismissed_version.as_deref() == Some(self.latest_version.as_str()) {
            return None;
        }
        newer(installed, &self.latest_version).then_some(self.latest_version.as_str())
    }
}

/// Hours before the cached lookup is refreshed, matching Codex's cadence.
pub const REFRESH_HOURS: u64 = 20;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// Cache path inside the application state directory.
pub fn path(home: &Path) -> PathBuf {
    home.join(".gitcode").join("version.json")
}

/// Read the cached lookup; a missing or malformed file is simply no answer.
pub fn read(home: &Path) -> Option<VersionInfo> {
    let text = std::fs::read_to_string(path(home)).ok()?;
    let info: VersionInfo = serde_json::from_str(&text).ok()?;
    is_version(&info.latest_version).then_some(info)
}

fn is_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_digit()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".+-".contains(&b))
}

/// Compare two versions, treating a leading `v` as decoration.
pub fn newer(installed: &str, latest: &str) -> bool {
    let parse = |value: &str| semver::Version::parse(value.trim_start_matches('v')).ok();
    match (parse(installed), parse(latest)) {
        (Some(installed), Some(latest)) => latest > installed,
        _ => false,
    }
}

/// Persist the lookup, keeping any previous dismissal.
pub fn store(home: &Path, latest: &str, now: u64) -> std::io::Result<VersionInfo> {
    let info = VersionInfo {
        latest_version: latest.trim_start_matches('v').to_owned(),
        last_checked_at: now,
        dismissed_version: read(home).and_then(|info| info.dismissed_version),
    };
    let file = path(home);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string(&info).map_err(std::io::Error::other)?;
    text.push('\n');
    std::fs::write(&file, text)?;
    Ok(info)
}

/// Remember that this exact version must not be offered again.
pub fn dismiss(home: &Path, version: &str) {
    let Some(mut info) = read(home) else { return };
    info.dismissed_version = Some(version.to_owned());
    let file = path(home);
    if let Ok(mut text) = serde_json::to_string(&info) {
        text.push('\n');
        let _ = std::fs::write(file, text);
    }
}

/// Query the published manifest and refresh the cache without blocking the UI.
pub async fn refresh(home: PathBuf) {
    let base = std::env::var("GITCODE_BASE_URL")
        .unwrap_or_else(|_| crate::update::INSTALL_BASE_URL.into());
    let Ok(latest) = published_version(&base).await else {
        return;
    };
    let _ = tokio::task::spawn_blocking(move || store(&home, &latest, now())).await;
}

/// Read only the version field of the published manifest.
async fn published_version(base: &str) -> Result<String, String> {
    let url = format!("{}/latest.json", base.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .user_agent(concat!("gitcode-update-check/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let bytes = client
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 {
        return Err("Некорректный latest.json".into());
    }
    #[derive(Deserialize)]
    struct Manifest {
        version: String,
    }
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|e| format!("Некорректный latest.json: {e}"))?;
    let version = manifest.version.trim_start_matches('v').to_owned();
    is_version(&version)
        .then_some(version)
        .ok_or_else(|| "Некорректная версия релиза".into())
}

/// Return the banner version and schedule a refresh when the cache is stale.
///
/// The UI reads only what was cached by an earlier run, so a slow or offline
/// network never delays the first frame. A missing cache schedules the first
/// lookup, which surfaces on the next start.
pub fn banner(home: &Path, installed: &'static str) -> Option<String> {
    let info = read(home);
    let stale = info.as_ref().is_none_or(|info| info.stale(now()));
    if stale {
        let directory = home.to_path_buf();
        tokio::spawn(async move { refresh(directory).await });
    }
    info?.banner(installed).map(str::to_owned)
}

/// Total seconds since the Unix epoch, exposed for deterministic tests.
pub fn timestamp() -> u64 {
    now()
}

/// Human-readable duration since the last lookup.
pub fn age(info: &VersionInfo, now: u64) -> Duration {
    Duration::from_secs(now.saturating_sub(info.last_checked_at))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn cache_round_trips_and_keeps_the_dismissal() {
        let home = temp();
        let first = store(home.path(), "0.5.0", 1_000).unwrap();
        assert_eq!(first.latest_version, "0.5.0");
        assert_eq!(first.dismissed_version, None);
        dismiss(home.path(), "0.5.0");
        let second = store(home.path(), "0.6.0", 2_000).unwrap();
        assert_eq!(second.latest_version, "0.6.0");
        assert_eq!(second.dismissed_version.as_deref(), Some("0.5.0"));
    }

    #[test]
    fn banner_skips_the_dismissed_version_only() {
        let info = VersionInfo {
            latest_version: "0.5.0".into(),
            last_checked_at: 0,
            dismissed_version: Some("0.5.0".into()),
        };
        assert_eq!(info.banner("0.4.0"), None);
        let newer = VersionInfo {
            latest_version: "0.6.0".into(),
            ..info
        };
        assert_eq!(newer.banner("0.4.0"), Some("0.6.0"));
    }

    #[test]
    fn an_older_published_version_is_not_offered() {
        let info = VersionInfo {
            latest_version: "0.3.3".into(),
            last_checked_at: 0,
            dismissed_version: None,
        };
        assert_eq!(info.banner("0.4.0"), None);
    }

    #[test]
    fn stale_cache_is_refreshed_after_twenty_hours() {
        let fresh = VersionInfo {
            latest_version: "0.5.0".into(),
            last_checked_at: 1_000,
            dismissed_version: None,
        };
        assert!(!fresh.stale(1_000 + REFRESH_HOURS * 3600 - 1));
        assert!(fresh.stale(1_000 + REFRESH_HOURS * 3600));
        assert_eq!(age(&fresh, 1_000 + 60), Duration::from_secs(60));
    }

    #[test]
    fn malformed_and_missing_cache_files_are_ignored() {
        let home = temp();
        assert!(read(home.path()).is_none());
        std::fs::create_dir_all(home.path().join(".gitcode")).unwrap();
        std::fs::write(path(home.path()), "{\"latest_version\":\"nope\"}").unwrap();
        assert!(read(home.path()).is_none());
        std::fs::write(
            path(home.path()),
            "{\"latest_version\":\"0.9.0\",\"last_checked_at\":5,\"extra\":1}",
        )
        .unwrap();
        assert!(read(home.path()).is_none());
    }
}
