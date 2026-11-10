//! Fetches a game's DLC list (IDs + names) from public Steam endpoints.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::paths;

/// How long a cached DLC list is considered fresh.
const CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

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

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn cache_path(appid: u32) -> PathBuf {
    paths::cache_dir().join("dlc").join(format!("{appid}.json"))
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
    let file = CacheFile { fetched: now(), dlcs: dlcs.to_vec() };
    if let Ok(json) = serde_json::to_vec(&file) {
        let _ = fs::write(p, json);
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
    let disabled = read_selections().remove(&appid.to_string()).unwrap_or_default();
    for d in &mut dlcs {
        d.enabled = !disabled.contains(&d.id);
    }
    Ok((dlcs, source))
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .user_agent(paths::APP_NAME)
        .build()
}

fn get_json(agent: &ureq::Agent, url: &str) -> Option<Value> {
    agent.get(url).call().ok()?.into_json().ok()
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
                            let v = get_json(&agent, &format!("https://api.steamcmd.net/v1/info/{id}"))?;
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

#[cfg(test)]
mod tests {
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
