//! Update checks against the project's GitHub releases.

use anyhow::{Context, Result};
use serde_json::Value;

const RELEASES_API_URL: &str =
    "https://api.github.com/repos/itamar567/df-desktop/releases/latest";

/// A release that is newer than the running build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub version: String,
    /// Release page URL to open when the user asks for the download.
    pub url: String,
}

pub async fn check() -> Result<Option<AvailableUpdate>> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("dragonfable-launcher/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let release: Value = client
        .get(RELEASES_API_URL)
        .send()
        .await
        .context("update check request failed")?
        .error_for_status()
        .context("update check returned an error status")?
        .json()
        .await
        .context("update check response was not valid JSON")?;
    Ok(latest_update(&release))
}

/// Extracts an update from a GitHub `/releases/latest` JSON body, or `None`
/// when the published release is not newer than this build.
fn latest_update(release: &Value) -> Option<AvailableUpdate> {
    let tag = release.get("tag_name")?.as_str()?;
    let version = newer_than_current(tag)?;
    let url = release.get("html_url")?.as_str()?.to_string();
    Some(AvailableUpdate { version, url })
}

/// Parses a release tag (`v`-prefixed semver) and returns the normalized
/// version string when it is newer than the running build.
fn newer_than_current(tag: &str) -> Option<String> {
    let current: semver::Version = env!("CARGO_PKG_VERSION").parse().ok()?;
    let latest: semver::Version = tag.trim().trim_start_matches('v').parse().ok()?;
    (latest > current).then(|| latest.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn current_version() -> semver::Version {
        env!("CARGO_PKG_VERSION").parse().unwrap()
    }

    fn tag_newer_than_current(major: u64, minor: u64, patch: u64) -> Option<String> {
        // Tests can't rewrite the compile-time current version, so compare
        // tags around it via the same parse logic `newer_than_current` uses.
        let latest = semver::Version::new(major, minor, patch);
        (latest > current_version()).then(|| latest.to_string())
    }

    #[test]
    fn older_tag_is_not_an_update() {
        assert_eq!(newer_than_current("v0.0.1"), None);
    }

    #[test]
    fn equal_tag_is_not_an_update() {
        assert_eq!(newer_than_current(current_version().to_string().as_str()), None);
    }

    #[test]
    fn newer_tag_without_v_prefix_still_normalizes() {
        assert_eq!(
            newer_than_current("999.0.0"),
            tag_newer_than_current(999, 0, 0)
        );
        assert_eq!(newer_than_current("999.0.0"), Some("999.0.0".to_string()));
    }

    #[test]
    fn malformed_tag_is_ignored() {
        assert_eq!(newer_than_current("not-a-version"), None);
        assert_eq!(newer_than_current(""), None);
    }

    #[test]
    fn release_json_yields_the_update() {
        let release = json!({
            "tag_name": "v999.0.0",
            "html_url": "https://github.com/itamar567/df-desktop/releases/tag/v999.0.0"
        });
        assert_eq!(
            latest_update(&release),
            Some(AvailableUpdate {
                version: "999.0.0".to_string(),
                url: "https://github.com/itamar567/df-desktop/releases/tag/v999.0.0".to_string(),
            })
        );
    }

    #[test]
    fn release_json_at_or_below_current_version_yields_none() {
        let release = json!({
            "tag_name": "v0.0.1",
            "html_url": "https://github.com/itamar567/df-desktop/releases/tag/v0.0.1"
        });
        assert_eq!(latest_update(&release), None);
    }

    #[test]
    fn release_json_with_missing_fields_yields_none() {
        assert_eq!(latest_update(&json!({})), None);
        assert_eq!(latest_update(&json!({ "tag_name": "v999.0.0" })), None);
    }
}
