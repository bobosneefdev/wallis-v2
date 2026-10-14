//! Update check against the latest GitHub release: the NSIS installer asset and its SHA-256.
use serde_json::Value;

const LATEST: &str = "https://api.github.com/repos/bobosneefdev/wallis-v2/releases/latest";

pub struct Update { pub version: String, pub url: String, pub sha256: String }

fn parse_version(text: &str) -> Option<[u64; 3]> {
    let mut parts = text.trim_start_matches('v').split('.').map(|part| part.parse().ok());
    Some([parts.next()??, parts.next()??, parts.next()??])
}

/// True when `latest` is a higher version than `current`; unparsable versions never count as newer.
pub fn newer(current: &str, latest: &str) -> bool { matches!((parse_version(current), parse_version(latest)), (Some(c), Some(l)) if l > c) }

pub fn parse_latest(json: &str) -> Result<Update, String> {
    let release: Value = serde_json::from_str(json).map_err(|error| format!("Unreadable release data: {error}"))?;
    let version = release["tag_name"].as_str().ok_or("The release has no version.")?.trim_start_matches('v').to_string();
    let asset = release["assets"].as_array().into_iter().flatten().find(|asset| asset["name"].as_str().is_some_and(|name| name.ends_with("-setup.exe"))).ok_or("The release has no installer.")?;
    let sha256 = asset["digest"].as_str().and_then(|digest| digest.strip_prefix("sha256:")).ok_or("The installer has no checksum.")?.to_string();
    Ok(Update { version, url: asset["browser_download_url"].as_str().ok_or("The installer has no download link.")?.to_string(), sha256 })
}

pub fn latest() -> Result<Update, String> {
    let body = ureq::get(LATEST).header("Accept", "application/vnd.github+json").call().map_err(|error| format!("Unable to check for updates: {error}"))?
        .body_mut().read_to_string().map_err(|error| format!("Unable to read the update info: {error}"))?;
    parse_latest(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_numerically() {
        assert!(newer("0.1.0", "v0.1.1") && newer("0.9.0", "0.10.0") && newer("0.1.0", "1.0.0"));
        assert!(!newer("0.1.0", "0.1.0") && !newer("0.2.0", "0.1.9") && !newer("0.1.0", "nightly"));
    }

    #[test]
    fn picks_the_installer_and_its_checksum() {
        let release = r#"{"tag_name":"v0.2.0","assets":[{"name":"notes.txt"},{"name":"Wallis_0.2.0_x64-setup.exe","digest":"sha256:abc123","browser_download_url":"https://example.test/setup.exe"}]}"#;
        let update = parse_latest(release).unwrap();
        assert_eq!((update.version.as_str(), update.sha256.as_str(), update.url.as_str()), ("0.2.0", "abc123", "https://example.test/setup.exe"));
        assert!(parse_latest(r#"{"tag_name":"v0.2.0","assets":[{"name":"Wallis-setup.exe"}]}"#).is_err());
    }
}
