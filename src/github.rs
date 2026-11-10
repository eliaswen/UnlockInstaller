//! Downloads unlocker releases from their official GitHub repositories.

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use crate::archive;
use crate::paths;
use crate::unlocker::Kind;

/// This application's own repository, used to check for updates.
pub const APP_REPO: &str = "eliaswen/UnlockInstaller";

/// Largest asset we are willing to download.
const MAX_ASSET_BYTES: u64 = 200 * 1024 * 1024;

/// Official repository for an unlocker, if it is published on GitHub.
pub fn repo(k: Kind) -> Option<&'static str> {
    match k {
        Kind::SmokeApi => Some("acidicoala/SmokeAPI"),
        Kind::Koaloader => Some("acidicoala/Koaloader"),
        Kind::CreamApi => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct Release {
    pub tag: String,
    pub asset: Asset,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .user_agent(paths::APP_NAME)
        .build()
}

/// Pick the release archive among a release's assets: archives only,
/// preferring ones named after the project, then zip files.
fn pick_asset(project: &str, assets: &[Asset]) -> Option<Asset> {
    let project = project.to_ascii_lowercase();
    assets
        .iter()
        .filter(|a| archive::has_archive_ext(&a.name))
        .filter(|a| {
            let n = a.name.to_ascii_lowercase();
            !(n.contains("source") || n.contains("debug") || n.contains("symbols") || n.contains("pdb"))
        })
        .max_by_key(|a| {
            let n = a.name.to_ascii_lowercase();
            (n.contains(&project) as u8) * 2 + n.ends_with(".zip") as u8
        })
        .cloned()
}

/// Look up the latest release of `repo` ("owner/name").
pub fn latest_release(repo: &str) -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let json: Value = agent()
        .get(&url)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(403 | 429, _) => {
                "GitHub API rate limit reached; try again later".to_string()
            }
            e => format!("Cannot reach GitHub: {e}"),
        })?
        .into_json()
        .map_err(|e| format!("Bad response from GitHub: {e}"))?;
    let tag = json["tag_name"].as_str().unwrap_or("latest").to_string();
    let assets: Vec<Asset> = json["assets"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some(Asset {
                        name: x["name"].as_str()?.to_string(),
                        url: x["browser_download_url"].as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let project = repo.rsplit('/').next().unwrap_or(repo);
    let asset = pick_asset(project, &assets).ok_or_else(|| {
        let names: Vec<&str> = assets.iter().map(|a| a.name.as_str()).collect();
        format!("Release {tag} has no archive asset (assets: {})", names.join(", "))
    })?;
    Ok(Release { tag, asset })
}

/// Where a user can get the newest release of this application.
pub fn app_release_url() -> String {
    format!("https://github.com/{APP_REPO}/releases/latest")
}

/// Numeric parts of a version such as `1.2.3` or `v1.2.3`.
fn parse_version(s: &str) -> Option<Vec<u64>> {
    let s = s.trim();
    let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
    s.split('.').map(|p| p.parse().ok()).collect()
}

fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// The version of the newest published release of this application, if it
/// is newer than the running one. Nothing is downloaded.
pub fn check_update() -> Result<Option<String>, String> {
    let url = format!("https://api.github.com/repos/{APP_REPO}/releases/latest");
    let resp = match agent().get(&url).set("Accept", "application/vnd.github+json").call() {
        Ok(r) => r,
        // no release has been published yet
        Err(ureq::Error::Status(404, _)) => return Ok(None),
        Err(ureq::Error::Status(403 | 429, _)) => {
            return Err("GitHub API rate limit reached; try again later".into());
        }
        Err(e) => return Err(format!("Cannot reach GitHub: {e}")),
    };
    let json: Value = resp.into_json().map_err(|e| format!("Bad response from GitHub: {e}"))?;
    let tag = json["tag_name"].as_str().unwrap_or_default();
    let version = tag.trim().trim_start_matches(['v', 'V']);
    Ok(is_newer(version, env!("CARGO_PKG_VERSION")).then(|| version.to_string()))
}

/// Download the latest release archive of `k` into the cache directory.
/// Returns the release tag and the downloaded file's path.
pub fn download_latest(k: Kind) -> Result<(String, PathBuf), String> {
    let repo = repo(k).ok_or_else(|| format!("{k} has no official GitHub releases"))?;
    let release = latest_release(repo)?;
    let resp = agent()
        .get(&release.asset.url)
        .call()
        .map_err(|e| format!("Download failed: {e}"))?;
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(MAX_ASSET_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Download failed: {e}"))?;
    if bytes.len() as u64 > MAX_ASSET_BYTES {
        return Err("Release asset is unexpectedly large; refusing to download".into());
    }
    let dir = paths::cache_dir().join("downloads");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Asset names come from the network: keep only the final path component.
    let file_name = release
        .asset
        .name
        .rsplit(['/', '\\'])
        .next()
        .filter(|n| !n.is_empty() && *n != "..")
        .unwrap_or("release.zip")
        .to_string();
    let path = dir.join(file_name);
    fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((release.tag, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(n: &str) -> Asset {
        Asset { name: n.into(), url: format!("https://x/{n}") }
    }

    #[test]
    fn picks_project_archive() {
        let assets = [a("checksums.txt"), a("Source.zip"), a("other.7z"), a("SmokeAPI-v4.1.3.zip")];
        assert_eq!(pick_asset("SmokeAPI", &assets).unwrap().name, "SmokeAPI-v4.1.3.zip");
        let assets = [a("koaloader.tar.gz"), a("readme.md")];
        assert_eq!(pick_asset("Koaloader", &assets).unwrap().name, "koaloader.tar.gz");
        assert!(pick_asset("SmokeAPI", &[a("readme.md")]).is_none());
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("v1.0.0", "0.9.12"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("nightly", "0.1.0"));
    }

    /// Hits the network: `cargo test -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn update_check_reaches_github() {
        println!("update: {:?}", check_update().unwrap());
    }

    /// Hits the network: `cargo test -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn latest_releases_have_archives() {
        for k in [Kind::SmokeApi, Kind::Koaloader] {
            let r = latest_release(repo(k).unwrap()).unwrap();
            println!("{k}: {} -> {}", r.tag, r.asset.name);
        }
    }

    /// Downloads and imports the real releases. On Linux, run with
    /// XDG_DATA_HOME and XDG_CACHE_HOME pointing somewhere disposable:
    /// `cargo test download_and_import -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn download_and_import() {
        use crate::unlocker;
        for k in [Kind::SmokeApi, Kind::Koaloader] {
            let (tag, path) = download_latest(k).unwrap();
            let msg = unlocker::import_archive(k, &path, None).unwrap();
            unlocker::set_source(k, &tag);
            println!("{msg}\n  -> {:?}", unlocker::import_summary(k));
        }
        println!("proxies: {:?}", unlocker::koaloader_proxies());
    }
}
