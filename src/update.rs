//! Updates from GitHub releases (REQ-023). Each release carries one binary
//! per platform and SHA256SUMS; an update downloads this platform's binary,
//! checks its digest, and installs it. The decisions here are pure; the
//! downloads (system curl) and the install are in `fetch`.
use serde::Deserialize;
use thiserror::Error;

pub mod fetch;

/// The latest release, as the GitHub API returns it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    UpToDate {
        version: String,
    },
    Update {
        version: String,
        binary: Asset,
        sums: Asset,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UpdateError {
    #[error("The latest release tag {0} is not a version")]
    BadTag(String),
    #[error("Release {version} has no binary for this platform ({asset}); build it from source")]
    NoBinary { version: String, asset: String },
    #[error("Release {0} has no SHA256SUMS; not installing an unchecked binary")]
    NoChecksums(String),
    #[error("No prebuilt binary exists for {os} {arch}")]
    UnsupportedPlatform { os: String, arch: String },
}

/// `major.minor.patch` of a version or tag (`v0.3.0`).
pub fn version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let parsed = (next()?, next()?, next()?);
    parts.next().is_none().then_some(parsed)
}

/// The release asset name for a platform (`std::env::consts` values).
pub fn asset_name(os: &str, arch: &str) -> Option<String> {
    let target = match (os, arch) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc.exe",
        _ => return None,
    };
    Some(format!("session-bridge-{target}"))
}

/// The SHA-256 digest listed for `name` in a SHA256SUMS file.
pub fn checksum_for(sums: &str, name: &str) -> Option<[u8; 32]> {
    sums.lines().find_map(|line| {
        let (hex, file) = line.split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        if file != name {
            return None;
        }
        let bytes = data_encoding::HEXLOWER_PERMISSIVE
            .decode(hex.as_bytes())
            .ok()?;
        bytes.try_into().ok()
    })
}

/// What to do about `release` when running `current` on `os`/`arch`.
pub fn plan(current: &str, release: &Release, os: &str, arch: &str) -> Result<Plan, UpdateError> {
    let latest =
        version(&release.tag_name).ok_or_else(|| UpdateError::BadTag(release.tag_name.clone()))?;
    let name = asset_name(os, arch).ok_or_else(|| UpdateError::UnsupportedPlatform {
        os: os.into(),
        arch: arch.into(),
    })?;
    let label = format!("{}.{}.{}", latest.0, latest.1, latest.2);
    if version(current).is_some_and(|running| running >= latest) {
        return Ok(Plan::UpToDate {
            version: current.into(),
        });
    }
    let find = |wanted: &str| release.assets.iter().find(|a| a.name == wanted).cloned();
    let binary = find(&name).ok_or_else(|| UpdateError::NoBinary {
        version: label.clone(),
        asset: name.clone(),
    })?;
    let sums = find("SHA256SUMS").ok_or_else(|| UpdateError::NoChecksums(label.clone()))?;
    Ok(Plan::Update {
        version: label,
        binary,
        sums,
    })
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
