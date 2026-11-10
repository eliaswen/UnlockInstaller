//! Steam library discovery: finds Steam installs, library folders, installed
//! apps, the Steamworks API libraries inside them (`steam_api(64).dll` for
//! Windows games, whether on Windows or under Proton, and `libsteam_api.so`
//! for native Linux games) and the directories holding the game executables
//! (for Koaloader proxy installs).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::binfmt::{self, Arch};
use crate::unlocker::{self, Installed};
use crate::vdf::{parse_vdf, Vdf};

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
}

#[derive(Debug, Clone)]
pub struct Game {
    pub appid: u32,
    pub name: String,
    pub dir: PathBuf,
    pub apis: Vec<ApiFile>,
    pub exe_dirs: Vec<ExeDir>,
    pub installed: Vec<Installed>,
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
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\WOW6432Node\Valve\Steam", "InstallPath"),
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
        "unins", "crash", "redist", "setup", "install", "vc_", "vcredist", "dxwebsetup", "dotnet",
        "easyanticheat", "battleye", "be_service", "update", "prereq", "cefprocess", "report",
        "helper", "launcherpatcher", "dxsetup", "oalinst", "physx",
    ];
    const DIR: &[&str] = &[
        "_commonredist", "redist", "directx", "thirdparty", "easyanticheat", "battleye",
        "prerequisites", "vcredist", "dotnet", "toolchain",
    ];
    let lower = path.to_string_lossy().to_ascii_lowercase();
    let file = path
        .file_name()
        .map(|f| f.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let parent = lower.strip_suffix(&file).unwrap_or(&lower);
    NAME.iter().any(|n| file.contains(n)) || DIR.iter().any(|d| parent.contains(d))
}

/// Walk a game directory once and collect API libraries + executable dirs.
pub fn inspect_game_dir(dir: &Path) -> (Vec<ApiFile>, Vec<ExeDir>) {
    // key: (dir, lowercase stem) so `x.dll` + `x_o.dll` dedupe
    let mut apis: BTreeMap<(PathBuf, String), ApiFile> = BTreeMap::new();
    let mut exes: Vec<ExeDir> = Vec::new();

    for entry in WalkDir::new(dir).follow_links(false).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        let lower = name.to_ascii_lowercase();
        let parent = entry.path().parent().unwrap_or(dir).to_path_buf();

        let api = match lower.as_str() {
            "steam_api.dll" | "steam_api64.dll" => Some((&name[..name.len() - 4], Platform::Windows)),
            "steam_api_o.dll" | "steam_api64_o.dll" => Some((&name[..name.len() - 6], Platform::Windows)),
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
                    let probe = if orig.is_file() { orig } else { entry.path().to_path_buf() };
                    match binfmt::file_arch(&probe) {
                        Some(a) => a,
                        None => continue,
                    }
                }
            };
            apis.insert(key, ApiFile { dir: parent, stem: stem.to_string(), platform, arch });
            continue;
        }

        if lower.ends_with(".exe") && !is_junk_exe(entry.path()) {
            if let Some(arch) = binfmt::file_arch(entry.path()) {
                let e = ExeDir { dir: parent, arch };
                if !exes.contains(&e) {
                    exes.push(e);
                }
            }
        }
    }
    (apis.into_values().collect(), exes)
}

/// Scan every library for installed apps that ship a Steamworks API library.
pub fn scan_games() -> Result<Vec<Game>, String> {
    let libs = library_folders();
    if libs.is_empty() {
        return Err("No Steam installation found".into());
    }
    let mut games: BTreeMap<u32, Game> = BTreeMap::new();
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
            let appid = state.get("appid").and_then(Vdf::str).and_then(|s| s.parse().ok());
            let name = state.get("name").and_then(Vdf::str);
            let installdir = state.get("installdir").and_then(Vdf::str);
            let (Some(appid), Some(name), Some(installdir)) = (appid, name, installdir) else {
                continue;
            };
            if games.contains_key(&appid) {
                continue;
            }
            let dir = steamapps.join("common").join(installdir);
            // Proton, Steam Linux Runtime etc. are tools, not games.
            if !dir.is_dir() || dir.join("toolmanifest.vdf").is_file() {
                continue;
            }
            let (apis, exe_dirs) = inspect_game_dir(&dir);
            if apis.is_empty() {
                continue; // tool / runtime / no Steamworks library
            }
            let mut game = Game {
                appid,
                name: name.to_string(),
                dir,
                apis,
                exe_dirs,
                installed: Vec::new(),
            };
            game.refresh_installed();
            games.insert(appid, game);
        }
    }
    let mut list: Vec<Game> = games.into_values().collect();
    list.sort_by_key(|g| g.name.to_lowercase());
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junk_exes() {
        assert!(is_junk_exe(Path::new("/g/_CommonRedist/vcredist_x64.exe")));
        assert!(is_junk_exe(Path::new("/g/UnityCrashHandler64.exe")));
        assert!(!is_junk_exe(Path::new("/g/Binaries/Win64/Game-Win64-Shipping.exe")));
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
        }
    }
}
