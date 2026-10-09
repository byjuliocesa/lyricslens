//! Noticing that a newer release exists.
//!
//! Nothing is ever installed here. Someone who installed with `install.sh` has
//! no package manager to tell them, so the program looks once a day and says
//! so in the preferences window. Upgrading stays something they ask for.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

// By repository id rather than owner/name: the id survives a rename of either.
const RELEASES: &str = "https://api.github.com/repositories/1338589467/releases/latest";

/// How long to leave it before looking again. A program that sits open all day
/// has no business asking more often than this.
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

const STAMP: &str = "last-update-check";

/// The newest release, when it is newer than this build and it is time to ask.
pub async fn newer_than_this(http: &reqwest::Client) -> Option<String> {
    // Flatpak updates the program with everything else; news of a release
    // would only point at an installer that does not apply.
    if crate::desktop::sandboxed() || !due() {
        return None;
    }
    stamp();

    let response = http
        .get(RELEASES)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        tracing::debug!(status = %response.status(), "could not ask about releases");
        return None;
    }

    let body: serde_json::Value = response.json().await.ok()?;
    let latest = body.get("tag_name")?.as_str()?.trim_start_matches('v');
    newer(latest, env!("CARGO_PKG_VERSION")).then(|| latest.to_owned())
}

/// Whether `latest` is a later version than `running`.
fn newer(latest: &str, running: &str) -> bool {
    let parts = |version: &str| -> Vec<u64> {
        version
            .split('.')
            .map(|part| part.trim().parse().unwrap_or(0))
            .collect()
    };
    parts(latest) > parts(running)
}

fn path() -> Option<PathBuf> {
    Some(crate::store::cache_dir()?.join(STAMP))
}

fn due() -> bool {
    let Some(path) = path() else {
        return false;
    };
    let Ok(changed) = std::fs::metadata(&path).and_then(|data| data.modified()) else {
        // Never asked, so now is the time.
        return true;
    };
    changed.elapsed().is_ok_and(|since| since >= EVERY)
}

/// Writes the time down before asking, not after: a service that is refusing
/// every call should be asked once a day, not once a minute.
fn stamp() {
    let Some(path) = path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        &path,
        SystemTime::now().elapsed().map_or(0, |_| 0).to_string(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_later_version_is_newer() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("0.1.10", "0.1.9"));
        assert!(newer("1.0.0", "0.9.9"));
    }

    #[test]
    fn the_same_version_is_not() {
        assert!(!newer("0.1.9", "0.1.9"));
        assert!(!newer("0.1.8", "0.1.9"));
    }

    #[test]
    fn nonsense_does_not_claim_to_be_newer() {
        assert!(!newer("", "0.1.9"));
        assert!(!newer("latest", "0.1.9"));
    }
}
