//! Qt user interface, built with cxx-qt.
//!
//! The windows are described in `qml/` and compiled into the binary. They
//! talk to one [`qobject::Backend`] object, which holds all state:
//!
//! - Rust publishes what the windows show as JSON in three properties:
//!   `view` (everything but the lists), `games` and `dlcs`. QML parses them
//!   and binds to the result.
//! - QML calls back into Rust through invokables.
//! - Slow work (scanning, network, file IO) runs on plain threads; results
//!   are queued back onto the Qt thread with [`cxx_qt::CxxQtThread`].

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use cxx_qt::{CxxQtThread, CxxQtType, Threading};
use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QString, QUrl};
use serde_json::{Value, json};

use crate::archive;
use crate::dlc::{self, Dlc, DownloadStatus};
use crate::github;
use crate::paths::{self, APP_NAME};
use crate::settings::{self, Settings};
use crate::steam::{self, Game, Platform};
use crate::unlocker::{self, ImportError, InstallRequest, Kind, Method};
use crate::wine;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(QString, view)]
        #[qproperty(QString, games)]
        #[qproperty(QString, dlcs)]
        type Backend = super::BackendRust;
    }

    #[auto_cxx_name]
    unsafe extern "RustQt" {
        /// Called once by the main window when it is ready.
        #[qinvokable]
        fn start(self: Pin<&mut Backend>);
        #[qinvokable]
        fn rescan(self: Pin<&mut Backend>);
        #[qinvokable]
        fn set_search(self: Pin<&mut Backend>, text: &QString);
        #[qinvokable]
        fn select(self: Pin<&mut Backend>, appid: u32);
        #[qinvokable]
        fn set_status(self: Pin<&mut Backend>, text: &QString);
        /// The file URL of a game's icon, or "" while it is being fetched
        /// (`icon_ready` follows) or when there is none.
        #[qinvokable]
        fn game_icon(self: Pin<&mut Backend>, appid: u32) -> QString;

        #[qinvokable]
        fn set_unlocker(self: Pin<&mut Backend>, index: i32);
        #[qinvokable]
        fn set_method(self: Pin<&mut Backend>, index: i32);
        #[qinvokable]
        fn set_proxy(self: Pin<&mut Backend>, index: i32);
        #[qinvokable]
        fn install(self: Pin<&mut Backend>);
        /// 0 = cancel, 1 = continue anyway, 2 = disable those DLCs.
        #[qinvokable]
        fn answer_dlc_downloads(self: Pin<&mut Backend>, choice: i32);
        #[qinvokable]
        fn uninstall(self: Pin<&mut Backend>);
        #[qinvokable]
        fn uninstall_all(self: Pin<&mut Backend>);

        #[qinvokable]
        fn toggle_dlc(self: Pin<&mut Backend>, id: u32, enabled: bool);
        #[qinvokable]
        fn set_all_dlcs(self: Pin<&mut Backend>, enabled: bool);
        #[qinvokable]
        fn reload_dlcs(self: Pin<&mut Backend>);
        #[qinvokable]
        fn check_dlc_downloads(self: Pin<&mut Backend>);

        #[qinvokable]
        fn check_updates(self: Pin<&mut Backend>);
        #[qinvokable]
        fn dismiss_update(self: Pin<&mut Backend>);

        // --- settings window ---
        #[qinvokable]
        fn download(self: Pin<&mut Backend>, kind: i32);
        #[qinvokable]
        fn import_archive(self: Pin<&mut Backend>, kind: i32, url: &QString);
        #[qinvokable]
        fn submit_password(self: Pin<&mut Backend>, password: &QString);
        #[qinvokable]
        fn cancel_password(self: Pin<&mut Backend>);
        #[qinvokable]
        fn set_default_unlocker(self: Pin<&mut Backend>, index: i32);
        #[qinvokable]
        fn set_default_method(self: Pin<&mut Backend>, index: i32);
        #[qinvokable]
        fn set_check_updates(self: Pin<&mut Backend>, enabled: bool);
        #[qinvokable]
        fn set_folder(self: Pin<&mut Backend>, folder: i32, url: &QString);
        #[qinvokable]
        fn reset_folder(self: Pin<&mut Backend>, folder: i32);

        /// Some enabled DLCs need a download; answer with `answer_dlc_downloads`.
        #[qsignal]
        fn dlc_downloads_required(self: Pin<&mut Backend>, names: QString);
        /// An archive is password-protected; answer with `submit_password`
        /// or `cancel_password`.
        #[qsignal]
        fn password_needed(self: Pin<&mut Backend>, title: QString, wrong: bool);
        #[qsignal]
        fn icon_ready(self: Pin<&mut Backend>, appid: u32, url: QString);
    }

    impl cxx_qt::Threading for Backend {}
}

const APP_ID: &str = "io.github.UnlockInstaller";
const UNLOCKERS: [Kind; 2] = [Kind::SmokeApi, Kind::CreamApi];
const IMPORTABLE: [Kind; 3] = [Kind::SmokeApi, Kind::CreamApi, Kind::Koaloader];
const MAIN_QML: &str = "qrc:/qt/qml/UnlockInstaller/qml/Main.qml";
/// How many game icons are fetched at once.
const ICON_WORKERS: usize = 4;

pub fn run() -> i32 {
    // Fusion draws every control itself, so the app looks the same
    // everywhere, and it follows the system's light or dark palette.
    set_env_default("QT_QUICK_CONTROLS_STYLE", "Fusion");
    if wine::detected() {
        // Wine's Direct3D is not dependable; draw in software so that the
        // "run the native version" warning can be shown at all.
        set_env_default("QT_QUICK_BACKEND", "software");
    }

    let mut app = QGuiApplication::new();
    QGuiApplication::set_desktop_file_name(&QString::from(APP_ID));
    let mut engine = QQmlApplicationEngine::new();
    if let Some(mut app) = app.as_mut() {
        app.as_mut().set_application_name(&QString::from(APP_NAME));
        app.set_application_version(&QString::from(env!("CARGO_PKG_VERSION")));
    }
    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from(MAIN_QML));
    }
    match app.as_mut() {
        Some(app) => app.exec(),
        None => 1,
    }
}

fn set_env_default(name: &str, value: &str) {
    if std::env::var_os(name).is_some() {
        return;
    }
    // SAFETY: called before Qt or any other thread is started.
    unsafe { std::env::set_var(name, value) };
    // On Windows, Rust changes the process environment, but Qt reads the C
    // runtime's copy of it, which is only taken at startup. Set that too.
    #[cfg(windows)]
    {
        unsafe extern "C" {
            fn _wputenv(envstring: *const u16) -> i32;
        }
        let entry: Vec<u16> = format!("{name}={value}").encode_utf16().chain([0]).collect();
        // SAFETY: `entry` is a NUL-terminated UTF-16 string; the CRT copies it.
        unsafe { _wputenv(entry.as_ptr()) };
    }
}

type BackendThread = CxxQtThread<qobject::Backend>;

/// Run `work` on a new thread, then `done` on the Qt thread.
fn spawn<T: Send + 'static>(
    this: &qobject::Backend,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(Pin<&mut qobject::Backend>, T) + Send + 'static,
) {
    let qt = this.qt_thread();
    std::thread::spawn(move || match std::panic::catch_unwind(AssertUnwindSafe(work)) {
        Ok(result) => {
            let _ = qt.queue(move |obj| done(obj, result));
        }
        Err(_) => eprintln!("background task panicked"),
    });
}

fn platform_name(p: Platform) -> &'static str {
    match p {
        Platform::Windows if cfg!(windows) => "Windows",
        Platform::Windows => "Windows (Proton)",
        Platform::Linux => "Native Linux",
    }
}

fn rel(base: &Path, p: &Path) -> String {
    let s = p.strip_prefix(base).unwrap_or(p).display().to_string();
    if s.is_empty() {
        "the game folder".into()
    } else {
        s
    }
}

fn installed_label(g: &Game) -> String {
    let mut parts: Vec<String> = g
        .installed
        .iter()
        .map(|i| match i.method {
            Method::Direct => i.unlocker.clone(),
            Method::Koaloader => format!("{} + Koaloader", i.unlocker),
        })
        .collect();
    parts.dedup();
    parts.join(", ")
}

fn file_url(path: &Path) -> String {
    QUrl::from_local_file(&QString::from(path.display().to_string().as_str()))
        .to_qstring()
        .to_string()
}

/// The local path of a `file://` URL from a QML file dialog.
fn url_path(url: &QString) -> Option<PathBuf> {
    QUrl::from(url)
        .to_local_file()
        .map(|p| PathBuf::from(p.to_string()))
        .filter(|p| !p.as_os_str().is_empty())
}

fn kind_at(list: &[Kind], index: i32) -> Option<Kind> {
    usize::try_from(index).ok().and_then(|i| list.get(i).copied())
}

fn method_at(index: i32) -> Method {
    if index == 1 {
        Method::Koaloader
    } else {
        Method::Direct
    }
}

fn method_index(m: Method) -> usize {
    match m {
        Method::Direct => 0,
        Method::Koaloader => 1,
    }
}

enum DlcState {
    NotLoaded,
    Loading,
    Loaded(Vec<Dlc>, String),
    Failed(String),
}

struct Entry {
    game: Game,
    dlcs: DlcState,
    download_status: BTreeMap<u32, DownloadStatus>,
    checking_downloads: bool,
}

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    selected: Option<u32>,
    busy: bool,
    scanning: bool,
}

impl State {
    fn entry(&self, appid: u32) -> Option<&Entry> {
        self.entries.iter().find(|e| e.game.appid == appid)
    }
    fn entry_mut(&mut self, appid: u32) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.game.appid == appid)
    }
    fn selected_entry(&self) -> Option<&Entry> {
        self.entry(self.selected?)
    }
}

/// A folder whose location can be changed in the settings.
#[derive(Clone, Copy, PartialEq)]
enum Folder {
    /// Where imported unlockers are extracted to.
    Data,
    Cache,
}

const FOLDERS: [Folder; 2] = [Folder::Data, Folder::Cache];

impl Folder {
    fn title(self) -> &'static str {
        match self {
            Folder::Data => "Unlocker files",
            Folder::Cache => "Cache",
        }
    }
    fn tooltip(self) -> &'static str {
        match self {
            Folder::Data => "Where imported unlocker archives are extracted to",
            Folder::Cache => "DLC lists and downloaded release archives",
        }
    }
    fn current(self) -> PathBuf {
        match self {
            Folder::Data => paths::data_dir(),
            Folder::Cache => paths::cache_dir(),
        }
    }
    /// The setting holding this folder's custom location.
    fn slot(self, s: &mut Settings) -> &mut Option<PathBuf> {
        match self {
            Folder::Data => &mut s.data_dir,
            Folder::Cache => &mut s.cache_dir,
        }
    }
}

/// An install waiting for the user to decide about DLCs that need a download.
struct PendingInstall {
    game: Game,
    req: InstallRequest,
    required: Vec<(u32, String)>,
}

/// Game icons waiting to be fetched, and how many workers fetch them.
#[derive(Default)]
struct IconQueue {
    waiting: VecDeque<u32>,
    workers: usize,
}

pub struct BackendRust {
    view: QString,
    games: QString,
    dlcs: QString,

    state: State,
    settings: Settings,
    search: String,
    status: String,
    /// Text of the "update available" banner.
    update: Option<String>,
    /// Choices in the details pane.
    unlocker: Kind,
    method: Method,
    proxy: Option<String>,
    /// Imported Koaloader proxies.
    proxies: Vec<String>,
    pending_install: Option<PendingInstall>,
    pending_password: Option<(Kind, PathBuf)>,
    downloading: Vec<Kind>,
    /// Fetched icon URLs ("" when there is none); absent while fetching.
    icons: HashMap<u32, String>,
    icons_requested: BTreeSet<u32>,
    icon_queue: Arc<Mutex<IconQueue>>,
}

impl Default for BackendRust {
    fn default() -> Self {
        let mut b = BackendRust {
            view: QString::default(),
            games: QString::from("[]"),
            dlcs: QString::from("[]"),
            state: State::default(),
            settings: Settings::load(),
            search: String::new(),
            status: String::new(),
            update: None,
            unlocker: Kind::SmokeApi,
            method: Method::Direct,
            proxy: None,
            proxies: Vec::new(),
            pending_install: None,
            pending_password: None,
            downloading: Vec::new(),
            icons: HashMap::new(),
            icons_requested: BTreeSet::new(),
            icon_queue: Arc::default(),
        };
        b.refresh_imports();
        b.preselect();
        b.normalize();
        b.view = QString::from(b.view_json().as_str());
        b
    }
}

// ----- state logic (plain Rust, no Qt) ---------------------------------------

impl BackendRust {
    fn selected_proxy(&self) -> Option<String> {
        match &self.proxy {
            Some(p) if self.proxies.contains(p) => Some(p.clone()),
            _ => self.proxies.first().cloned(),
        }
    }

    fn koaloader_possible(&self) -> bool {
        self.state
            .selected_entry()
            .is_some_and(|e| e.game.platform() == Platform::Windows && !e.game.exe_dirs.is_empty())
    }

    fn refresh_imports(&mut self) {
        self.proxies = unlocker::koaloader_proxies();
    }

    /// Koaloader only works for SmokeAPI on Windows games.
    fn normalize(&mut self) {
        if !(self.koaloader_possible() && self.unlocker == Kind::SmokeApi) {
            self.method = Method::Direct;
        }
    }

    /// Preselect the options matching the selected game's install, or the
    /// defaults from the settings.
    fn preselect(&mut self) {
        let Some(e) = self.state.selected_entry() else {
            return;
        };
        let first = e.game.installed.first().cloned();
        let suggested = e.game.suggested_proxy.as_ref().map(|s| s.proxy.clone());
        if let Some(inst) = first {
            if let Some(k) = UNLOCKERS.iter().find(|k| k.name() == inst.unlocker) {
                self.unlocker = *k;
            }
            self.method = inst.method;
            if inst.proxy.is_some() {
                self.proxy = inst.proxy;
            }
        } else {
            // Nothing installed: start from the defaults chosen in Settings
            // (normalize falls back to direct where Koaloader can't work)...
            self.unlocker = self.settings.unlocker;
            self.method = self.settings.method;
            // ...but pick the proxy the game is seen to load, if imported.
            if let Some(p) = suggested.filter(|p| self.proxies.contains(p)) {
                self.proxy = Some(p);
            }
        }
    }

    fn games_json(&self) -> String {
        let needle = self.search.to_lowercase();
        let rows: Vec<Value> = self
            .state
            .entries
            .iter()
            .filter(|e| needle.is_empty() || e.game.name.to_lowercase().contains(&needle))
            .map(|e| {
                let inst = installed_label(&e.game);
                json!({
                    "appid": e.game.appid,
                    "name": e.game.name,
                    "installed": !inst.is_empty(),
                    "subtitle": if inst.is_empty() { platform_name(e.game.platform()).to_string() } else { inst },
                })
            })
            .collect();
        Value::Array(rows).to_string()
    }

    fn dlcs_json(&self) -> String {
        let rows: Vec<Value> = match self.state.selected_entry() {
            Some(Entry {
                dlcs: DlcState::Loaded(list, _),
                download_status,
                ..
            }) => list
                .iter()
                .map(|d| {
                    json!({
                        "id": d.id,
                        "name": d.name,
                        "enabled": d.enabled,
                        "status": download_status.get(&d.id).map(|s| s.label()).unwrap_or(""),
                    })
                })
                .collect(),
            _ => Vec::new(),
        };
        Value::Array(rows).to_string()
    }

    fn view_json(&self) -> String {
        let st = &self.state;
        let entry = st.selected_entry();
        let busy = st.busy;
        let has_installs = entry.is_some_and(|e| !e.game.installed.is_empty());
        let any_installed = st.entries.iter().any(|e| !e.game.installed.is_empty());
        let kind = self.unlocker;
        let koaloader = self.method == Method::Koaloader;
        let proxy = self.selected_proxy();

        // Say why Install is unavailable instead of just greying it out.
        let blocker = if unlocker::import_summary(kind).is_none() {
            format!("{kind} has not been imported yet (see Settings)")
        } else if koaloader && proxy.is_none() {
            "Koaloader has not been imported yet (see Settings)".to_string()
        } else {
            String::new()
        };

        let mut v = json!({
            "appName": APP_NAME,
            "version": env!("CARGO_PKG_VERSION"),
            "wine": wine::detected(),
            "releaseUrl": github::app_release_url(),
            "status": self.status,
            "noImports": UNLOCKERS.iter().all(|k| unlocker::import_summary(*k).is_none()),
            "update": self.update.clone().unwrap_or_default(),
            "rescanEnabled": !busy && !st.scanning,
            "rescanLabel": if st.scanning { "Scanning…" } else { "Rescan" },
            "noGamesText": if st.scanning { "Scanning…" } else { "No games found" },
            "uninstallAllEnabled": !busy && any_installed,
            "hasSelection": entry.is_some(),
            "appid": st.selected.unwrap_or(0),
            "unlocker": UNLOCKERS.iter().position(|k| *k == kind).unwrap_or(0),
            "method": method_index(self.method),
            "methodEnabled": self.koaloader_possible() && kind == Kind::SmokeApi,
            "koaloader": koaloader,
            "proxies": self.proxies,
            "proxy": proxy.as_ref().and_then(|p| self.proxies.iter().position(|x| x == p)).map_or(-1, |i| i as i64),
            "installLabel": if has_installs { "Reinstall" } else { "Install" },
            "installBlocker": blocker,
            "installEnabled": !busy && entry.is_some() && blocker.is_empty(),
            "uninstallEnabled": !busy && has_installs,
            "settings": self.settings_json(),
        });

        if let Some(e) = entry {
            let g = &e.game;
            let apis: Vec<String> = g
                .platform_apis()
                .map(|a| format!("{} ({}-bit)", rel(&g.dir, &a.path()), a.arch.bits()))
                .collect();
            let installed = if g.installed.is_empty() {
                "No unlocker installed".to_string()
            } else {
                let parts: Vec<String> = g
                    .installed
                    .iter()
                    .map(|i| match i.method {
                        Method::Direct => format!("{} (direct) in {}", i.unlocker, rel(&g.dir, &i.dir)),
                        Method::Koaloader => format!(
                            "{} via Koaloader ({}.dll) in {}",
                            i.unlocker,
                            i.proxy.as_deref().unwrap_or("?"),
                            rel(&g.dir, &i.dir)
                        ),
                    })
                    .collect();
                format!("Installed: {}", parts.join("; "))
            };
            let koa_proxy = g.installed.iter().find_map(|i| i.proxy.clone());
            let exe_dirs: Vec<String> = g.exe_dirs.iter().map(|d| rel(&g.dir, &d.dir)).collect();
            let hint = if koaloader {
                let p = proxy.clone().unwrap_or_else(|| "<proxy>".into());
                let mut text = format!(
                    "Koaloader puts {p}.dll and SmokeAPI next to the game executable ({}).",
                    exe_dirs.join(", ")
                );
                if unlocker::NEEDS_LAUNCH_OPTIONS {
                    text.push_str(&format!(
                        " Under Proton the game's Steam launch options must contain: {}",
                        unlocker::launch_options(&p)
                    ));
                }
                text
            } else {
                String::new()
            };
            let (note, note_class, note_tip) = proxy_note(g.suggested_proxy.as_ref(), proxy.as_deref());
            let extra = json!({
                "title": g.name,
                "info": format!("App ID {}  ·  {}", g.appid, platform_name(g.platform())),
                "path": g.dir.display().to_string(),
                "apis": format!("Steam API: {}", apis.join(", ")),
                "installed": installed,
                "installedOk": has_installs,
                "launchVisible": unlocker::NEEDS_LAUNCH_OPTIONS && koa_proxy.is_some(),
                "launchText": koa_proxy.map(|p| unlocker::launch_options(&p)).unwrap_or_default(),
                "hint": hint,
                "proxyNote": note,
                "proxyNoteClass": note_class,
                "proxyNoteTip": note_tip,
                "dlc": dlc_header(e),
            });
            if let (Value::Object(v), Value::Object(extra)) = (&mut v, extra) {
                v.extend(extra);
            }
        }
        v.to_string()
    }

    fn settings_json(&self) -> Value {
        let imports: Vec<Value> = IMPORTABLE
            .iter()
            .enumerate()
            .map(|(i, kind)| {
                json!({
                    "index": i,
                    "name": kind.name(),
                    "status": unlocker::import_summary(*kind).unwrap_or_else(|| "Not imported".into()),
                    "downloadTip": github::repo(*kind)
                        .map(|repo| format!("Download the latest release from github.com/{repo}"))
                        .unwrap_or_default(),
                    "downloading": self.downloading.contains(kind),
                    "importTip": format!("Import a {kind} release archive (zip, 7z, rar or tar)"),
                })
            })
            .collect();
        let mut s = self.settings.clone();
        let folders: Vec<Value> = FOLDERS
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let path = f.current();
                json!({
                    "index": i,
                    "title": f.title(),
                    "tip": f.tooltip(),
                    "path": path.display().to_string(),
                    "url": file_url(&path),
                    "custom": f.slot(&mut s).is_some(),
                })
            })
            .collect();
        let folders_note = if cfg!(windows) {
            "Changing a folder does not move the files already in it.".to_string()
        } else if paths::config_dir_from_env() {
            format!(
                "Changing a folder does not move the files already in it. The config folder is \
                 set by the {} environment variable.",
                paths::CONFIG_DIR_ENV
            )
        } else {
            format!(
                "Changing a folder does not move the files already in it. To use another config \
                 folder, start the app with the {} environment variable set.",
                paths::CONFIG_DIR_ENV
            )
        };
        let mut patterns: Vec<String> = archive::PATTERNS.iter().map(|p| p.to_string()).collect();
        if !cfg!(windows) {
            patterns.extend(archive::PATTERNS.iter().map(|p| p.to_ascii_uppercase()));
        }
        json!({
            "imports": imports,
            "defaultUnlocker": UNLOCKERS.iter().position(|k| *k == self.settings.unlocker).unwrap_or(0),
            "defaultMethod": method_index(self.settings.method),
            "checkUpdates": self.settings.check_updates,
            "folders": folders,
            "settingsPath": settings::location(),
            "foldersNote": folders_note,
            "archiveFilters": [
                format!("Archives (zip, 7z, rar, tar) ({})", patterns.join(" ")),
                "All files (*)",
            ],
        })
    }

    /// Change and save the settings. Returns an error message if they could
    /// not be saved.
    fn change_settings(&mut self, change: impl FnOnce(&mut Settings)) -> Result<(), String> {
        change(&mut self.settings);
        let saved = self.settings.save();
        // the unlocker folder may have changed
        self.refresh_imports();
        self.preselect();
        saved.map_err(|e| format!("Could not save the settings: {e}"))
    }

    fn set_folder(&mut self, folder: Folder, path: Option<PathBuf>) {
        self.status = match self.change_settings(|s| *folder.slot(s) = path) {
            Ok(()) => format!(
                "{} folder is now {} (existing files were not moved)",
                folder.title(),
                folder.current().display()
            ),
            Err(e) => e,
        };
    }

    fn set_dlc_enabled(&mut self, filter: impl Fn(&Dlc) -> bool, enabled: bool) {
        let Some(appid) = self.state.selected else {
            return;
        };
        if let Some(Entry {
            dlcs: DlcState::Loaded(list, _),
            ..
        }) = self.state.entry_mut(appid)
        {
            for d in list.iter_mut().filter(|d| filter(d)) {
                d.enabled = enabled;
            }
            dlc::save_selection(appid, list);
        }
    }

    /// Store games updated by a background job.
    fn finish_job(&mut self, msg: String, games: Vec<Game>) {
        self.state.busy = false;
        for g in games {
            if let Some(e) = self.state.entry_mut(g.appid) {
                e.game = g;
            }
        }
        self.status = msg;
        self.preselect();
    }
}

/// The DLC section header of the details pane.
fn dlc_header(e: &Entry) -> Value {
    let (text, spinning, loaded, failed) = match &e.dlcs {
        DlcState::NotLoaded => (String::new(), false, false, false),
        DlcState::Loading => ("Fetching DLC list…".into(), true, false, false),
        DlcState::Failed(err) => (format!("Failed to fetch DLCs: {err}"), false, false, true),
        DlcState::Loaded(list, _) if list.is_empty() => {
            ("This game has no DLCs on Steam".into(), false, false, false)
        }
        DlcState::Loaded(list, source) => {
            let on = list.iter().filter(|d| d.enabled).count();
            (
                format!("DLCs · {on} of {} enabled · from {source}", list.len()),
                false,
                true,
                false,
            )
        }
    };
    json!({
        "text": text,
        "spinning": spinning,
        "loaded": loaded,
        "refreshVisible": !spinning,
        "refreshLabel": if failed { "Retry" } else { "Refresh" },
        "checkEnabled": !e.checking_downloads,
        "checkLabel": if e.checking_downloads { "Checking…" } else { "Check downloads" },
    })
}

/// Which proxy the game is seen to load, and whether it's selected:
/// (text, style class, tooltip).
fn proxy_note(s: Option<&unlocker::ProxySuggestion>, selected: Option<&str>) -> (String, &'static str, String) {
    let Some(s) = s else {
        return (
            "No proxy found in the game's imports".into(),
            "dim",
            "None of the Koaloader proxies appear in the import tables of the game's \
             executables or the DLLs next to them. The game may still load one at runtime; \
             version.dll and winmm.dll often work."
                .into(),
        );
    };
    let available = unlocker::koaloader_proxies().contains(&s.proxy);
    let mut tip = format!("{}.dll is linked by: {}", s.proxy, s.importers.join(", "));
    if !s.everywhere {
        tip.push_str("\nNot every executable folder loads it.");
    }
    let (text, class) = if selected == Some(s.proxy.as_str()) {
        ("✓ Suggested for this game".to_string(), "success")
    } else if available {
        (format!("Suggested: {}.dll", s.proxy), "warning")
    } else {
        tip.push_str("\nIt isn't among the imported Koaloader files.");
        (format!("Suggested: {}.dll (not imported)", s.proxy), "dim")
    };
    (text, class, tip)
}

// ----- the Qt object ---------------------------------------------------------

impl qobject::Backend {
    /// Re-publish `view` from the current state.
    fn publish(mut self: Pin<&mut Self>) {
        let json = {
            let mut r = self.as_mut().rust_mut();
            r.normalize();
            r.view_json()
        };
        self.set_view(QString::from(json.as_str()));
    }

    fn publish_games(self: Pin<&mut Self>) {
        let json = self.rust().games_json();
        self.set_games(QString::from(json.as_str()));
    }

    fn publish_dlcs(self: Pin<&mut Self>) {
        let json = self.rust().dlcs_json();
        self.set_dlcs(QString::from(json.as_str()));
    }

    fn publish_all(mut self: Pin<&mut Self>) {
        self.as_mut().publish_games();
        self.as_mut().publish_dlcs();
        self.publish();
    }

    fn status(mut self: Pin<&mut Self>, s: impl Into<String>) {
        self.as_mut().rust_mut().status = s.into();
        self.publish();
    }

    pub fn start(mut self: Pin<&mut Self>) {
        self.as_mut().rescan();
        if self.rust().settings.check_updates {
            self.as_mut().run_update_check(false);
        }
    }

    pub fn set_status(self: Pin<&mut Self>, text: &QString) {
        self.status(text.to_string());
    }

    pub fn set_search(mut self: Pin<&mut Self>, text: &QString) {
        self.as_mut().rust_mut().search = text.to_string();
        self.publish_games();
    }

    pub fn game_icon(mut self: Pin<&mut Self>, appid: u32) -> QString {
        if let Some(url) = self.rust().icons.get(&appid) {
            return QString::from(url.as_str());
        }
        if !self.as_mut().rust_mut().icons_requested.insert(appid) {
            return QString::default();
        }
        let queue = self.rust().icon_queue.clone();
        let start_worker = {
            let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
            q.waiting.push_back(appid);
            let start = q.workers < ICON_WORKERS;
            if start {
                q.workers += 1;
            }
            start
        };
        if start_worker {
            let qt = self.qt_thread();
            std::thread::spawn(move || icon_worker(queue, qt));
        }
        QString::default()
    }

    // ----- details pane --------------------------------------------------

    pub fn rescan(mut self: Pin<&mut Self>) {
        {
            let mut r = self.as_mut().rust_mut();
            if r.state.scanning {
                return;
            }
            r.state.scanning = true;
            r.status = "Scanning Steam libraries…".into();
        }
        self.as_mut().publish();
        spawn(&self, steam::scan_games, |mut this, res| {
            {
                let mut r = this.as_mut().rust_mut();
                let st = &mut r.state;
                st.scanning = false;
                if let Ok(games) = &res {
                    let mut old = std::mem::take(&mut st.entries);
                    st.entries = games
                        .iter()
                        .cloned()
                        .map(|game| {
                            // keep already-loaded DLC lists across rescans
                            let dlcs = old
                                .iter()
                                .position(|e| e.game.appid == game.appid)
                                .map(|i| old.swap_remove(i).dlcs)
                                .unwrap_or(DlcState::NotLoaded);
                            Entry {
                                game,
                                dlcs,
                                download_status: BTreeMap::new(),
                                checking_downloads: false,
                            }
                        })
                        .collect();
                    if st.selected_entry().is_none() {
                        st.selected = None;
                    }
                }
                r.status = match &res {
                    Ok(g) => format!("Found {} game(s) using the Steamworks API", g.len()),
                    Err(e) => format!("Scan failed: {e}"),
                };
                r.preselect();
            }
            this.publish_all();
        });
    }

    pub fn select(mut self: Pin<&mut Self>, appid: u32) {
        let needs_load = {
            let mut r = self.as_mut().rust_mut();
            if r.state.entry(appid).is_none() {
                return;
            }
            r.state.selected = Some(appid);
            r.preselect();
            matches!(r.state.entry(appid).map(|e| &e.dlcs), Some(DlcState::NotLoaded))
        };
        if needs_load {
            self.load_dlcs(false);
        } else {
            self.as_mut().publish_dlcs();
            self.publish();
        }
    }

    pub fn set_unlocker(mut self: Pin<&mut Self>, index: i32) {
        if let Some(k) = kind_at(&UNLOCKERS, index) {
            self.as_mut().rust_mut().unlocker = k;
        }
        self.publish();
    }

    pub fn set_method(mut self: Pin<&mut Self>, index: i32) {
        self.as_mut().rust_mut().method = method_at(index);
        self.publish();
    }

    pub fn set_proxy(mut self: Pin<&mut Self>, index: i32) {
        {
            let mut r = self.as_mut().rust_mut();
            if let Some(p) = usize::try_from(index).ok().and_then(|i| r.proxies.get(i)).cloned() {
                r.proxy = Some(p);
            }
        }
        self.publish();
    }

    fn load_dlcs(mut self: Pin<&mut Self>, refresh: bool) {
        let Some(appid) = self.rust().state.selected else {
            return;
        };
        if let Some(e) = self.as_mut().rust_mut().state.entry_mut(appid) {
            e.dlcs = DlcState::Loading;
            e.download_status.clear();
        }
        self.as_mut().publish_dlcs();
        self.as_mut().publish();
        spawn(
            &self,
            move || dlc::load_dlcs(appid, refresh),
            move |mut this, res| {
                if let Some(e) = this.as_mut().rust_mut().state.entry_mut(appid) {
                    e.dlcs = match res {
                        Ok((list, source)) => DlcState::Loaded(list, source.to_string()),
                        Err(err) => DlcState::Failed(err),
                    };
                }
                if this.rust().state.selected == Some(appid) {
                    this.as_mut().publish_dlcs();
                    this.publish();
                }
            },
        );
    }

    pub fn reload_dlcs(self: Pin<&mut Self>) {
        self.load_dlcs(true);
    }

    pub fn check_dlc_downloads(mut self: Pin<&mut Self>) {
        let (appid, dlcs) = {
            let mut r = self.as_mut().rust_mut();
            let Some(appid) = r.state.selected else { return };
            let Some(entry) = r.state.entry_mut(appid) else {
                return;
            };
            let DlcState::Loaded(dlcs, _) = &entry.dlcs else {
                return;
            };
            if entry.checking_downloads {
                return;
            }
            entry.checking_downloads = true;
            let dlcs = dlcs.clone();
            r.status = "Checking DLC download requirements…".into();
            (appid, dlcs)
        };
        self.as_mut().publish();
        spawn(
            &self,
            move || dlc::check_downloads(appid, &dlcs),
            move |mut this, statuses| {
                let selected = {
                    let mut r = this.as_mut().rust_mut();
                    if let Some(entry) = r.state.entry_mut(appid) {
                        entry.download_status = statuses;
                        entry.checking_downloads = false;
                    }
                    let selected = r.state.selected == Some(appid);
                    if selected {
                        r.status = "DLC download requirement check complete".into();
                    }
                    selected
                };
                if selected {
                    this.as_mut().publish_dlcs();
                    this.publish();
                }
            },
        );
    }

    /// Only the count in the header changes, so the DLC list itself (and its
    /// scroll position) is left alone.
    pub fn toggle_dlc(mut self: Pin<&mut Self>, id: u32, enabled: bool) {
        self.as_mut().rust_mut().set_dlc_enabled(|d| d.id == id, enabled);
        self.publish();
    }

    pub fn set_all_dlcs(mut self: Pin<&mut Self>, enabled: bool) {
        self.as_mut().rust_mut().set_dlc_enabled(|_| true, enabled);
        self.as_mut().publish_dlcs();
        self.publish();
    }

    fn set_busy(mut self: Pin<&mut Self>, busy: bool, status: impl Into<String>) {
        {
            let mut r = self.as_mut().rust_mut();
            r.state.busy = busy;
            r.status = status.into();
        }
        self.publish();
    }

    fn finish_job(mut self: Pin<&mut Self>, msg: String, games: Vec<Game>) {
        self.as_mut().rust_mut().finish_job(msg, games);
        self.publish_all();
    }

    pub fn install(mut self: Pin<&mut Self>) {
        let r = self.rust();
        let kind = r.unlocker;
        let Some(e) = r.state.selected_entry() else { return };
        let dlcs = match &e.dlcs {
            DlcState::Loaded(d, _) => d.clone(),
            _ if kind == Kind::CreamApi => {
                self.status("CreamAPI needs the DLC list; wait for it to load (or retry)");
                return;
            }
            _ => Vec::new(),
        };
        let game = e.game.clone();
        let req = InstallRequest {
            unlocker: kind,
            method: r.method,
            proxy: r.selected_proxy().unwrap_or_default(),
            dlcs,
        };
        self.as_mut().set_busy(
            true,
            format!("Checking DLC downloads before installing into {}…", game.name),
        );
        let (appid, check) = (game.appid, req.dlcs.clone());
        spawn(
            &self,
            move || dlc::check_downloads(appid, &check),
            move |mut this, statuses| {
                let required: Vec<(u32, String)> = req
                    .dlcs
                    .iter()
                    .filter(|d| d.enabled && statuses.get(&d.id) == Some(&DownloadStatus::Required))
                    .map(|d| (d.id, d.name.clone()))
                    .collect();
                if required.is_empty() {
                    this.perform_install(game, req);
                } else {
                    let names = required
                        .iter()
                        .map(|(_, name)| name.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    this.as_mut().rust_mut().pending_install = Some(PendingInstall { game, req, required });
                    this.dlc_downloads_required(QString::from(names.as_str()));
                }
            },
        );
    }

    pub fn answer_dlc_downloads(mut self: Pin<&mut Self>, choice: i32) {
        let Some(PendingInstall { game, mut req, required }) = self.as_mut().rust_mut().pending_install.take()
        else {
            return;
        };
        match choice {
            1 => self.perform_install(game, req),
            2 => {
                let ids: BTreeSet<u32> = required.iter().map(|(id, _)| *id).collect();
                for d in &mut req.dlcs {
                    if ids.contains(&d.id) {
                        d.enabled = false;
                    }
                }
                dlc::save_selection(game.appid, &req.dlcs);
                if let Some(Entry {
                    dlcs: DlcState::Loaded(list, _),
                    ..
                }) = self.as_mut().rust_mut().state.entry_mut(game.appid)
                {
                    for d in list.iter_mut().filter(|d| ids.contains(&d.id)) {
                        d.enabled = false;
                    }
                }
                self.as_mut().publish_dlcs();
                self.perform_install(game, req);
            }
            _ => self.set_busy(false, "Installation cancelled"),
        }
    }

    fn perform_install(mut self: Pin<&mut Self>, game: Game, req: InstallRequest) {
        self.as_mut()
            .status(format!("Installing {} into {}…", req.unlocker, game.name));
        spawn(
            &self,
            move || {
                let res = unlocker::install(&game, &req);
                let mut game = game;
                game.refresh_installed();
                (res, game)
            },
            |this, (res, game)| {
                this.finish_job(res.unwrap_or_else(|e| format!("Install failed: {e}")), vec![game]);
            },
        );
    }

    pub fn uninstall(mut self: Pin<&mut Self>) {
        let Some(game) = self.rust().state.selected_entry().map(|e| e.game.clone()) else {
            return;
        };
        self.as_mut().set_busy(true, format!("Uninstalling from {}…", game.name));
        spawn(
            &self,
            move || {
                let res = unlocker::uninstall(&game);
                let mut game = game;
                game.refresh_installed();
                (res, game)
            },
            |this, (res, game)| {
                this.finish_job(res.unwrap_or_else(|e| format!("Uninstall failed: {e}")), vec![game]);
            },
        );
    }

    pub fn uninstall_all(mut self: Pin<&mut Self>) {
        let games: Vec<Game> = self
            .rust()
            .state
            .entries
            .iter()
            .filter(|e| !e.game.installed.is_empty())
            .map(|e| e.game.clone())
            .collect();
        if games.is_empty() || self.rust().state.busy {
            return;
        }
        self.as_mut()
            .set_busy(true, format!("Uninstalling from {} game(s)…", games.len()));
        spawn(
            &self,
            move || {
                let mut errors = Vec::new();
                let mut ok = 0;
                let games: Vec<Game> = games
                    .into_iter()
                    .map(|mut g| {
                        match unlocker::uninstall(&g) {
                            Ok(_) => ok += 1,
                            Err(e) => errors.push(format!("{}: {e}", g.name)),
                        }
                        g.refresh_installed();
                        g
                    })
                    .collect();
                let mut msg = format!("Uninstalled from {ok} game(s)");
                if !errors.is_empty() {
                    msg.push_str(&format!("; {} failed: {}", errors.len(), errors.join("; ")));
                }
                (msg, games)
            },
            |this, (msg, games)| this.finish_job(msg, games),
        );
    }

    // ----- updates -------------------------------------------------------

    pub fn check_updates(self: Pin<&mut Self>) {
        self.run_update_check(true);
    }

    /// Ask GitHub whether a newer release exists and, if so, show a banner
    /// linking to it. Nothing is downloaded. `manual` checks (from the menu)
    /// also report "up to date" and errors; the startup check stays quiet.
    fn run_update_check(mut self: Pin<&mut Self>, manual: bool) {
        if manual {
            self.as_mut().status("Checking for updates…");
        }
        spawn(&self, github::check_update, move |mut this, res| {
            let current = env!("CARGO_PKG_VERSION");
            {
                let mut r = this.as_mut().rust_mut();
                match res {
                    Ok(Some(version)) => {
                        r.update = Some(format!("{APP_NAME} {version} is available (you have {current})."));
                        if manual {
                            r.status = format!("{APP_NAME} {version} is available");
                        }
                    }
                    Ok(None) if manual => r.status = format!("{APP_NAME} {current} is the latest version"),
                    Err(e) if manual => r.status = format!("Update check failed: {e}"),
                    _ => {}
                }
            }
            this.publish();
        });
    }

    pub fn dismiss_update(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().update = None;
        self.publish();
    }

    // ----- settings ------------------------------------------------------

    pub fn set_default_unlocker(mut self: Pin<&mut Self>, index: i32) {
        if let Some(k) = kind_at(&UNLOCKERS, index) {
            self.as_mut().settings_changed(|s| s.unlocker = k);
        }
    }

    pub fn set_default_method(self: Pin<&mut Self>, index: i32) {
        self.settings_changed(|s| s.method = method_at(index));
    }

    pub fn set_check_updates(self: Pin<&mut Self>, enabled: bool) {
        self.settings_changed(|s| s.check_updates = enabled);
    }

    fn settings_changed(mut self: Pin<&mut Self>, change: impl FnOnce(&mut Settings)) {
        {
            let mut r = self.as_mut().rust_mut();
            if let Err(e) = r.change_settings(change) {
                r.status = e;
            }
        }
        self.publish();
    }

    pub fn set_folder(mut self: Pin<&mut Self>, folder: i32, url: &QString) {
        let (Some(folder), Some(path)) = (folder_at(folder), url_path(url)) else {
            return;
        };
        self.as_mut().rust_mut().set_folder(folder, Some(path));
        self.publish();
    }

    pub fn reset_folder(mut self: Pin<&mut Self>, folder: i32) {
        if let Some(folder) = folder_at(folder) {
            self.as_mut().rust_mut().set_folder(folder, None);
            self.publish();
        }
    }

    pub fn download(mut self: Pin<&mut Self>, kind: i32) {
        let Some(kind) = kind_at(&IMPORTABLE, kind) else { return };
        if self.rust().downloading.contains(&kind) {
            return;
        }
        {
            let mut r = self.as_mut().rust_mut();
            r.downloading.push(kind);
            r.status = format!("Downloading the latest {kind} release from GitHub…");
        }
        self.as_mut().publish();
        spawn(
            &self,
            move || {
                let (tag, path) = github::download_latest(kind)?;
                let msg = unlocker::import_archive(kind, &path, None).map_err(|e| e.to_string())?;
                unlocker::set_source(kind, &tag);
                Ok::<_, String>(format!("Downloaded {kind} {tag}. {msg}"))
            },
            move |mut this, res| {
                {
                    let mut r = this.as_mut().rust_mut();
                    r.downloading.retain(|k| *k != kind);
                    r.status = res.unwrap_or_else(|e| format!("{kind} download failed: {e}"));
                    r.refresh_imports();
                }
                this.publish();
            },
        );
    }

    pub fn import_archive(self: Pin<&mut Self>, kind: i32, url: &QString) {
        let (Some(kind), Some(path)) = (kind_at(&IMPORTABLE, kind), url_path(url)) else {
            return;
        };
        self.import(kind, path, None);
    }

    fn import(mut self: Pin<&mut Self>, kind: Kind, path: PathBuf, password: Option<String>) {
        self.as_mut()
            .status(format!("Importing {kind} from {}…", path.display()));
        let (p, pw) = (path.clone(), password.clone());
        spawn(
            &self,
            move || unlocker::import_archive(kind, &p, pw.as_deref()),
            move |mut this, res| {
                let ask = {
                    let mut r = this.as_mut().rust_mut();
                    let ask = match res {
                        Ok(msg) => {
                            r.status = msg;
                            false
                        }
                        Err(ImportError::NeedPassword | ImportError::WrongPassword) => {
                            r.status = format!("{kind} archive is password-protected");
                            r.pending_password = Some((kind, path));
                            true
                        }
                        Err(e) => {
                            r.status = format!("Import failed: {e}");
                            false
                        }
                    };
                    r.refresh_imports();
                    ask
                };
                this.as_mut().publish();
                if ask {
                    this.password_needed(
                        QString::from(format!("{kind} archive password").as_str()),
                        password.is_some(),
                    );
                }
            },
        );
    }

    pub fn submit_password(mut self: Pin<&mut Self>, password: &QString) {
        if let Some((kind, path)) = self.as_mut().rust_mut().pending_password.take() {
            self.import(kind, path, Some(password.to_string()));
        }
    }

    pub fn cancel_password(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().pending_password = None;
    }
}

fn folder_at(index: i32) -> Option<Folder> {
    usize::try_from(index).ok().and_then(|i| FOLDERS.get(i).copied())
}

/// Fetch queued game icons until the queue is empty.
fn icon_worker(queue: Arc<Mutex<IconQueue>>, qt: BackendThread) {
    loop {
        let appid = {
            let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
            match q.waiting.pop_front() {
                Some(appid) => appid,
                None => {
                    q.workers -= 1;
                    return;
                }
            }
        };
        let path = steam::game_icon_path(appid);
        let url = match steam::fetch_game_icon(appid) {
            Ok(_) if path.is_file() => file_url(&path),
            _ => String::new(),
        };
        let _ = qt.queue(move |mut this| {
            this.as_mut().rust_mut().icons.insert(appid, url.clone());
            if !url.is_empty() {
                this.icon_ready(appid, QString::from(url.as_str()));
            }
        });
    }
}
