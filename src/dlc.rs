//! Fetches a game's DLC list (IDs + names) from public Steam endpoints.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::paths;
use crate::steam;
use crate::vdf::parse_vdf;

/// How long a cached DLC list is considered fresh.
const CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadStatus {
    Required,
    Unlikely,
    NotRequired,
    AlreadyDone,
    Unknown,
}

impl DownloadStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Required => "Download required",
            Self::Unlikely => "Download unlikely to be required",
            Self::NotRequired => "Download not required",
            Self::AlreadyDone => "Download already done",
            Self::Unknown => "Not enough information",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dlc {
    pub id: u32,
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    fetched: u64,
    dlcs: Vec<Dlc>,
}

#[derive(Serialize, Deserialize)]
struct RequirementCache {
    fetched: u64,
    status: DownloadStatus,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_path(appid: u32) -> PathBuf {
    paths::cache_dir().join("dlc").join(format!("{appid}.json"))
}

fn requirement_cache_path(appid: u32, dlc_id: u32) -> PathBuf {
    paths::cache_dir()
        .join("dlc-requirements")
        .join(format!("{appid}-{dlc_id}.json"))
}

fn selections_path() -> PathBuf {
    paths::config_dir().join("selections.json")
}

/// appid -> DLC IDs the user disabled
type Selections = BTreeMap<String, BTreeSet<u32>>;

fn read_selections() -> Selections {
    fs::read(selections_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Remember which DLCs are disabled for `appid`.
pub fn save_selection(appid: u32, dlcs: &[Dlc]) {
    let mut all = read_selections();
    let disabled: BTreeSet<u32> = dlcs.iter().filter(|d| !d.enabled).map(|d| d.id).collect();
    if disabled.is_empty() {
        all.remove(&appid.to_string());
    } else {
        all.insert(appid.to_string(), disabled);
    }
    let p = selections_path();
    if let Some(dir) = p.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(&all) {
        let _ = fs::write(p, json);
    }
}

fn read_cache(appid: u32) -> Option<CacheFile> {
    serde_json::from_slice(&fs::read(cache_path(appid)).ok()?).ok()
}

fn write_cache(appid: u32, dlcs: &[Dlc]) {
    let p = cache_path(appid);
    if let Some(dir) = p.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let file = CacheFile {
        fetched: now(),
        dlcs: dlcs.to_vec(),
    };
    if let Ok(json) = serde_json::to_vec(&file) {
        let _ = fs::write(p, json);
    }
}

fn read_requirement_cache(appid: u32, dlc_id: u32) -> Option<(u64, DownloadStatus)> {
    let file: RequirementCache =
        serde_json::from_slice(&fs::read(requirement_cache_path(appid, dlc_id)).ok()?).ok()?;
    Some((file.fetched, file.status))
}

fn write_requirement_cache(appid: u32, dlc_id: u32, status: DownloadStatus) {
    let path = requirement_cache_path(appid, dlc_id);
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let file = RequirementCache {
        fetched: now(),
        status,
    };
    if let Ok(json) = serde_json::to_vec(&file) {
        let _ = fs::write(path, json);
    }
}

/// DLC list for `appid`, from the cache when fresh (unless `refresh`),
/// otherwise from Steam. Falls back to a stale cache when Steam is
/// unreachable. Saved selections are applied to the result.
pub fn load_dlcs(appid: u32, refresh: bool) -> Result<(Vec<Dlc>, &'static str), String> {
    let cached = read_cache(appid);
    let fresh = cached
        .as_ref()
        .is_some_and(|c| now().saturating_sub(c.fetched) < CACHE_TTL.as_secs());
    let (mut dlcs, source) = match cached {
        Some(c) if fresh && !refresh => (c.dlcs, "cache"),
        cached => match fetch_dlcs(appid) {
            Ok(d) => {
                write_cache(appid, &d);
                (d, "Steam")
            }
            Err(e) => match cached {
                Some(c) => (c.dlcs, "stale cache (Steam unreachable)"),
                None => return Err(e),
            },
        },
    };
    let disabled = read_selections()
        .remove(&appid.to_string())
        .unwrap_or_default();
    for d in &mut dlcs {
        d.enabled = !disabled.contains(&d.id);
    }
    Ok((dlcs, source))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .user_agent(paths::APP_NAME)
        .build()
        .into()
}

fn get_json(agent: &ureq::Agent, url: &str) -> Option<Value> {
    agent.get(url).call().ok()?.body_mut().read_json().ok()
}

fn as_u32(v: &Value) -> Option<u32> {
    v.as_u64()
        .map(|n| n as u32)
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// Gather DLC IDs (with names when available) for `appid`.
pub fn fetch_dlcs(appid: u32) -> Result<Vec<Dlc>, String> {
    let agent = agent();
    // id -> name (None until resolved)
    let mut dlcs: BTreeMap<u32, Option<String>> = BTreeMap::new();
    let mut any_source_ok = false;

    // 1) SteamCMD info: extended.listofdlc + depots[*].dlcappid
    if let Some(info) = get_json(&agent, &format!("https://api.steamcmd.net/v1/info/{appid}")) {
        if let Some(app) = info.get("data").and_then(|d| d.get(appid.to_string())) {
            any_source_ok = true;
            if let Some(list) = app.pointer("/extended/listofdlc").and_then(Value::as_str) {
                for id in list.split(',').filter_map(|s| s.trim().parse().ok()) {
                    dlcs.entry(id).or_insert(None);
                }
            }
            if let Some(depots) = app.get("depots").and_then(Value::as_object) {
                for depot in depots.values() {
                    if let Some(id) = depot.get("dlcappid").and_then(as_u32) {
                        dlcs.entry(id).or_insert(None);
                    }
                }
            }
        }
    }

    // 2) Store appdetails: data.dlc = [ids]
    let url = format!("https://store.steampowered.com/api/appdetails?appids={appid}&l=english");
    if let Some(details) = get_json(&agent, &url) {
        if let Some(data) = details.get(appid.to_string()).and_then(|a| a.get("data")) {
            any_source_ok = true;
            if let Some(list) = data.get("dlc").and_then(Value::as_array) {
                for id in list.iter().filter_map(as_u32) {
                    dlcs.entry(id).or_insert(None);
                }
            }
        }
    }

    if !any_source_ok {
        return Err("Could not reach Steam (SteamCMD API and Store both failed)".into());
    }

    // 3) Resolve names in parallel via SteamCMD info per DLC.
    let missing: Vec<u32> = dlcs
        .iter()
        .filter(|(_, n)| n.is_none())
        .map(|(id, _)| *id)
        .collect();
    let resolved = resolve_names(&agent, &missing);
    for (id, name) in resolved {
        dlcs.insert(id, Some(name));
    }

    let mut out: Vec<Dlc> = dlcs
        .into_iter()
        .map(|(id, name)| Dlc {
            id,
            name: name.unwrap_or_else(|| format!("Unknown DLC {id}")),
            enabled: true,
        })
        .collect();
    out.sort_by_key(|d| d.name.to_lowercase());
    Ok(out)
}

fn resolve_names(agent: &ureq::Agent, ids: &[u32]) -> Vec<(u32, String)> {
    const WORKERS: usize = 8;
    if ids.is_empty() {
        return Vec::new();
    }
    let chunk = ids.len().div_ceil(WORKERS);
    std::thread::scope(|s| {
        let handles: Vec<_> = ids
            .chunks(chunk)
            .map(|part| {
                let agent = agent.clone();
                s.spawn(move || {
                    part.iter()
                        .filter_map(|&id| {
                            let v = get_json(
                                &agent,
                                &format!("https://api.steamcmd.net/v1/info/{id}"),
                            )?;
                            let name = v
                                .get("data")?
                                .get(id.to_string())?
                                .pointer("/common/name")?
                                .as_str()?
                                .to_string();
                            Some((id, name))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    })
}

fn installed_depots() -> BTreeSet<u32> {
    let mut installed = BTreeSet::new();
    for library in steam::library_folders() {
        let steamapps = library.join("steamapps");
        let Ok(entries) = fs::read_dir(steamapps) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
                continue;
            }
            let Ok(source) = fs::read_to_string(entry.path()) else {
                continue;
            };
            let app = parse_vdf(&source);
            let Some(depots) = app
                .get("appstate")
                .and_then(|state| state.get("installeddepots"))
            else {
                continue;
            };
            for (id, _) in depots.entries() {
                if let Ok(id) = id.parse() {
                    installed.insert(id);
                }
            }
        }
    }
    installed
}

fn requirement_from_info(
    info: &Value,
    dlc_id: u32,
    installed: &BTreeSet<u32>,
) -> Option<DownloadStatus> {
    let app = info.get("data")?.as_object()?.values().next()?;
    let depots = app.get("depots")?.as_object()?;
    let mut depot_ids = Vec::new();
    let mut has_size = false;
    let mut has_download = false;
    let mut matched = false;
    for (id, depot) in depots {
        let Ok(id) = id.parse::<u32>() else { continue };
        let Some(depot_dlc_id) = depot.get("dlcappid").and_then(as_u32) else {
            continue;
        };
        if depot_dlc_id != dlc_id {
            continue;
        }
        matched = true;
        depot_ids.push(id);
        if let Some(manifests) = depot.get("manifests").and_then(Value::as_object) {
            if let Some(public) = manifests.get("public") {
                if let Some(size) = public.get("size").and_then(as_u32) {
                    has_size = true;
                    has_download |= public
                        .get("download")
                        .and_then(as_u32)
                        .is_some_and(|download| download > 0);
                    has_download |= size > 0;
                }
            }
        }
    }
    if !matched {
        return Some(DownloadStatus::NotRequired);
    }
    if depot_ids.iter().all(|id| installed.contains(id)) {
        return Some(DownloadStatus::AlreadyDone);
    }
    if has_download {
        Some(DownloadStatus::Required)
    } else if has_size {
        Some(DownloadStatus::Unlikely)
    } else {
        Some(DownloadStatus::Unknown)
    }
}

/// Check whether each DLC appears to add files. This is intentionally
/// separate from [`load_dlcs`] because it performs one SteamCMD request per
/// DLC unless a recent local result is available.
pub fn check_downloads(appid: u32, dlcs: &[Dlc]) -> BTreeMap<u32, DownloadStatus> {
    let installed = installed_depots();
    let agent = agent();
    let mut parent_info = None;
    let mut parent_info_attempted = false;
    let mut result = BTreeMap::new();
    for dlc in dlcs {
        if let Some((fetched, status)) = read_requirement_cache(appid, dlc.id) {
            if status != DownloadStatus::Unknown
                && now().saturating_sub(fetched) < CACHE_TTL.as_secs()
            {
                result.insert(dlc.id, status);
                continue;
            }
        }
        if !parent_info_attempted {
            parent_info_attempted = true;
            parent_info = get_json(&agent, &format!("https://api.steamcmd.net/v1/info/{appid}"));
        }
        let status = parent_info
            .as_ref()
            .and_then(|info| requirement_from_info(info, dlc.id, &installed))
            .unwrap_or(DownloadStatus::Unknown);
        write_requirement_cache(appid, dlc.id, status);
        result.insert(dlc.id, status);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: u32, depots: Value) -> Value {
        serde_json::json!({ "data": { id.to_string(): { "depots": depots } } })
    }

    #[test]
    fn classifies_dlc_download_status() {
        assert_eq!(
            requirement_from_info(&info(1, serde_json::json!({})), 1, &BTreeSet::new()),
            Some(DownloadStatus::NotRequired)
        );
        assert_eq!(
            requirement_from_info(
                &info(
                    1,
                    serde_json::json!({
                        "10": { "dlcappid": 1, "manifests": { "public": { "size": 123, "download": 123 } } }
                    })
                ),
                1,
                &BTreeSet::new()
            ),
            Some(DownloadStatus::Required)
        );
        assert_eq!(
            requirement_from_info(
                &info(
                    1,
                    serde_json::json!({
                        "10": { "dlcappid": 1, "manifests": { "public": { "size": 0, "download": 0 } } }
                    })
                ),
                1,
                &BTreeSet::new()
            ),
            Some(DownloadStatus::Unlikely)
        );
        assert_eq!(
            requirement_from_info(
                &info(
                    1,
                    serde_json::json!({
                        "10": { "dlcappid": 1, "manifests": { "public": { "size": 123, "download": 123 } } }
                    })
                ),
                1,
                &BTreeSet::from([10])
            ),
            Some(DownloadStatus::AlreadyDone)
        );
        assert_eq!(
            requirement_from_info(
                &info(
                    1,
                    serde_json::json!({
                        "10": { "dlcappid": 1, "manifests": { "public": {} } }
                    })
                ),
                1,
                &BTreeSet::new()
            ),
            Some(DownloadStatus::Unknown)
        );
    }

    /// Hits the network: `cargo test -- --ignored`
    #[test]
    #[ignore]
    fn fetches_portal2_dlcs() {
        let dlcs = super::fetch_dlcs(620).unwrap();
        for d in &dlcs {
            println!("{} {}", d.id, d.name);
        }
        assert!(!dlcs.is_empty());
    }
}
