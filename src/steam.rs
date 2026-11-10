//! Steam library discovery: finds Steam installs, library folders, installed
//! apps, the Steamworks API libraries inside them (`steam_api(64).dll` for
//! Windows games, whether on Windows or under Proton, and `libsteam_api.so`
//! for native Linux games) and the directories holding the game executables
//! (for Koaloader proxy installs).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use serde_json::Value;

use crate::binfmt::{self, Arch};
use crate::paths;
use crate::unlocker::{self, Installed};
use crate::vdf::{Vdf, parse_vdf};

const ICON_CACHE_TTL: u64 = 30 * 24 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Platform {
    Windows,
    Linux,
}

/// A Steamworks API library location inside a game.
#[derive(Debug, Clone)]
pub struct ApiFile {
    pub dir: PathBuf,
    /// File stem as found on disk, e.g. `steam_api64` / `libsteam_api`.
    pub stem: String,
    pub platform: Platform,
    pub arch: Arch,
}

impl ApiFile {
    fn ext(&self) -> &'static str {
        match self.platform {
            Platform::Windows => "dll",
            Platform::Linux => "so",
        }
    }
    pub fn path(&self) -> PathBuf {
        self.dir.join(format!("{}.{}", self.stem, self.ext()))
    }
    /// Where the original library is moved to while an unlocker is installed.
    pub fn orig_path(&self) -> PathBuf {
        self.dir.join(format!("{}_o.{}", self.stem, self.ext()))
    }
}

/// A directory containing Windows game executables (Koaloader target).
#[derive(Debug, Clone, PartialEq)]
pub struct ExeDir {
    pub dir: PathBuf,
    pub arch: Arch,
    /// DLLs statically linked (imported or delay-imported) by the
    /// executables in `dir` and by the game's own DLLs next to them,
    /// lowercased (`version.dll`), mapped to the file names importing them.
    pub imports: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct Game {
    pub appid: u32,
    pub name: String,
    pub dir: PathBuf,
    pub apis: Vec<ApiFile>,
    pub exe_dirs: Vec<ExeDir>,
    pub installed: Vec<Installed>,
    /// Koaloader proxy the game's binaries are seen to load (see
    /// [`unlocker::suggest_proxy`]).
    pub suggested_proxy: Option<unlocker::ProxySuggestion>,
}

/// Where [`fetch_game_icon`] caches a game's icon.
pub fn game_icon_path(appid: u32) -> PathBuf {
    paths::cache_dir()
        .join("game-icons")
        .join(format!("{appid}.ico"))
}

/// Fetch a game's Steam library icon, preferring the local cache.
pub fn fetch_game_icon(appid: u32) -> Result<Vec<u8>, String> {
    let path = game_icon_path(appid);
    let cached = fs::read(&path).ok();
    if let Some(bytes) = &cached {
        if let Ok(modified) = fs::metadata(&path).and_then(|m| m.modified()) {
            if modified
                .elapsed()
                .map(|age| age.as_secs() < ICON_CACHE_TTL)
                .unwrap_or(false)
            {
                return Ok(bytes.clone());
            }
        }
    }

    let clienticon = match ureq::get(&format!("https://api.steamcmd.net/v1/info/{appid}")).call() {
        Ok(mut response) => {
            let info: Value = response
                .body_mut()
                .read_json()
                .map_err(|e| format!("could not read Steam icon metadata: {e}"))?;
            info.get("data")
                .and_then(|data| data.get(appid.to_string()))
                .and_then(|app| app.pointer("/common/clienticon"))
                .and_then(Value::as_str)
                .filter(|icon| !icon.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("Steam did not provide a client icon for app {appid}"))?
        }
        Err(_e) if cached.is_some() => return Ok(cached.unwrap_or_default()),
        Err(e) => return Err(format!("could not fetch Steam icon metadata: {e}")),
    };
    let url = format!(
        "https://cdn.cloudflare.steamstatic.com/steamcommunity/public/images/apps/{appid}/{clienticon}.ico"
    );
    let bytes = match ureq::get(&url).call() {
        Ok(mut response) => response
            .body_mut()
            .read_to_vec()
            .map_err(|e| format!("could not read game icon: {e}"))?,
        Err(_e) if cached.is_some() => return Ok(cached.unwrap_or_default()),
        Err(e) => return Err(format!("could not fetch game icon: {e}")),
    };
    if bytes.is_empty() {
        return Err("Steam returned an empty game icon".into());
    }
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, &bytes);
    Ok(bytes)
}

impl Game {
    /// Platform the installed depot was built for. When libraries for both
    /// platforms exist (leftovers from switching between Proton and native,
    /// e.g. a stray `bin/win_x64`), Windows executables decide.
    pub fn platform(&self) -> Platform {
        let has_linux = self.apis.iter().any(|a| a.platform == Platform::Linux);
        let has_windows = self.apis.iter().any(|a| a.platform == Platform::Windows);
        match (has_windows, has_linux) {
            (true, true) if self.exe_dirs.is_empty() => Platform::Linux,
            (_, false) | (true, true) => Platform::Windows,
            (false, true) => Platform::Linux,
        }
    }

    /// API libraries belonging to the game's platform.
    pub fn platform_apis(&self) -> impl Iterator<Item = &ApiFile> {
        let p = self.platform();
        self.apis.iter().filter(move |a| a.platform == p)
    }
    pub fn refresh_installed(&mut self) {
        self.installed = unlocker::detect(self);
        self.suggested_proxy = unlocker::suggest_proxy(self);
    }
}

/// Where Steam may be installed: the registry and the default location.
#[cfg(windows)]
fn steam_root_candidates() -> Vec<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    let mut out = Vec::new();
    for (hive, key, value) in [
        (HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamPath"),
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Valve\Steam",
            "InstallPath",
        ),
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\Valve\Steam", "InstallPath"),
    ] {
        let path = RegKey::predef(hive)
            .open_subkey(key)
            .and_then(|k| k.get_value::<String, _>(value));
        if let Ok(p) = path {
            out.push(PathBuf::from(p));
        }
    }
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(p) = std::env::var_os(var) {
            out.push(PathBuf::from(p).join("Steam"));
        }
    }
    out
}

/// Where Steam may be installed: native, Flatpak and Snap locations.
#[cfg(not(windows))]
fn steam_root_candidates() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    vec![
        home.join(".steam/steam"),
        home.join(".steam/root"),
        home.join(".local/share/Steam"),
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        home.join(".var/app/com.valvesoftware.Steam/data/Steam"),
        home.join("snap/steam/common/.local/share/Steam"),
    ]
}

/// Steam root directories that actually exist.
fn steam_roots() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for c in steam_root_candidates() {
        // dunce: plain `C:\...` paths on Windows instead of `\\?\C:\...`
        if let Ok(p) = dunce::canonicalize(&c) {
            if p.join("steamapps").is_dir() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// All Steam library folders (each containing a `steamapps` dir).
pub fn library_folders() -> Vec<PathBuf> {
    let mut libs: Vec<PathBuf> = Vec::new();
    let mut add = |p: PathBuf| {
        if let Ok(p) = dunce::canonicalize(&p) {
            if p.join("steamapps").is_dir() && !libs.contains(&p) {
                libs.push(p);
            }
        }
    };
    for root in steam_roots() {
        add(root.clone());
        let Ok(src) = fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) else {
            continue;
        };
        let vdf = parse_vdf(&src);
        let Some(folders) = vdf.get("libraryfolders") else {
            continue;
        };
        for (_, entry) in folders.entries() {
            match entry {
                // new format: "0" { "path" "..." }
                Vdf::Obj(_) => {
                    if let Some(p) = entry.get("path").and_then(Vdf::str) {
                        add(PathBuf::from(p));
                    }
                }
                // old format: "1" "/path"
                Vdf::Str(p) if Path::new(p).is_absolute() => add(PathBuf::from(p)),
                _ => {}
            }
        }
    }
    libs
}

/// Executables that are never the game itself.
fn is_junk_exe(path: &Path) -> bool {
    const NAME: &[&str] = &[
        "unins",
        "crash",
        "redist",
        "setup",
        "install",
        "vc_",
        "vcredist",
        "dxwebsetup",
        "dotnet",
        "easyanticheat",
        "battleye",
        "be_service",
        "update",
        "prereq",
        "cefprocess",
        "report",
        "helper",
        "launcherpatcher",
        "dxsetup",
        "oalinst",
        "physx",
    ];
    const DIR: &[&str] = &[
        "_commonredist",
        "redist",
        "directx",
        "thirdparty",
        "easyanticheat",
        "battleye",
        "prerequisites",
        "vcredist",
        "dotnet",
        "toolchain",
    ];
    let lower = path.to_string_lossy().to_ascii_lowercase();
    let file = path
        .file_name()
        .map(|f| f.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let parent = lower.strip_suffix(&file).unwrap_or(&lower);
    NAME.iter().any(|n| file.contains(n)) || DIR.iter().any(|d| parent.contains(d))
}

/// Number of worker threads for scanning. Directory walks mostly wait on the
/// disk, so more threads than cores would still help a little, but this keeps
/// the load reasonable.
fn scan_threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 16)
}

/// Run `f` over `items` on several threads, keeping the input order.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let threads = scan_threads().min(items.len());
    let mut out: Vec<(usize, R)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut local = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(i) else { break };
                        local.push((i, f(item)));
                    }
                    local
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect()
    });
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, r)| r).collect()
}

/// Whether a file name (lowercased) is one the scan cares about: a Steamworks
/// API library, an unlocker's backup of one, or an executable.
fn is_interesting(lower: &str) -> bool {
    matches!(
        lower,
        "steam_api.dll"
            | "steam_api64.dll"
            | "steam_api_o.dll"
            | "steam_api64_o.dll"
            | "libsteam_api.so"
            | "libsteam_api_o.so"
    ) || lower.ends_with(".exe")
}

/// Walk several directory trees at once on a pool of threads and return, per
/// root, the sorted paths of the files [`is_interesting`] accepts. Symlinks
/// are not followed. All trees share one work queue, so a single huge game
/// is spread over every thread instead of holding up the scan on one.
fn find_interesting(roots: &[&Path]) -> Vec<Vec<PathBuf>> {
    // (pending directories, number of directories being read right now)
    let queue: Mutex<(Vec<(usize, PathBuf)>, usize)> = Mutex::new((
        roots.iter().enumerate().map(|(i, r)| (i, r.to_path_buf())).collect(),
        0,
    ));
    let wake = Condvar::new();

    let worker = || {
        let mut hits: Vec<(usize, PathBuf)> = Vec::new();
        let mut subdirs: Vec<(usize, PathBuf)> = Vec::new();
        loop {
            let (root, dir) = {
                let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if let Some(job) = q.0.pop() {
                        q.1 += 1;
                        break job;
                    }
                    if q.1 == 0 {
                        return hits; // nothing queued, nobody can queue more
                    }
                    q = wake.wait(q).unwrap_or_else(|e| e.into_inner());
                }
            };
            if let Ok(rd) = fs::read_dir(&dir) {
                for entry in rd.flatten() {
                    // file_type() comes from the directory listing: no stat
                    let Ok(ft) = entry.file_type() else { continue };
                    if ft.is_dir() {
                        subdirs.push((root, entry.path()));
                    } else if ft.is_file() {
                        let name = entry.file_name();
                        let lower = name.to_string_lossy().to_ascii_lowercase();
                        if is_interesting(&lower) {
                            hits.push((root, entry.path()));
                        }
                    }
                }
            }
            let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
            q.0.append(&mut subdirs);
            q.1 -= 1;
            drop(q);
            wake.notify_all();
        }
    };

    let mut out: Vec<Vec<PathBuf>> = vec![Vec::new(); roots.len()];
    std::thread::scope(|s| {
        let workers: Vec<_> = (0..scan_threads()).map(|_| s.spawn(worker)).collect();
        for w in workers {
            for (root, path) in w.join().unwrap_or_default() {
                out[root].push(path);
            }
        }
    });
    for v in &mut out {
        v.sort();
    }
    out
}

/// Walk a game directory once and collect API libraries + executable dirs.
pub fn inspect_game_dir(dir: &Path) -> (Vec<ApiFile>, Vec<ExeDir>) {
    let files = find_interesting(&[dir]).pop().unwrap_or_default();
    inspect_files(dir, &files)
}

/// Turn the interesting files found in a game directory into its API
/// libraries and executable dirs.
fn inspect_files(dir: &Path, files: &[PathBuf]) -> (Vec<ApiFile>, Vec<ExeDir>) {
    // key: (dir, lowercase stem) so `x.dll` + `x_o.dll` dedupe
    let mut apis: BTreeMap<(PathBuf, String), ApiFile> = BTreeMap::new();
    let mut exes: Vec<ExeDir> = Vec::new();

    for path in files {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy()) else { continue };
        let lower = name.to_ascii_lowercase();
        let parent = path.parent().unwrap_or(dir).to_path_buf();

        let api = match lower.as_str() {
            "steam_api.dll" | "steam_api64.dll" => {
                Some((&name[..name.len() - 4], Platform::Windows))
            }
            "steam_api_o.dll" | "steam_api64_o.dll" => {
                Some((&name[..name.len() - 6], Platform::Windows))
            }
            "libsteam_api.so" => Some((&name[..name.len() - 3], Platform::Linux)),
            "libsteam_api_o.so" => Some((&name[..name.len() - 5], Platform::Linux)),
            _ => None,
        };
        if let Some((stem, platform)) = api {
            let key = (parent.clone(), stem.to_ascii_lowercase());
            if apis.contains_key(&key) {
                continue;
            }
            // Determine bitness from the original if present (ours may differ in
            // nothing but content, but the original is authoritative).
            let arch = match platform {
                Platform::Windows if stem.to_ascii_lowercase().ends_with("64") => Arch::X64,
                Platform::Windows => Arch::X86,
                Platform::Linux => {
                    let orig = parent.join(format!("{stem}_o.so"));
                    let probe = if orig.is_file() { orig } else { path.clone() };
                    match binfmt::file_arch(&probe) {
                        Some(a) => a,
                        None => continue,
                    }
                }
            };
            apis.insert(
                key,
                ApiFile {
                    dir: parent,
                    stem: stem.to_string(),
                    platform,
                    arch,
                },
            );
            continue;
        }

        if lower.ends_with(".exe") && !is_junk_exe(path) {
            if let Some(arch) = binfmt::file_arch(path) {
                let i = match exes.iter().position(|e| e.dir == parent && e.arch == arch) {
                    Some(i) => i,
                    None => {
                        exes.push(ExeDir { dir: parent, arch, imports: BTreeMap::new() });
                        exes.len() - 1
                    }
                };
                add_imports(&mut exes[i].imports, path);
            }
        }
    }
    for e in &mut exes {
        add_local_dll_imports(e);
    }
    (apis.into_values().collect(), exes)
}

fn add_imports(imports: &mut BTreeMap<String, Vec<String>>, file: &Path) {
    let Some(names) = binfmt::pe_imports(file) else { return };
    let who = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
    for n in names {
        let v = imports.entry(n).or_default();
        if !v.contains(&who) {
            v.push(who.clone());
        }
    }
}

/// The game's own DLLs next to its executables (e.g. `UnityPlayer.dll`,
/// often where the real engine lives behind a tiny launcher exe) resolve
/// their imports from the same folder, so a proxy they import works too.
/// Unlocker DLLs (Koaloader proxies, SmokeAPI, steam_api) are skipped so an
/// existing install doesn't influence the result.
fn add_local_dll_imports(e: &mut ExeDir) {
    let Ok(rd) = fs::read_dir(&e.dir) else { return };
    let mut dlls: Vec<PathBuf> = rd
        .flatten()
        .map(|d| d.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_ascii_lowercase();
            let Some(stem) = name.strip_suffix(".dll") else { return false };
            !stem.starts_with("steam_api")
                && !stem.starts_with("smokeapi")
                && !stem.starts_with("smoke_api")
                && !unlocker::KOALOADER_PROXIES.contains(&stem)
        })
        .collect();
    dlls.sort();
    for d in dlls {
        add_imports(&mut e.imports, &d);
    }
}

/// Scan every library for installed apps that ship a Steamworks API library.
pub fn scan_games() -> Result<Vec<Game>, String> {
    let libs = library_folders();
    if libs.is_empty() {
        return Err("No Steam installation found".into());
    }
    // Installed apps from the manifests: (appid, name, install dir).
    let mut apps: Vec<(u32, String, PathBuf)> = Vec::new();
    for lib in libs {
        let steamapps = lib.join("steamapps");
        let Ok(rd) = fs::read_dir(&steamapps) else {
            continue;
        };
        for entry in rd.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            if !(fname.starts_with("appmanifest_") && fname.ends_with(".acf")) {
                continue;
            }
            let Ok(src) = fs::read_to_string(entry.path()) else {
                continue;
            };
            let acf = parse_vdf(&src);
            let Some(state) = acf.get("AppState") else {
                continue;
            };
            let appid = state
                .get("appid")
                .and_then(Vdf::str)
                .and_then(|s| s.parse().ok());
            let name = state.get("name").and_then(Vdf::str);
            let installdir = state.get("installdir").and_then(Vdf::str);
            let (Some(appid), Some(name), Some(installdir)) = (appid, name, installdir) else {
                continue;
            };
            if apps.iter().any(|a| a.0 == appid) {
                continue;
            }
            let dir = steamapps.join("common").join(installdir);
            // Proton, Steam Linux Runtime etc. are tools, not games.
            if !dir.is_dir() || dir.join("toolmanifest.vdf").is_file() {
                continue;
            }
            apps.push((appid, name.to_string(), dir));
        }
    }

    // Walk every game directory in one parallel pass, then read the binaries
    // found in each game on several threads as well.
    let roots: Vec<&Path> = apps.iter().map(|a| a.2.as_path()).collect();
    let found = find_interesting(&roots);
    let jobs: Vec<(&(u32, String, PathBuf), &Vec<PathBuf>)> = apps.iter().zip(&found).collect();
    let mut list: Vec<Game> = par_map(&jobs, |((appid, name, dir), files)| {
        let (apis, exe_dirs) = inspect_files(dir, files);
        if apis.is_empty() {
            return None; // tool / runtime / no Steamworks library
        }
        let mut game = Game {
            appid: *appid,
            name: name.clone(),
            dir: dir.clone(),
            apis,
            exe_dirs,
            installed: Vec::new(),
            suggested_proxy: None,
        };
        game.refresh_installed();
        Some(game)
    })
    .into_iter()
    .flatten()
    .collect();
    list.sort_by_key(|g| (g.name.to_lowercase(), g.appid));
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junk_exes() {
        assert!(is_junk_exe(Path::new("/g/_CommonRedist/vcredist_x64.exe")));
        assert!(is_junk_exe(Path::new("/g/UnityCrashHandler64.exe")));
        assert!(!is_junk_exe(Path::new(
            "/g/Binaries/Win64/Game-Win64-Shipping.exe"
        )));
    }

    /// Scans the real Steam install: `cargo test -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn scan_local_steam() {
        println!("libraries: {:?}", library_folders());
        for g in scan_games().unwrap() {
            println!(
                "{} {} {:?} apis={} exe_dirs={} installed={:?}",
                g.appid,
                g.name,
                g.platform(),
                g.apis.len(),
                g.exe_dirs.len(),
                g.installed
            );
            println!("    suggested proxy: {:?}", g.suggested_proxy);
        }
    }
}
