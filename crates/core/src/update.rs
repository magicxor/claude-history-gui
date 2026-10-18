//! Update check for claude-history-gui: the latest GitHub release of
//! [`REPOSITORY`] compared with the running version.
//!
//! Upstream's self-updater (`update.rs`: curl, tar and sha256sum, no Windows
//! support) is not ported. The GUI only checks for a newer release and
//! links to it. Everything here is pure except [`check_latest_release`]
//! (HTTPS through ureq with rustls, 30 s timeout), which never runs under
//! `cfg(test)`: tests use [`parse_release`] and [`update_status`] with
//! fixture JSON.
//!
//! The caller passes its own version (the GUI binary's
//! `env!("CARGO_PKG_VERSION")`); this crate's version tracks upstream.

use crate::error::{AppError, Result};
use serde::Deserialize;
use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

/// The GitHub repository releases are published in.
pub const REPOSITORY: &str = "magicxor/claude-history-gui";
/// GitHub API endpoint of the latest (non-draft, non-prerelease) release.
pub const LATEST_RELEASE_API_URL: &str =
    "https://api.github.com/repos/magicxor/claude-history-gui/releases/latest";
/// The releases page, for "open in browser".
pub const RELEASES_PAGE_URL: &str = "https://github.com/magicxor/claude-history-gui/releases";

/// A pre-release identifier (`beta`, `1`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PreRelease {
    Numeric(u64),
    Alpha(String),
}

impl Ord for PreRelease {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (PreRelease::Numeric(left), PreRelease::Numeric(right)) => left.cmp(right),
            (PreRelease::Numeric(_), PreRelease::Alpha(_)) => Ordering::Less,
            (PreRelease::Alpha(_), PreRelease::Numeric(_)) => Ordering::Greater,
            (PreRelease::Alpha(left), PreRelease::Alpha(right)) => left.cmp(right),
        }
    }
}

impl PartialOrd for PreRelease {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A semantic version (`MAJOR.MINOR.PATCH[-PRE][+BUILD]`) with SemVer 2.0
/// precedence; build metadata is ignored.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<PreRelease>,
}

impl Version {
    pub fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
            pre: Vec::new(),
        }
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                // A pre-release sorts before its release.
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl FromStr for Version {
    type Err = String;

    /// Accepts an optional `v`/`V` prefix (release tags) and one to three
    /// numeric components (`1`, `1.2`, `1.2.3`; missing ones are 0).
    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        let invalid = || format!("invalid version '{input}'");
        let text = input.trim();
        let text = text
            .strip_prefix('v')
            .or_else(|| text.strip_prefix('V'))
            .unwrap_or(text);
        let text = text.split_once('+').map_or(text, |(version, _)| version);
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let numbers = core
            .split('.')
            .map(|part| {
                (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| part.parse::<u64>().ok())
                    .flatten()
                    .ok_or_else(invalid)
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if numbers.is_empty() || numbers.len() > 3 {
            return Err(invalid());
        }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => pre
                .split('.')
                .map(|part| {
                    if part.is_empty()
                        || !part
                            .chars()
                            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
                    {
                        Err(invalid())
                    } else if part.bytes().all(|byte| byte.is_ascii_digit()) {
                        part.parse::<u64>()
                            .map(PreRelease::Numeric)
                            .map_err(|_| invalid())
                    } else {
                        Ok(PreRelease::Alpha(part.to_string()))
                    }
                })
                .collect::<std::result::Result<Vec<_>, _>>()?,
        };
        Ok(Version {
            major: numbers[0],
            minor: numbers.get(1).copied().unwrap_or(0),
            patch: numbers.get(2).copied().unwrap_or(0),
            pre,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        for (index, part) in self.pre.iter().enumerate() {
            f.write_str(if index == 0 { "-" } else { "." })?;
            match part {
                PreRelease::Numeric(number) => write!(f, "{number}")?,
                PreRelease::Alpha(text) => f.write_str(text)?,
            }
        }
        Ok(())
    }
}

/// A downloadable file of a release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub download_url: String,
    pub size: u64,
    pub content_type: Option<String>,
}

/// A published release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseInfo {
    /// The tag (`v0.2.0`).
    pub tag: String,
    /// The version parsed from the tag.
    pub version: Version,
    /// The release title.
    pub name: Option<String>,
    /// The release page.
    pub url: String,
    /// Release notes (Markdown).
    pub notes: String,
    pub published_at: Option<String>,
    pub prerelease: bool,
    pub draft: bool,
    pub assets: Vec<ReleaseAsset>,
}

impl ReleaseInfo {
    /// The asset built for `os` (`std::env::consts::OS`: `windows`,
    /// `macos`, `linux`) and `arch` (`x86_64`, `aarch64`), judged by its
    /// file name; checksum and signature files are skipped.
    pub fn asset_for(&self, os: &str, arch: &str) -> Option<&ReleaseAsset> {
        let os_words: &[&str] = match os {
            "windows" => &["windows", "win64", "win32", "-win-", "_win_", ".win."],
            "macos" => &["macos", "darwin", "apple", "osx", "mac-", "-mac"],
            "linux" => &["linux"],
            other => return self.assets.iter().find(|asset| asset.name.contains(other)),
        };
        let arch_words: Vec<&str> = match arch {
            "x86_64" => vec!["x86_64", "x86-64", "amd64", "x64", "win64"],
            "aarch64" => vec!["aarch64", "arm64"],
            other => vec![other],
        };
        let candidates = self.assets.iter().filter(|asset| {
            let name = asset.name.to_ascii_lowercase();
            !(name.ends_with(".sha256")
                || name.ends_with(".sha256sum")
                || name.ends_with(".sig")
                || name.ends_with(".asc")
                || name.ends_with(".minisig"))
                && os_words.iter().any(|word| name.contains(word))
        });
        let mut fallback = None;
        for asset in candidates {
            let name = asset.name.to_ascii_lowercase();
            if arch_words.iter().any(|word| name.contains(word)) {
                return Some(asset);
            }
            // A universal build (no architecture in the name).
            let names_any_arch = [
                "x86_64", "x86-64", "amd64", "x64", "aarch64", "arm64", "i686",
            ]
            .iter()
            .any(|word| name.contains(word));
            if !names_any_arch && fallback.is_none() {
                fallback = Some(asset);
            }
        }
        fallback
    }
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    name: Option<String>,
    html_url: String,
    body: Option<String>,
    published_at: Option<String>,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    content_type: Option<String>,
}

/// Parse a GitHub "release" JSON object (`GET /repos/{repo}/releases/latest`).
pub fn parse_release(json: &str) -> Result<ReleaseInfo> {
    let release: GithubRelease = serde_json::from_str(json)
        .map_err(|error| AppError::UpdateError(format!("invalid release data: {error}")))?;
    let version = release
        .tag_name
        .parse::<Version>()
        .map_err(|error| AppError::UpdateError(format!("release tag: {error}")))?;
    Ok(ReleaseInfo {
        tag: release.tag_name,
        version,
        name: release.name.filter(|name| !name.trim().is_empty()),
        url: release.html_url,
        notes: release.body.unwrap_or_default(),
        published_at: release.published_at,
        prerelease: release.prerelease,
        draft: release.draft,
        assets: release
            .assets
            .into_iter()
            .map(|asset| ReleaseAsset {
                name: asset.name,
                download_url: asset.browser_download_url,
                size: asset.size,
                content_type: asset.content_type,
            })
            .collect(),
    })
}

/// The outcome of an update check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateStatus {
    /// The running version is the latest release.
    UpToDate { current: Version },
    /// A newer release exists.
    Available {
        current: Version,
        release: ReleaseInfo,
    },
    /// The running version is newer than the latest release (a development
    /// or pre-release build).
    Newer {
        current: Version,
        latest: ReleaseInfo,
    },
    /// The repository has no published release yet.
    NoRelease { current: Version },
}

/// Compare `current_version` with the latest `release` (`None`: nothing
/// published).
pub fn update_status(current_version: &str, release: Option<ReleaseInfo>) -> Result<UpdateStatus> {
    let current = current_version
        .parse::<Version>()
        .map_err(AppError::UpdateError)?;
    Ok(match release {
        None => UpdateStatus::NoRelease { current },
        Some(release) => match release.version.cmp(&current) {
            Ordering::Greater => UpdateStatus::Available { current, release },
            Ordering::Equal => UpdateStatus::UpToDate { current },
            Ordering::Less => UpdateStatus::Newer {
                current,
                latest: release,
            },
        },
    })
}

/// A readable error for a non-success GitHub API response.
fn github_error(status: u16, body: &str) -> AppError {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("message")?.as_str().map(str::to_string))
        .unwrap_or_default();
    AppError::UpdateError(if message.is_empty() {
        format!("GitHub returned HTTP {status}")
    } else {
        format!("GitHub returned HTTP {status}: {message}")
    })
}

/// Interpret a GitHub API response (status and body) for
/// [`LATEST_RELEASE_API_URL`].
pub fn update_status_from_response(
    current_version: &str,
    status: u16,
    body: &str,
) -> Result<UpdateStatus> {
    match status {
        200 => update_status(current_version, Some(parse_release(body)?)),
        404 => update_status(current_version, None),
        status => Err(github_error(status, body)),
    }
}

/// Ask GitHub for the latest release and compare it with
/// `current_version` (blocking; run it off the UI thread).
#[cfg(not(any(test, feature = "hermetic")))]
pub fn check_latest_release(current_version: &str) -> Result<UpdateStatus> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        .http_status_as_error(false)
        .user_agent(format!("claude-history-gui/{current_version}"))
        .build()
        .into();
    let mut response = agent
        .get(LATEST_RELEASE_API_URL)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|error| AppError::UpdateError(format!("failed to reach GitHub: {error}")))?;
    let status = response.status().as_u16();
    let body = response.body_mut().read_to_string().map_err(|error| {
        AppError::UpdateError(format!("failed to read GitHub's reply: {error}"))
    })?;
    update_status_from_response(current_version, status, &body)
}

/// Guard rail: tests (and every `hermetic` build) must never use the
/// network.
#[cfg(any(test, feature = "hermetic"))]
pub fn check_latest_release(_current_version: &str) -> Result<UpdateStatus> {
    panic!("tests must not use the network; use update_status_from_response");
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE_JSON: &str = r###"{
        "url": "https://api.github.com/repos/magicxor/claude-history-gui/releases/1",
        "html_url": "https://github.com/magicxor/claude-history-gui/releases/tag/v0.2.0",
        "tag_name": "v0.2.0",
        "name": "claude-history-gui 0.2.0",
        "draft": false,
        "prerelease": false,
        "published_at": "2026-10-01T12:00:00Z",
        "body": "## Changes\n- Faster search",
        "assets": [
            {"name": "claude-history-gui-0.2.0-x86_64-pc-windows-msvc.zip",
             "browser_download_url": "https://example.invalid/win.zip",
             "size": 1234, "content_type": "application/zip"},
            {"name": "claude-history-gui-0.2.0-x86_64-pc-windows-msvc.zip.sha256",
             "browser_download_url": "https://example.invalid/win.zip.sha256",
             "size": 64, "content_type": "text/plain"},
            {"name": "claude-history-gui-0.2.0-aarch64-apple-darwin.dmg",
             "browser_download_url": "https://example.invalid/mac-arm.dmg",
             "size": 2000, "content_type": null},
            {"name": "claude-history-gui-0.2.0-x86_64-apple-darwin.dmg",
             "browser_download_url": "https://example.invalid/mac-x64.dmg",
             "size": 2001},
            {"name": "claude-history-gui-0.2.0-linux-amd64.tar.gz",
             "browser_download_url": "https://example.invalid/linux.tar.gz",
             "size": 3000}
        ]
    }"###;

    fn version(text: &str) -> Version {
        text.parse().unwrap()
    }

    #[test]
    fn versions_parse_and_display() {
        assert_eq!(version("v1.2.3"), Version::new(1, 2, 3));
        assert_eq!(version("V0.1.76"), Version::new(0, 1, 76));
        assert_eq!(version("1.2"), Version::new(1, 2, 0));
        assert_eq!(version(" 2 "), Version::new(2, 0, 0));
        assert_eq!(version("1.2.3+build.5"), Version::new(1, 2, 3));
        let pre = version("1.0.0-beta.11");
        assert!(pre.is_prerelease());
        assert_eq!(
            pre.pre,
            vec![PreRelease::Alpha("beta".into()), PreRelease::Numeric(11)]
        );
        assert_eq!(pre.to_string(), "1.0.0-beta.11");
        assert_eq!(version("v0.2.0").to_string(), "0.2.0");
        for bad in [
            "",
            "v",
            "1..2",
            "1.2.3.4",
            "x.y.z",
            "1.2.-3",
            "1.0.0-",
            "1.0.0-a..b",
            "1.0.0-a b",
        ] {
            assert!(bad.parse::<Version>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn semver_precedence() {
        // The SemVer 2.0 example chain.
        let ordered = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "2.0.0",
            "10.0.0",
        ];
        for pair in ordered.windows(2) {
            assert!(
                version(pair[0]) < version(pair[1]),
                "{} < {}",
                pair[0],
                pair[1]
            );
        }
        assert_eq!(version("1.0.0+a").cmp(&version("1.0.0+b")), Ordering::Equal);
    }

    #[test]
    fn release_json_parses() {
        let release = parse_release(RELEASE_JSON).unwrap();
        assert_eq!(release.tag, "v0.2.0");
        assert_eq!(release.version, Version::new(0, 2, 0));
        assert_eq!(release.name.as_deref(), Some("claude-history-gui 0.2.0"));
        assert_eq!(
            release.url,
            "https://github.com/magicxor/claude-history-gui/releases/tag/v0.2.0"
        );
        assert_eq!(release.notes, "## Changes\n- Faster search");
        assert_eq!(
            release.published_at.as_deref(),
            Some("2026-10-01T12:00:00Z")
        );
        assert!(!release.prerelease && !release.draft);
        assert_eq!(release.assets.len(), 5);
        assert_eq!(release.assets[0].size, 1234);
        assert_eq!(release.assets[2].content_type, None);

        let minimal = parse_release(
            r#"{"tag_name":"0.3.0-rc.1","html_url":"https://x","name":"","body":null}"#,
        )
        .unwrap();
        assert_eq!(minimal.name, None);
        assert!(minimal.notes.is_empty() && minimal.assets.is_empty());
        assert!(minimal.version.is_prerelease());

        assert!(matches!(
            parse_release("not json"),
            Err(AppError::UpdateError(_))
        ));
        let bad_tag =
            parse_release(r#"{"tag_name":"nightly","html_url":"https://x"}"#).unwrap_err();
        assert_eq!(
            bad_tag.to_string(),
            "Update error: release tag: invalid version 'nightly'"
        );
    }

    #[test]
    fn platform_assets() {
        let release = parse_release(RELEASE_JSON).unwrap();
        let name = |os, arch| release.asset_for(os, arch).map(|asset| asset.name.as_str());
        assert_eq!(
            name("windows", "x86_64"),
            Some("claude-history-gui-0.2.0-x86_64-pc-windows-msvc.zip")
        );
        assert_eq!(
            name("macos", "aarch64"),
            Some("claude-history-gui-0.2.0-aarch64-apple-darwin.dmg")
        );
        assert_eq!(
            name("macos", "x86_64"),
            Some("claude-history-gui-0.2.0-x86_64-apple-darwin.dmg")
        );
        assert_eq!(
            name("linux", "x86_64"),
            Some("claude-history-gui-0.2.0-linux-amd64.tar.gz")
        );
        assert_eq!(name("linux", "aarch64"), None);
        assert_eq!(name("windows", "aarch64"), None);
    }

    #[test]
    fn update_status_compares_versions() {
        let release = parse_release(RELEASE_JSON).unwrap();
        assert_eq!(
            update_status("0.1.9", Some(release.clone())).unwrap(),
            UpdateStatus::Available {
                current: Version::new(0, 1, 9),
                release: release.clone()
            }
        );
        assert_eq!(
            update_status("0.2.0", Some(release.clone())).unwrap(),
            UpdateStatus::UpToDate {
                current: Version::new(0, 2, 0)
            }
        );
        // A release candidate of the same version is older than the release.
        assert!(matches!(
            update_status("0.2.0-rc.1", Some(release.clone())).unwrap(),
            UpdateStatus::Available { .. }
        ));
        assert!(matches!(
            update_status("0.3.0", Some(release)).unwrap(),
            UpdateStatus::Newer { .. }
        ));
        assert_eq!(
            update_status("0.1.0", None).unwrap(),
            UpdateStatus::NoRelease {
                current: Version::new(0, 1, 0)
            }
        );
        assert!(update_status("dev", None).is_err());
        // This crate's own version is a valid semver.
        assert!(env!("CARGO_PKG_VERSION").parse::<Version>().is_ok());
    }

    #[test]
    fn api_responses() {
        assert!(matches!(
            update_status_from_response("0.1.0", 200, RELEASE_JSON).unwrap(),
            UpdateStatus::Available { .. }
        ));
        assert!(matches!(
            update_status_from_response("0.1.0", 404, r#"{"message":"Not Found"}"#).unwrap(),
            UpdateStatus::NoRelease { .. }
        ));
        assert_eq!(
            update_status_from_response("0.1.0", 403, r#"{"message":"API rate limit exceeded"}"#)
                .unwrap_err()
                .to_string(),
            "Update error: GitHub returned HTTP 403: API rate limit exceeded"
        );
        assert_eq!(
            update_status_from_response("0.1.0", 502, "<html>")
                .unwrap_err()
                .to_string(),
            "Update error: GitHub returned HTTP 502"
        );
    }

    #[test]
    #[should_panic(expected = "tests must not use the network")]
    fn network_check_is_forbidden_in_tests() {
        let _ = check_latest_release("0.1.0");
    }
}
