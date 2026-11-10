//! Importing user-supplied unlocker zips, detecting existing installs and
//! installing/uninstalling unlockers.
//!
//! No unlocker binaries ship with this program: the user imports the release
//! zips and the files we need are extracted into
//! `<data dir>/<SmokeAPI|CreamAPI|Koaloader>/` (see [`crate::paths`]).
//!
//! Install methods:
//! - **Direct** (SmokeAPI "proxy mode" / classic CreamAPI): the original
//!   `steam_api(64).dll` / `libsteam_api.so` is renamed to `*_o.*` and the
//!   unlocker takes its place. Works for Windows games (on Windows or under
//!   Proton) and native Linux games.
//! - **Koaloader** (SmokeAPI "hook mode"): a Koaloader proxy DLL such as
//!   `version.dll` plus `SmokeAPI32/64.dll` are placed next to the game
//!   executable(s). On Windows that is all; under Proton the game's launch
//!   options must also contain `WINEDLLOVERRIDES="<proxy>=n,b" %command%`.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::archive;
use crate::binfmt::{self, Arch, Format};
use crate::dlc::Dlc;
use crate::paths;
use crate::steam::{ApiFile, Game, Platform};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    SmokeApi,
    CreamApi,
    Koaloader,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::SmokeApi => "SmokeAPI",
            Kind::CreamApi => "CreamAPI",
            Kind::Koaloader => "Koaloader",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Method {
    Direct,
    Koaloader,
}

/// An unlocker install found on disk.
#[derive(Debug, Clone, Serialize)]
pub struct Installed {
    pub method: Method,
    /// "SmokeAPI", "CreamAPI" or "Unknown"
    pub unlocker: String,
    pub dir: PathBuf,
    /// Koaloader proxy name (e.g. `version`) for Koaloader installs.
    pub proxy: Option<String>,
}

/// Proxy DLLs Koaloader ships (without `.dll`).
pub const KOALOADER_PROXIES: &[&str] = &[
    "version", "winmm", "winhttp", "dinput8", "dxgi", "d3d11", "d3d10", "d3d9", "dwmapi",
    "audioses", "glu32", "hid", "iphlpapi", "msasn1", "msimg32", "mswsock", "opengl32", "profapi",
    "propsys", "textshaping", "wldp", "xinput9_1_0",
];

const CREAM_CONFIG: &str = "cream_api.ini";
const SMOKE_CONFIG: &str = "SmokeAPI.config.json";
const SMOKE_TEMPLATE: &str = "config_template.json";
const SOURCE_FILE: &str = "source.txt";
/// Names Koaloader auto-loads; we write the `SmokeAPI32/64.dll` form, which
/// every Koaloader version recognises, but detect/remove all of them.
const SMOKE_KOALOADER_NAMES: &[&str] = &[
    "SmokeAPI32.dll", "SmokeAPI64.dll", "SmokeAPI.dll", "smoke_api32.dll", "smoke_api64.dll",
    "smoke_api.dll",
];
/// Files an unlocker may leave next to its DLL; removed on uninstall.
const EXTRA_FILES: &[&str] = &[
    CREAM_CONFIG, SMOKE_CONFIG, "SmokeAPI.log.log", "SmokeAPI.log", "Koaloader.config.json",
    "Koaloader.log", "Koaloader.log.log",
];
/// Passwords commonly used for unlocker zips; tried before asking the user.
const KNOWN_PASSWORDS: &[&str] = &["cs.rin.ru", "csrin.ru", "csrin"];

fn store_dir(k: Kind) -> PathBuf {
    paths::data_dir().join(k.name())
}

/// Stored file for an unlocker library (SmokeAPI / CreamAPI).
fn lib_file(k: Kind, platform: Platform, arch: Arch) -> PathBuf {
    let name = match platform {
        Platform::Windows => format!("win-{}.dll", arch.bits()),
        Platform::Linux => format!("linux-{}.so", arch.bits()),
    };
    store_dir(k).join(name)
}

/// Stored file for a Koaloader proxy DLL.
fn proxy_file(proxy: &str, arch: Arch) -> PathBuf {
    store_dir(Kind::Koaloader).join(format!("{proxy}-{}.dll", arch.bits()))
}

pub fn has_lib(k: Kind, platform: Platform, arch: Arch) -> bool {
    lib_file(k, platform, arch).is_file()
}

/// Record where the imported files came from (archive name or release tag).
pub fn set_source(k: Kind, source: &str) {
    let _ = fs::write(store_dir(k).join(SOURCE_FILE), source);
}

fn source(k: Kind) -> Option<String> {
    let s = fs::read_to_string(store_dir(k).join(SOURCE_FILE)).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Short description of what has been imported for `k`, or None.
pub fn import_summary(k: Kind) -> Option<String> {
    let files = files_summary(k)?;
    Some(match source(k) {
        Some(src) => format!("{src} ({files})"),
        None => files,
    })
}

fn files_summary(k: Kind) -> Option<String> {
    if k == Kind::Koaloader {
        let n = koaloader_proxies().len();
        return (n > 0).then(|| format!("{n} proxies"));
    }
    let mut parts = Vec::new();
    for (p, label) in [(Platform::Windows, "Win"), (Platform::Linux, "Linux")] {
        let bits: Vec<&str> = [Arch::X86, Arch::X64]
            .into_iter()
            .filter(|a| has_lib(k, p, *a))
            .map(Arch::bits)
            .collect();
        if !bits.is_empty() {
            parts.push(format!("{label} {}", bits.join("/")));
        }
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// Koaloader proxies that have been imported (in preference order).
pub fn koaloader_proxies() -> Vec<String> {
    KOALOADER_PROXIES
        .iter()
        .filter(|p| proxy_file(p, Arch::X86).is_file() || proxy_file(p, Arch::X64).is_file())
        .map(|p| p.to_string())
        .collect()
}

/// Whether Koaloader installs need Steam launch options to work. Wine
/// prefers its builtin system DLLs unless told otherwise; real Windows
/// loads the proxy DLL next to the executable on its own.
pub const NEEDS_LAUNCH_OPTIONS: bool = !cfg!(windows);

pub fn launch_options(proxy: &str) -> String {
    format!("WINEDLLOVERRIDES=\"{proxy}=n,b\" %command%")
}

// ---------------------------------------------------------------------------
// Archive import
// ---------------------------------------------------------------------------

pub use crate::archive::ArchiveError as ImportError;

/// Higher is better. Prefers non-logging builds and plausibly-named files.
fn score(path: &str) -> i32 {
    let p = path.to_ascii_lowercase();
    let file = p.rsplit('/').next().unwrap_or(&p);
    let mut s = 0;
    if file.contains("steam_api") || file.contains("smoke") || file.contains("cream") {
        s += 2;
    }
    if p.contains("nonlog") {
        s += 3;
    } else if p.contains("log_build") || p.contains("log/") {
        s -= 5;
    }
    if p.contains("koaloader") || p.contains("proxy") {
        s -= 3;
    }
    s
}

/// Import a release archive (zip, 7z, rar, tar.*). Tries the given password,
/// or no password followed by a few well-known ones when `password` is None.
pub fn import_archive(k: Kind, zip_path: &Path, password: Option<&str>) -> Result<String, ImportError> {
    if let Some(pw) = password {
        return import_with(k, zip_path, Some(pw));
    }
    match import_with(k, zip_path, None) {
        Err(ImportError::NeedPassword) => {}
        other => return other,
    }
    for pw in KNOWN_PASSWORDS {
        match import_with(k, zip_path, Some(pw)) {
            Err(ImportError::WrongPassword) => continue,
            other => return other,
        }
    }
    Err(ImportError::NeedPassword)
}

struct Extracted {
    name: String,
    format: Format,
    arch: Arch,
    bytes: Vec<u8>,
}

fn read_candidates(
    k: Kind,
    zip_path: &Path,
    password: Option<&str>,
) -> Result<(Vec<Extracted>, Option<Vec<u8>>), ImportError> {
    let wanted = |name: &str| {
        let lower = name.to_ascii_lowercase();
        lower.ends_with(".dll")
            || lower.ends_with(".so")
            || (k == Kind::SmokeApi && lower.ends_with("smokeapi.config.json"))
    };
    let mut out = Vec::new();
    let mut template = None;
    for f in archive::extract(zip_path, password, wanted)? {
        if f.name.to_ascii_lowercase().ends_with(".json") {
            template = Some(f.bytes);
        } else if let Some((format, arch)) = binfmt::detect(&f.bytes) {
            out.push(Extracted { name: f.name, format, arch, bytes: f.bytes });
        }
    }
    Ok((out, template))
}

fn import_with(k: Kind, zip_path: &Path, password: Option<&str>) -> Result<String, ImportError> {
    let (files, template) = read_candidates(k, zip_path, password)?;
    let io = |e: std::io::Error| ImportError::Failed(e.to_string());

    // Collect (destination, bytes, source name) before touching the store.
    let mut writes: Vec<(PathBuf, Vec<u8>, String)> = Vec::new();
    match k {
        Kind::Koaloader => {
            for f in files {
                if f.format != Format::Pe {
                    continue;
                }
                let file = f.name.rsplit('/').next().unwrap_or(&f.name).to_ascii_lowercase();
                let Some(stem) = file.strip_suffix(".dll") else { continue };
                if !KOALOADER_PROXIES.contains(&stem) {
                    continue;
                }
                let dest = proxy_file(stem, f.arch);
                if !writes.iter().any(|(d, _, _)| *d == dest) {
                    writes.push((dest, f.bytes, f.name));
                }
            }
        }
        Kind::SmokeApi | Kind::CreamApi => {
            for (platform, format) in [(Platform::Windows, Format::Pe), (Platform::Linux, Format::Elf)] {
                for arch in [Arch::X86, Arch::X64] {
                    let best = files
                        .iter()
                        .filter(|f| f.format == format && f.arch == arch)
                        // .so files must look like an unlocker, not a random dependency
                        .filter(|f| format == Format::Pe || score(&f.name) >= 2)
                        .max_by_key(|f| score(&f.name));
                    if let Some(f) = best {
                        writes.push((lib_file(k, platform, arch), f.bytes.clone(), f.name.clone()));
                    }
                }
            }
        }
    }
    if writes.is_empty() {
        return Err(ImportError::Failed(format!("No usable {k} files found in this archive")));
    }

    let dir = store_dir(k);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(io)?;
    }
    fs::create_dir_all(&dir).map_err(io)?;
    for (dest, bytes, _) in &writes {
        fs::write(dest, bytes).map_err(io)?;
    }
    if let Some(t) = template {
        fs::write(dir.join(SMOKE_TEMPLATE), t).map_err(io)?;
    }
    if let Some(name) = zip_path.file_name() {
        set_source(k, &name.to_string_lossy());
    }
    let summary = match k {
        Kind::Koaloader => format!("{} proxy DLLs", writes.len()),
        _ => writes.iter().map(|(_, _, n)| n.as_str()).collect::<Vec<_>>().join(", "),
    };
    Ok(format!("Imported {k}: {summary}"))
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// Identify which unlocker a binary is by looking for telltale strings.
fn identify(path: &Path, dir: &Path) -> String {
    if let Ok(bytes) = fs::read(path) {
        if binfmt::contains_ci(&bytes, "smokeapi") || binfmt::contains_ci(&bytes, "smoke_api") {
            return "SmokeAPI".into();
        }
        if binfmt::contains_ci(&bytes, "creamapi") || binfmt::contains_ci(&bytes, "cream_api") {
            return "CreamAPI".into();
        }
    }
    if dir.join(CREAM_CONFIG).is_file() {
        "CreamAPI".into()
    } else if dir.join(SMOKE_CONFIG).is_file() {
        "SmokeAPI".into()
    } else {
        "Unknown".into()
    }
}

fn koaloader_in(dir: &Path) -> Option<Installed> {
    for proxy in KOALOADER_PROXIES {
        let p = dir.join(format!("{proxy}.dll"));
        if !p.is_file() {
            continue;
        }
        let Ok(bytes) = fs::read(&p) else { continue };
        if !binfmt::contains_ci(&bytes, "koaloader") {
            continue;
        }
        let unlocker = if SMOKE_KOALOADER_NAMES.iter().any(|n| dir.join(n).is_file()) {
            "SmokeAPI"
        } else {
            "Unknown"
        };
        return Some(Installed {
            method: Method::Koaloader,
            unlocker: unlocker.into(),
            dir: dir.to_path_buf(),
            proxy: Some(proxy.to_string()),
        });
    }
    None
}

pub fn detect(game: &Game) -> Vec<Installed> {
    let mut out: Vec<Installed> = Vec::new();
    for api in &game.apis {
        if api.orig_path().is_file() {
            out.push(Installed {
                method: Method::Direct,
                unlocker: identify(&api.path(), &api.dir),
                dir: api.dir.clone(),
                proxy: None,
            });
        }
    }
    let mut dirs: Vec<&Path> = game.exe_dirs.iter().map(|e| e.dir.as_path()).collect();
    dirs.extend(game.apis.iter().map(|a| a.dir.as_path()));
    dirs.sort();
    dirs.dedup();
    for d in dirs {
        if let Some(k) = koaloader_in(d) {
            out.push(k);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Install / uninstall
// ---------------------------------------------------------------------------

fn cream_ini(appid: u32, dlcs: &[Dlc]) -> String {
    let mut s = format!(
        "[steam]\n\
         appid = {appid}\n\
         unlockall = false\n\
         orgapi = steam_api_o.dll\n\
         orgapi64 = steam_api64_o.dll\n\
         extraprotection = false\n\
         forceoffline = false\n\
         \n\
         [steam_misc]\n\
         disableuserinterface = false\n\
         \n\
         [dlc]\n"
    );
    for d in dlcs.iter().filter(|d| d.enabled) {
        let name = d.name.replace(['\r', '\n'], " ");
        s.push_str(&format!("{} = {}\n", d.id, name));
    }
    s
}

/// SmokeAPI unlocks everything by default, so a config is only needed to
/// lock DLCs the user deselected. `$schema`/`$version` are taken from the
/// template shipped in the imported zip, when there was one.
fn smoke_config(dlcs: &[Dlc]) -> Option<String> {
    let locked: Vec<&Dlc> = dlcs.iter().filter(|d| !d.enabled).collect();
    if locked.is_empty() {
        return None;
    }
    let template: Option<Value> = fs::read(store_dir(Kind::SmokeApi).join(SMOKE_TEMPLATE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let mut cfg = json!({ "logging": false, "default_app_status": "unlocked" });
    for key in ["$schema", "$version"] {
        if let Some(v) = template.as_ref().and_then(|t| t.get(key)) {
            cfg[key] = v.clone();
        }
    }
    let overrides: serde_json::Map<String, Value> = locked
        .iter()
        .map(|d| (d.id.to_string(), json!("locked")))
        .collect();
    cfg["override_dlc_status"] = Value::Object(overrides);
    serde_json::to_string_pretty(&cfg).ok()
}

fn remove_if_file(p: &Path) -> Result<(), String> {
    if p.is_file() {
        fs::remove_file(p).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(())
}

fn remove_extras(dir: &Path) -> Result<(), String> {
    EXTRA_FILES.iter().try_for_each(|f| remove_if_file(&dir.join(f)))
}

fn write(p: &Path, bytes: impl AsRef<[u8]>) -> Result<(), String> {
    fs::write(p, bytes).map_err(|e| format!("{}: {e}", p.display()))
}

#[derive(Debug, Clone)]
pub struct InstallRequest {
    pub unlocker: Kind,
    pub method: Method,
    pub proxy: String,
    pub dlcs: Vec<Dlc>,
}

fn install_direct(game: &Game, req: &InstallRequest) -> Result<String, String> {
    let k = req.unlocker;
    // Validate everything first so we never leave a half-installed game.
    let apis: Vec<&ApiFile> = game.platform_apis().collect();
    for api in &apis {
        if !has_lib(k, api.platform, api.arch) {
            let plat = match api.platform {
                Platform::Windows => "Windows",
                Platform::Linux => "Linux",
            };
            return Err(format!(
                "The imported {k} zip has no {plat} {}-bit library (needed for {})",
                api.arch.bits(),
                api.path().display()
            ));
        }
    }
    let config = match k {
        Kind::CreamApi => Some((CREAM_CONFIG, cream_ini(game.appid, &req.dlcs))),
        _ => smoke_config(&req.dlcs).map(|c| (SMOKE_CONFIG, c)),
    };
    for api in &apis {
        let lib = fs::read(lib_file(k, api.platform, api.arch)).map_err(|e| e.to_string())?;
        let (path, orig) = (api.path(), api.orig_path());
        if !orig.exists() {
            if !path.is_file() {
                continue;
            }
            fs::rename(&path, &orig).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        write(&path, lib)?;
        if let Some((name, content)) = &config {
            write(&api.dir.join(name), content)?;
        }
    }
    Ok(format!("Installed {k} (direct) into {} location(s)", apis.len()))
}

fn install_koaloader(game: &Game, req: &InstallRequest) -> Result<String, String> {
    if req.unlocker != Kind::SmokeApi {
        return Err("Koaloader mode only supports SmokeAPI".into());
    }
    if game.platform() != Platform::Windows {
        return Err("Koaloader mode is only for Windows games".into());
    }
    if game.exe_dirs.is_empty() {
        return Err("No game executables found to place Koaloader next to".into());
    }
    let proxy = req.proxy.as_str();
    if !KOALOADER_PROXIES.contains(&proxy) {
        return Err(format!("Unknown Koaloader proxy '{proxy}'"));
    }
    for t in &game.exe_dirs {
        if !proxy_file(proxy, t.arch).is_file() {
            return Err(format!("Koaloader {proxy}.dll ({}-bit) has not been imported", t.arch.bits()));
        }
        if !has_lib(Kind::SmokeApi, Platform::Windows, t.arch) {
            return Err(format!("SmokeAPI {}-bit DLL has not been imported", t.arch.bits()));
        }
        let dest = t.dir.join(format!("{proxy}.dll"));
        if dest.exists() {
            return Err(format!(
                "{} already exists (the game ships its own {proxy}.dll); pick another proxy",
                dest.display()
            ));
        }
    }
    let config = smoke_config(&req.dlcs);
    for t in &game.exe_dirs {
        let koa = fs::read(proxy_file(proxy, t.arch)).map_err(|e| e.to_string())?;
        let smoke = fs::read(lib_file(Kind::SmokeApi, Platform::Windows, t.arch)).map_err(|e| e.to_string())?;
        write(&t.dir.join(format!("{proxy}.dll")), koa)?;
        write(&t.dir.join(format!("SmokeAPI{}.dll", t.arch.bits())), smoke)?;
        if let Some(c) = &config {
            write(&t.dir.join(SMOKE_CONFIG), c)?;
        }
    }
    let mut msg = format!(
        "Installed SmokeAPI via Koaloader ({proxy}.dll) into {} folder(s)",
        game.exe_dirs.len()
    );
    if NEEDS_LAUNCH_OPTIONS {
        msg.push_str(&format!(". Set the game's Steam launch options to: {}", launch_options(proxy)));
    }
    Ok(msg)
}

/// Install `req` into `game`, first removing any existing unlocker.
pub fn install(game: &Game, req: &InstallRequest) -> Result<String, String> {
    if !game.installed.is_empty() {
        uninstall(game)?;
    }
    match req.method {
        Method::Direct => install_direct(game, req),
        Method::Koaloader => install_koaloader(game, req),
    }
}

/// Remove every unlocker install detected in `game`.
pub fn uninstall(game: &Game) -> Result<String, String> {
    let mut restored = 0;
    for api in &game.apis {
        restored += uninstall_direct(api)? as usize;
    }
    for inst in detect(game).into_iter().filter(|i| i.method == Method::Koaloader) {
        if let Some(proxy) = &inst.proxy {
            remove_if_file(&inst.dir.join(format!("{proxy}.dll")))?;
        }
        for n in SMOKE_KOALOADER_NAMES {
            remove_if_file(&inst.dir.join(n))?;
        }
        remove_extras(&inst.dir)?;
        restored += 1;
    }
    if restored == 0 {
        return Err("Nothing to uninstall".into());
    }
    Ok(format!("Removed {restored} unlocker install(s) from {}", game.name))
}

fn uninstall_direct(api: &ApiFile) -> Result<bool, String> {
    let (path, orig) = (api.path(), api.orig_path());
    if !orig.is_file() {
        return Ok(false);
    }
    remove_if_file(&path)?;
    fs::rename(&orig, &path).map_err(|e| format!("{}: {e}", orig.display()))?;
    remove_extras(&api.dir)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam::inspect_game_dir;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ci-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn game_at(dir: &Path) -> Game {
        let (apis, exe_dirs) = inspect_game_dir(dir);
        let mut g = Game {
            appid: 1,
            name: "Test".into(),
            dir: dir.to_path_buf(),
            apis,
            exe_dirs,
            installed: vec![],
        };
        g.refresh_installed();
        g
    }

    #[test]
    fn scoring_prefers_nonlog() {
        assert!(score("nonlog_build/steam_api64.dll") > score("log_build/steam_api64.dll"));
        assert!(score("steam_api.dll") > score("Koaloader/version.dll"));
    }

    #[test]
    fn direct_detect_and_uninstall() {
        let game_dir = tmp("direct");
        fs::write(game_dir.join("steam_api64.dll"), b"original").unwrap();
        let g = game_at(&game_dir);
        assert_eq!(g.apis.len(), 1);
        assert!(g.installed.is_empty());

        // simulate a CreamAPI install
        let api = &g.apis[0];
        fs::rename(api.path(), api.orig_path()).unwrap();
        fs::write(api.path(), b"...CreamAPI...").unwrap();
        fs::write(game_dir.join(CREAM_CONFIG), "x").unwrap();
        let g = game_at(&game_dir);
        assert_eq!(g.apis.len(), 1, "x.dll + x_o.dll must dedupe");
        assert_eq!(g.installed.len(), 1);
        assert_eq!(g.installed[0].unlocker, "CreamAPI");

        uninstall(&g).unwrap();
        assert_eq!(fs::read(api.path()).unwrap(), b"original");
        assert!(!api.orig_path().exists() && !game_dir.join(CREAM_CONFIG).exists());
        assert!(game_at(&game_dir).installed.is_empty());
        fs::remove_dir_all(&game_dir).unwrap();
    }

    #[test]
    fn koaloader_detect_and_uninstall() {
        let game_dir = tmp("koa");
        fs::write(game_dir.join("steam_api64.dll"), b"original").unwrap();
        // a real-looking 64-bit exe so it is picked up as a target
        let mut exe = vec![0u8; 0x100];
        exe[0..2].copy_from_slice(b"MZ");
        exe[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        exe[0x80..0x84].copy_from_slice(b"PE\0\0");
        exe[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        fs::write(game_dir.join("Game.exe"), &exe).unwrap();
        fs::write(game_dir.join("version.dll"), b"..Koaloader..").unwrap();
        fs::write(game_dir.join("SmokeAPI64.dll"), b"smoke").unwrap();
        fs::write(game_dir.join("SmokeAPI.config.json"), b"{}").unwrap();

        let g = game_at(&game_dir);
        assert_eq!(g.exe_dirs.len(), 1);
        assert_eq!(g.installed.len(), 1);
        assert_eq!(g.installed[0].method, Method::Koaloader);
        assert_eq!(g.installed[0].proxy.as_deref(), Some("version"));
        assert_eq!(g.installed[0].unlocker, "SmokeAPI");

        uninstall(&g).unwrap();
        assert!(!game_dir.join("version.dll").exists());
        assert!(!game_dir.join("SmokeAPI64.dll").exists());
        assert!(game_dir.join("steam_api64.dll").exists());
        fs::remove_dir_all(&game_dir).unwrap();
    }

    #[test]
    fn native_linux_api_detected() {
        let game_dir = tmp("native");
        fs::write(game_dir.join("libsteam_api.so"), b"\x7fELF\x02rest-of-elf").unwrap();
        let g = game_at(&game_dir);
        assert_eq!(g.apis.len(), 1);
        assert_eq!(g.platform(), Platform::Linux);
        assert_eq!(g.apis[0].arch, Arch::X64);
        assert!(g.apis[0].orig_path().ends_with("libsteam_api_o.so"));
        fs::remove_dir_all(&game_dir).unwrap();
    }

    #[test]
    fn cream_ini_lists_enabled_only() {
        let dlcs = vec![
            Dlc { id: 1, name: "A".into(), enabled: true },
            Dlc { id: 2, name: "B".into(), enabled: false },
        ];
        let ini = cream_ini(10, &dlcs);
        assert!(ini.contains("appid = 10") && ini.contains("1 = A") && !ini.contains("2 = B"));
    }

    /// Installs the *imported* unlockers into a fake game and uninstalls
    /// them again. Needs SmokeAPI + Koaloader imported into the store, e.g.
    /// after `download_and_import` with the same data directory.
    #[test]
    #[ignore]
    fn real_store_install_roundtrip() {
        let game_dir = tmp("real");
        let mut exe = vec![0u8; 0x100];
        exe[0..2].copy_from_slice(b"MZ");
        exe[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        exe[0x80..0x84].copy_from_slice(b"PE\0\0");
        exe[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        fs::write(game_dir.join("Game.exe"), &exe).unwrap();
        fs::write(game_dir.join("steam_api64.dll"), b"original").unwrap();
        let dlcs = vec![
            Dlc { id: 11, name: "On".into(), enabled: true },
            Dlc { id: 12, name: "Off".into(), enabled: false },
        ];
        for (method, proxy) in [(Method::Direct, ""), (Method::Koaloader, "winmm")] {
            let g = game_at(&game_dir);
            let req = InstallRequest { unlocker: Kind::SmokeApi, method, proxy: proxy.into(), dlcs: dlcs.clone() };
            println!("{}", install(&g, &req).unwrap());
            let g = game_at(&game_dir);
            println!("detected: {:?}", g.installed);
            assert_eq!(g.installed.len(), 1);
            assert_eq!(g.installed[0].unlocker, "SmokeAPI");
            assert_eq!(g.installed[0].method, method);
            let cfg = fs::read_to_string(game_dir.join(SMOKE_CONFIG)).unwrap();
            assert!(cfg.contains("\"12\": \"locked\""), "{cfg}");
            uninstall(&g).unwrap();
            assert!(game_at(&game_dir).installed.is_empty());
            assert_eq!(fs::read(game_dir.join("steam_api64.dll")).unwrap(), b"original");
        }
        let mut left: Vec<_> = fs::read_dir(&game_dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        left.sort();
        assert_eq!(left, ["Game.exe", "steam_api64.dll"]);
        fs::remove_dir_all(&game_dir).unwrap();
    }

    #[test]
    fn encrypted_zip_import_flow() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let dir = tmp("zip");
        let zip_path = dir.join("test.zip");
        let mut pe = vec![0u8; 0x100];
        pe[0..2].copy_from_slice(b"MZ");
        pe[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        pe[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        {
            let mut w = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
            let opts = SimpleFileOptions::default()
                .with_aes_encryption(zip::AesMode::Aes256, "secret");
            w.start_file("steam_api64.dll", opts).unwrap();
            w.write_all(&pe).unwrap();
            w.finish().unwrap();
        }
        let (files, _) = match read_candidates(Kind::CreamApi, &zip_path, None) {
            Err(ImportError::NeedPassword) => read_candidates(Kind::CreamApi, &zip_path, Some("secret")).unwrap(),
            other => panic!("expected NeedPassword, got {:?}", other.map(|_| ())),
        };
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].arch, Arch::X64);
        assert!(matches!(
            read_candidates(Kind::CreamApi, &zip_path, Some("wrong")),
            Err(ImportError::WrongPassword)
        ));
        fs::remove_dir_all(&dir).unwrap();
    }
}
