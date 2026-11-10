//! GTK4 user interface.
//!
//! All state lives in [`App`] (shared via `Rc`). Slow work (scanning,
//! network, file IO) runs on GIO's blocking thread pool and the result is
//! handled back on the main loop.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib, pango};

use crate::archive;
use crate::dlc::{self, Dlc};
use crate::github;
use crate::paths::{self, APP_NAME};
use crate::settings::{self, Settings};
use crate::steam::{self, Game, Platform};
use crate::unlocker::{self, ImportError, InstallRequest, Kind, Method};
use crate::wine;

const APP_ID: &str = "io.github.UnlockInstaller";
const UNLOCKERS: [Kind; 2] = [Kind::SmokeApi, Kind::CreamApi];
const IMPORTABLE: [Kind; 3] = [Kind::SmokeApi, Kind::CreamApi, Kind::Koaloader];

pub fn run() -> glib::ExitCode {
    let app = gtk::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| {
        // The plain GTK theme has no colours for these Adwaita-style classes.
        let css = gtk::CssProvider::new();
        css.load_from_string(
            ".success { color: #26a269; } \
             .error { color: #c01c28; } \
             .banner { background-color: alpha(@theme_selected_bg_color, 0.15); padding: 8px 12px; }",
        );
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    });
    app.connect_activate(|app| {
        // Starting the app again activates the running instance: show its
        // window instead of building a second one.
        match app.active_window() {
            Some(window) => window.present(),
            None => App::build(app).rescan(),
        }
    });
    // Don't let GTK interpret our command line.
    app.run_with_args::<&str>(&[])
}

/// Run `work` on a worker thread, then `done` on the GTK main loop.
fn run_bg<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(T) + 'static) {
    glib::spawn_future_local(async move {
        match gio::spawn_blocking(work).await {
            Ok(result) => done(result),
            Err(_) => eprintln!("background task panicked"),
        }
    });
}

fn label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l
}

fn wrapping_label() -> gtk::Label {
    let l = label("");
    l.set_wrap(true);
    l.set_wrap_mode(pango::WrapMode::WordChar);
    l.set_selectable(true);
    l
}

fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}

fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

/// A dimmed, right-aligned label for the left column of a form grid.
fn form_label(text: &str) -> gtk::Label {
    let l = label(text);
    l.set_xalign(1.0);
    l.add_css_class("dim-label");
    l
}

/// A framed list for settings-style rows.
fn boxed_list() -> (gtk::Frame, gtk::ListBox) {
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_show_separators(true);
    list.add_css_class("rich-list");
    let frame = gtk::Frame::new(None);
    frame.set_child(Some(&list));
    (frame, list)
}

/// A settings row: title and optional subtitle on the left, widgets on the right.
fn setting_row(title: &str, subtitle: Option<&gtk::Label>, controls: &[&gtk::Widget]) -> gtk::ListBoxRow {
    let text = vbox(2);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);
    text.append(&label(title));
    if let Some(sub) = subtitle {
        sub.add_css_class("dim-label");
        sub.add_css_class("caption");
        text.append(sub);
    }
    let row = hbox(8);
    row.append(&text);
    for c in controls {
        c.set_valign(gtk::Align::Center);
        row.append(*c);
    }
    gtk::ListBoxRow::builder().child(&row).activatable(false).build()
}

fn section_title(text: &str) -> gtk::Label {
    let l = label(text);
    l.add_css_class("heading");
    l
}

fn platform_name(p: Platform) -> &'static str {
    match p {
        Platform::Windows if cfg!(windows) => "Windows",
        Platform::Windows => "Windows (Proton)",
        Platform::Linux => "Native Linux",
    }
}

fn rel(base: &std::path::Path, p: &std::path::Path) -> String {
    let s = p.strip_prefix(base).unwrap_or(p).display().to_string();
    if s.is_empty() { "the game folder".into() } else { s }
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

enum DlcState {
    NotLoaded,
    Loading,
    Loaded(Vec<Dlc>, String),
    Failed(String),
}

struct Entry {
    game: Game,
    dlcs: DlcState,
}

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    selected: Option<u32>,
    /// appids in the order shown in the (filtered) game list
    visible: Vec<u32>,
    busy: bool,
    scanning: bool,
    dlc_checks: Vec<(u32, gtk::CheckButton)>,
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

impl Folder {
    fn title(self) -> &'static str {
        match self {
            Folder::Data => "Unlocker files",
            Folder::Cache => "Cache",
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

/// Widgets of the settings window, while it is open.
struct SettingsUi {
    window: gtk::Window,
    import_labels: Vec<(Kind, gtk::Label)>,
    /// Per folder: the label showing its path and its "Reset" button.
    folders: Vec<(Folder, gtk::Label, gtk::Button)>,
    status: gtk::Label,
}

struct App {
    window: gtk::ApplicationWindow,
    status: gtk::Label,
    rescan_btn: gtk::Button,
    uninstall_all: gio::SimpleAction,
    banner: gtk::Box,
    update_banner: gtk::Box,
    update_label: gtk::Label,
    no_games: gtk::Label,
    settings: RefCell<Settings>,
    settings_ui: RefCell<Option<SettingsUi>>,

    search: gtk::SearchEntry,
    games: gtk::ListBox,

    detail_stack: gtk::Stack,
    title: gtk::Label,
    info: gtk::Label,
    path: gtk::Label,
    apis: gtk::Label,
    installed: gtk::Label,
    launch_row: gtk::Box,
    launch_entry: gtk::Entry,
    unlocker_dd: gtk::DropDown,
    method_dd: gtk::DropDown,
    proxy_label: gtk::Label,
    proxy_dd: gtk::DropDown,
    proxy_model: gtk::StringList,
    install_btn: gtk::Button,
    uninstall_btn: gtk::Button,
    hint: gtk::Label,

    dlc_label: gtk::Label,
    dlc_spinner: gtk::Spinner,
    dlc_all: gtk::Button,
    dlc_none: gtk::Button,
    dlc_refresh: gtk::Button,
    dlc_list: gtk::ListBox,

    state: RefCell<State>,
    /// Set while we change widgets programmatically, to ignore their signals.
    suppress: Cell<bool>,
}

impl App {
    fn build(gapp: &gtk::Application) -> Rc<App> {
        let window = gtk::ApplicationWindow::builder()
            .application(gapp)
            .title(APP_NAME)
            .default_width(1100)
            .default_height(720)
            .build();

        // --- header bar ---------------------------------------------------
        let header = gtk::HeaderBar::new();
        let rescan_btn = gtk::Button::with_label("Rescan");
        rescan_btn.set_tooltip_text(Some("Scan the Steam libraries again"));
        header.pack_start(&rescan_btn);
        let menu = gio::Menu::new();
        menu.append(Some("Settings"), Some("win.settings"));
        menu.append(Some("Uninstall from all games…"), Some("win.uninstall-all"));
        menu.append(Some("Check for updates"), Some("win.check-updates"));
        menu.append(Some(&format!("About {APP_NAME}")), Some("win.about"));
        let menu_btn = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text("Main menu")
            .menu_model(&menu)
            .primary(true)
            .build();
        header.pack_end(&menu_btn);
        window.set_titlebar(Some(&header));
        let uninstall_all = gio::SimpleAction::new("uninstall-all", None);
        let settings_action = gio::SimpleAction::new("settings", None);
        let about_action = gio::SimpleAction::new("about", None);
        let updates_action = gio::SimpleAction::new("check-updates", None);
        for a in [&uninstall_all, &settings_action, &about_action, &updates_action] {
            window.add_action(a);
        }

        // --- "nothing imported yet" banner ----------------------------------
        let banner = hbox(12);
        banner.add_css_class("banner");
        let banner_text = label("No unlocker has been imported yet. Download or import one to get started.");
        banner_text.set_hexpand(true);
        banner_text.set_wrap(true);
        let banner_btn = gtk::Button::with_label("Open Settings");
        banner_btn.set_action_name(Some("win.settings"));
        banner_btn.set_valign(gtk::Align::Center);
        banner.append(&banner_text);
        banner.append(&banner_btn);

        // --- "update available" banner --------------------------------------
        let update_banner = hbox(12);
        update_banner.add_css_class("banner");
        update_banner.set_visible(false);
        let update_label = label("");
        update_label.set_hexpand(true);
        update_label.set_wrap(true);
        let update_link = gtk::LinkButton::with_label(&github::app_release_url(), "View release");
        update_link.set_valign(gtk::Align::Center);
        let update_dismiss = gtk::Button::with_label("Dismiss");
        update_dismiss.set_valign(gtk::Align::Center);
        update_banner.append(&update_label);
        update_banner.append(&update_link);
        update_banner.append(&update_dismiss);
        let b = update_banner.clone();
        update_dismiss.connect_clicked(move |_| b.set_visible(false));

        // --- game list (left) ----------------------------------------------
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("Search games…"));
        search.set_margin_start(8);
        search.set_margin_end(8);
        search.set_margin_top(8);
        let games = gtk::ListBox::new();
        games.set_selection_mode(gtk::SelectionMode::Single);
        games.add_css_class("navigation-sidebar");
        let no_games = label("");
        no_games.set_xalign(0.5);
        no_games.add_css_class("dim-label");
        no_games.set_margin_top(24);
        games.set_placeholder(Some(&no_games));
        let games_scroll = gtk::ScrolledWindow::builder()
            .child(&games)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let left = vbox(6);
        left.append(&search);
        left.append(&games_scroll);

        // --- details (right) -----------------------------------------------
        let title = label("");
        title.add_css_class("title-2");
        title.set_wrap(true);
        let info = label("");
        info.add_css_class("dim-label");
        let path = label("");
        path.add_css_class("dim-label");
        path.add_css_class("caption");
        path.set_ellipsize(pango::EllipsizeMode::Middle);
        path.set_selectable(true);
        let apis = wrapping_label();
        apis.add_css_class("dim-label");
        apis.add_css_class("caption");
        let installed = wrapping_label();
        let summary = vbox(4);
        summary.append(&title);
        summary.append(&info);
        summary.append(&path);
        summary.append(&apis);

        let unlocker_dd = gtk::DropDown::from_strings(&["SmokeAPI", "CreamAPI"]);
        let method_dd = gtk::DropDown::from_strings(&["Direct (replace steam_api)", "Koaloader proxy"]);
        let proxy_model = gtk::StringList::new(&[]);
        let proxy_dd = gtk::DropDown::new(Some(proxy_model.clone()), None::<gtk::Expression>);
        let proxy_label = form_label("Proxy DLL");
        let form = gtk::Grid::new();
        form.set_row_spacing(8);
        form.set_column_spacing(12);
        for (row, (l, dd)) in [
            (form_label("Unlocker"), &unlocker_dd),
            (form_label("Method"), &method_dd),
            (proxy_label.clone(), &proxy_dd),
        ]
        .into_iter()
        .enumerate()
        {
            dd.set_halign(gtk::Align::Start);
            form.attach(&l, 0, row as i32, 1, 1);
            form.attach(dd, 1, row as i32, 1, 1);
        }
        let hint = wrapping_label();
        hint.add_css_class("dim-label");
        hint.add_css_class("caption");

        let launch_row = hbox(6);
        let launch_entry = gtk::Entry::new();
        launch_entry.set_editable(false);
        launch_entry.set_hexpand(true);
        let copy_btn = gtk::Button::with_label("Copy");
        launch_row.append(&label("Steam launch options"));
        launch_row.append(&launch_entry);
        launch_row.append(&copy_btn);

        let install_btn = gtk::Button::with_label("Install");
        install_btn.add_css_class("suggested-action");
        let uninstall_btn = gtk::Button::with_label("Uninstall");
        uninstall_btn.add_css_class("destructive-action");
        let actions = hbox(8);
        installed.set_hexpand(true);
        installed.set_valign(gtk::Align::Center);
        actions.append(&installed);
        actions.append(&uninstall_btn);
        actions.append(&install_btn);

        let dlc_label = label("");
        dlc_label.add_css_class("heading");
        dlc_label.set_hexpand(true);
        dlc_label.set_ellipsize(pango::EllipsizeMode::End);
        let dlc_spinner = gtk::Spinner::new();
        let dlc_all = gtk::Button::with_label("All");
        let dlc_none = gtk::Button::with_label("None");
        let dlc_refresh = gtk::Button::with_label("Refresh");
        let dlc_header = hbox(6);
        dlc_header.append(&dlc_label);
        dlc_header.append(&dlc_spinner);
        dlc_header.append(&dlc_all);
        dlc_header.append(&dlc_none);
        dlc_header.append(&dlc_refresh);
        let dlc_list = gtk::ListBox::new();
        dlc_list.set_selection_mode(gtk::SelectionMode::None);
        let dlc_scroll = gtk::ScrolledWindow::builder()
            .child(&dlc_list)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let dlc_frame = gtk::Frame::new(None);
        dlc_frame.set_child(Some(&dlc_scroll));

        let details = vbox(12);
        details.set_margin_start(18);
        details.set_margin_end(18);
        details.set_margin_top(18);
        details.set_margin_bottom(12);
        for w in [
            summary.upcast_ref::<gtk::Widget>(),
            form.upcast_ref(),
            hint.upcast_ref(),
            launch_row.upcast_ref(),
            actions.upcast_ref(),
            dlc_header.upcast_ref(),
            dlc_frame.upcast_ref(),
        ] {
            details.append(w);
        }
        dlc_header.set_margin_top(6);

        let empty = label("Select a game to get started");
        empty.set_xalign(0.5);
        empty.add_css_class("title-3");
        empty.add_css_class("dim-label");
        let detail_stack = gtk::Stack::new();
        detail_stack.add_named(&empty, Some("empty"));
        detail_stack.add_named(&details, Some("details"));

        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_start_child(Some(&left));
        paned.set_end_child(Some(&detail_stack));
        paned.set_position(340);
        paned.set_shrink_start_child(false);
        paned.set_shrink_end_child(false);
        paned.set_resize_start_child(false);
        paned.set_vexpand(true);

        let status = label("");
        status.add_css_class("dim-label");
        status.set_ellipsize(pango::EllipsizeMode::End);
        status.set_margin_start(12);
        status.set_margin_end(12);
        status.set_margin_top(5);
        status.set_margin_bottom(6);

        let root = vbox(0);
        root.append(&update_banner);
        root.append(&banner);
        root.append(&paned);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&status);
        window.set_child(Some(&root));

        let app = Rc::new(App {
            window,
            status,
            rescan_btn,
            uninstall_all,
            banner,
            update_banner,
            update_label,
            no_games,
            settings: RefCell::new(Settings::load()),
            settings_ui: RefCell::new(None),
            search,
            games,
            detail_stack,
            title,
            info,
            path,
            apis,
            installed,
            launch_row,
            launch_entry,
            unlocker_dd,
            method_dd,
            proxy_label,
            proxy_dd,
            proxy_model,
            install_btn,
            uninstall_btn,
            hint,
            dlc_label,
            dlc_spinner,
            dlc_all,
            dlc_none,
            dlc_refresh,
            dlc_list,
            state: RefCell::new(State::default()),
            suppress: Cell::new(false),
        });

        // --- signals ---------------------------------------------------------
        let a = app.clone();
        app.rescan_btn.connect_clicked(move |_| a.rescan());
        let a = app.clone();
        app.uninstall_all.connect_activate(move |_, _| a.confirm_uninstall_all());
        let a = app.clone();
        settings_action.connect_activate(move |_, _| a.open_settings());
        let a = app.clone();
        about_action.connect_activate(move |_, _| a.show_about());
        let a = app.clone();
        updates_action.connect_activate(move |_, _| a.check_updates(true));
        let a = app.clone();
        app.search.connect_search_changed(move |_| a.refresh_games());
        let a = app.clone();
        app.games.connect_row_selected(move |_, row| {
            if a.suppress.get() {
                return;
            }
            let Some(row) = row else { return };
            let appid = a.state.borrow().visible.get(row.index() as usize).copied();
            if let Some(appid) = appid {
                a.select(appid);
            }
        });
        let a = app.clone();
        copy_btn.connect_clicked(move |_| {
            a.launch_entry.clipboard().set_text(&a.launch_entry.text());
            a.set_status("Launch options copied to clipboard");
        });
        for dd in [&app.unlocker_dd, &app.method_dd, &app.proxy_dd] {
            let a = app.clone();
            dd.connect_selected_notify(move |_| {
                if !a.suppress.get() {
                    a.update_actions();
                }
            });
        }
        let a = app.clone();
        app.install_btn.connect_clicked(move |_| a.install());
        let a = app.clone();
        app.uninstall_btn.connect_clicked(move |_| a.uninstall());
        let a = app.clone();
        app.dlc_all.connect_clicked(move |_| a.set_all_dlcs(true));
        let a = app.clone();
        app.dlc_none.connect_clicked(move |_| a.set_all_dlcs(false));
        let a = app.clone();
        app.dlc_refresh.connect_clicked(move |_| a.load_dlcs(true));

        app.refresh_imports();
        app.refresh_detail();
        app.window.present();
        if wine::detected() {
            app.warn_wine();
        }
        if app.settings.borrow().check_updates {
            app.check_updates(false);
        }
        app
    }

    /// Show a message in the status bar (and in the settings window, if open).
    fn set_status(&self, s: impl AsRef<str>) {
        self.status.set_text(s.as_ref());
        self.status.set_tooltip_text(Some(s.as_ref()));
        if let Some(ui) = self.settings_ui.borrow().as_ref() {
            ui.status.set_text(s.as_ref());
        }
    }

    /// The window dialogs should be attached to.
    fn dialog_parent(&self) -> gtk::Window {
        match self.settings_ui.borrow().as_ref() {
            Some(ui) => ui.window.clone(),
            None => self.window.clone().upcast(),
        }
    }

    fn selected_kind(&self) -> Kind {
        UNLOCKERS[(self.unlocker_dd.selected() as usize).min(1)]
    }

    fn selected_method(&self) -> Method {
        if self.method_dd.selected() == 1 { Method::Koaloader } else { Method::Direct }
    }

    fn selected_proxy(&self) -> Option<String> {
        self.proxy_dd
            .selected_item()
            .and_downcast::<gtk::StringObject>()
            .map(|s| s.string().to_string())
    }

    // ----- refreshing widgets from state -------------------------------------

    fn refresh_imports(&self) {
        if let Some(ui) = self.settings_ui.borrow().as_ref() {
            for (kind, l) in &ui.import_labels {
                match unlocker::import_summary(*kind) {
                    Some(s) => l.set_text(&s),
                    None => l.set_text("Not imported"),
                }
            }
        }
        self.refresh_folders();
        self.banner
            .set_visible(UNLOCKERS.iter().all(|k| unlocker::import_summary(*k).is_none()));
        let current = self.selected_proxy();
        let proxies = unlocker::koaloader_proxies();
        let strs: Vec<&str> = proxies.iter().map(String::as_str).collect();
        self.suppress.set(true);
        self.proxy_model.splice(0, self.proxy_model.n_items(), &strs);
        if let Some(i) = current.and_then(|c| proxies.iter().position(|p| *p == c)) {
            self.proxy_dd.set_selected(i as u32);
        }
        self.suppress.set(false);
        self.update_actions();
    }

    fn refresh_games(&self) {
        let needle = self.search.text().to_lowercase();
        let rows: Vec<(u32, String, bool, String)> = {
            let st = self.state.borrow();
            st.entries
                .iter()
                .filter(|e| needle.is_empty() || e.game.name.to_lowercase().contains(&needle))
                .map(|e| {
                    (
                        e.game.appid,
                        e.game.name.clone(),
                        e.game.platform() == Platform::Linux,
                        installed_label(&e.game),
                    )
                })
                .collect()
        };
        let selected = self.state.borrow().selected;

        self.suppress.set(true);
        self.games.remove_all();
        let mut select_row = None;
        for (i, (appid, name, native, inst)) in rows.iter().enumerate() {
            let row = vbox(2);
            row.set_margin_top(4);
            row.set_margin_bottom(4);
            let n = label(name);
            n.set_ellipsize(pango::EllipsizeMode::End);
            n.set_tooltip_text(Some(name));
            row.append(&n);
            let sub = if !inst.is_empty() {
                Some((inst.as_str(), "success"))
            } else if *native {
                Some((platform_name(Platform::Linux), "dim-label"))
            } else {
                None
            };
            if let Some((text, class)) = sub {
                let t = label(text);
                t.add_css_class("caption");
                t.add_css_class(class);
                t.set_ellipsize(pango::EllipsizeMode::End);
                row.append(&t);
            }
            self.games.append(&row);
            if Some(*appid) == selected {
                select_row = Some(i as i32);
            }
        }
        if let Some(i) = select_row {
            self.games.select_row(self.games.row_at_index(i).as_ref());
        }
        self.suppress.set(false);
        self.state.borrow_mut().visible = rows.into_iter().map(|r| r.0).collect();
        self.update_actions();
    }

    fn refresh_detail(&self) {
        let st = self.state.borrow();
        let Some(e) = st.selected_entry() else {
            self.detail_stack.set_visible_child_name("empty");
            drop(st);
            self.update_actions();
            return;
        };
        let g = &e.game;
        self.detail_stack.set_visible_child_name("details");
        self.title.set_text(&g.name);
        self.info
            .set_text(&format!("App ID {}  ·  {}", g.appid, platform_name(g.platform())));
        self.path.set_text(&g.dir.display().to_string());
        let apis: Vec<String> = g
            .platform_apis()
            .map(|a| format!("{} ({}-bit)", rel(&g.dir, &a.path()), a.arch.bits()))
            .collect();
        self.apis.set_text(&format!("Steam API: {}", apis.join(", ")));

        if g.installed.is_empty() {
            self.installed.set_text("No unlocker installed");
            self.installed.remove_css_class("success");
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
            self.installed.set_text(&format!("Installed: {}", parts.join("; ")));
            self.installed.add_css_class("success");
        }
        let koa_proxy = g.installed.iter().find_map(|i| i.proxy.clone());
        self.launch_row
            .set_visible(unlocker::NEEDS_LAUNCH_OPTIONS && koa_proxy.is_some());
        if let Some(p) = &koa_proxy {
            self.launch_entry.set_text(&unlocker::launch_options(p));
        }

        // Preselect options matching the current install.
        let first = g.installed.first().cloned();
        drop(st);
        self.suppress.set(true);
        if let Some(inst) = first {
            if let Some(i) = UNLOCKERS.iter().position(|k| k.name() == inst.unlocker) {
                self.unlocker_dd.set_selected(i as u32);
            }
            self.method_dd
                .set_selected(if inst.method == Method::Koaloader { 1 } else { 0 });
            if let Some(p) = inst.proxy {
                if let Some(i) = unlocker::koaloader_proxies().iter().position(|x| *x == p) {
                    self.proxy_dd.set_selected(i as u32);
                }
            }
        } else {
            // Nothing installed: start from the defaults chosen in Settings
            // (update_actions falls back to direct where Koaloader can't work).
            let s = self.settings.borrow();
            let i = UNLOCKERS.iter().position(|k| *k == s.unlocker).unwrap_or(0);
            self.unlocker_dd.set_selected(i as u32);
            self.method_dd
                .set_selected(if s.method == Method::Koaloader { 1 } else { 0 });
        }
        self.suppress.set(false);
        self.update_actions();
    }

    /// Enable/disable/show/hide controls based on current state.
    fn update_actions(&self) {
        let st = self.state.borrow();
        let entry = st.selected_entry();
        let busy = st.busy;
        let koaloader_possible = entry.is_some_and(|e| {
            e.game.platform() == Platform::Windows && !e.game.exe_dirs.is_empty()
        });
        let has_installs = entry.is_some_and(|e| !e.game.installed.is_empty());
        let any_installed = st.entries.iter().any(|e| !e.game.installed.is_empty());
        let exe_dirs: Vec<String> = entry
            .map(|e| e.game.exe_dirs.iter().map(|d| rel(&e.game.dir, &d.dir)).collect())
            .unwrap_or_default();
        let scanning = st.scanning;
        let has_entry = entry.is_some();
        drop(st);

        let kind = self.selected_kind();
        let method_ok = koaloader_possible && kind == Kind::SmokeApi;
        if !method_ok && self.method_dd.selected() != 0 {
            self.suppress.set(true);
            self.method_dd.set_selected(0);
            self.suppress.set(false);
        }
        self.method_dd.set_sensitive(method_ok);
        let koaloader = self.selected_method() == Method::Koaloader;
        let proxy = self.selected_proxy();
        self.proxy_label.set_visible(koaloader);
        self.proxy_dd.set_visible(koaloader);
        self.hint.set_visible(koaloader);
        if koaloader {
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
            self.hint.set_text(&text);
        }
        // Say why Install is unavailable instead of just greying it out.
        let blocker = if unlocker::import_summary(kind).is_none() {
            Some(format!("{kind} has not been imported yet (see Settings)"))
        } else if koaloader && proxy.is_none() {
            Some("Koaloader has not been imported yet (see Settings)".to_string())
        } else {
            None
        };
        self.install_btn.set_label(if has_installs { "Reinstall" } else { "Install" });
        self.install_btn.set_tooltip_text(blocker.as_deref());
        self.install_btn
            .set_sensitive(!busy && has_entry && blocker.is_none());
        self.uninstall_btn.set_sensitive(!busy && has_installs);
        self.rescan_btn.set_sensitive(!busy && !scanning);
        self.rescan_btn.set_label(if scanning { "Scanning…" } else { "Rescan" });
        self.no_games.set_text(if scanning { "Scanning…" } else { "No games found" });
        self.uninstall_all.set_enabled(!busy && any_installed);
    }

    fn refresh_dlcs(self: &Rc<Self>) {
        self.dlc_list.remove_all();
        let mut checks = Vec::new();
        let (text, spinning, loaded, failed) = {
            let st = self.state.borrow();
            match st.selected_entry().map(|e| &e.dlcs) {
                None | Some(DlcState::NotLoaded) => (String::new(), false, false, false),
                Some(DlcState::Loading) => ("Fetching DLC list…".into(), true, false, false),
                Some(DlcState::Failed(e)) => (format!("Failed to fetch DLCs: {e}"), false, false, true),
                Some(DlcState::Loaded(list, source)) => {
                    for d in list {
                        let cb = gtk::CheckButton::with_label(&d.name);
                        cb.set_active(d.enabled);
                        cb.set_hexpand(true);
                        let a = self.clone();
                        let id = d.id;
                        cb.connect_toggled(move |cb| a.toggle_dlc(id, cb.is_active()));
                        let id_label = label(&d.id.to_string());
                        id_label.add_css_class("dim-label");
                        id_label.add_css_class("caption");
                        let row = hbox(8);
                        row.set_margin_start(6);
                        row.set_margin_end(10);
                        row.set_margin_top(2);
                        row.set_margin_bottom(2);
                        row.append(&cb);
                        row.append(&id_label);
                        self.dlc_list.append(&row);
                        checks.push((d.id, cb));
                    }
                    let text = if list.is_empty() {
                        "This game has no DLCs on Steam".to_string()
                    } else {
                        format!("DLCs · from {source}")
                    };
                    (text, false, !list.is_empty(), false)
                }
            }
        };
        self.state.borrow_mut().dlc_checks = checks;
        self.dlc_label.set_text(&text);
        self.dlc_spinner.set_spinning(spinning);
        self.dlc_spinner.set_visible(spinning);
        self.dlc_all.set_visible(loaded);
        self.dlc_none.set_visible(loaded);
        self.dlc_refresh.set_visible(!spinning && self.state.borrow().selected.is_some());
        self.dlc_refresh.set_label(if failed { "Retry" } else { "Refresh" });
        if loaded {
            self.update_dlc_count();
        }
    }

    fn update_dlc_count(&self) {
        let st = self.state.borrow();
        if let Some(Entry { dlcs: DlcState::Loaded(list, source), .. }) = st.selected_entry() {
            let on = list.iter().filter(|d| d.enabled).count();
            self.dlc_label
                .set_text(&format!("DLCs · {on} of {} enabled · from {source}", list.len()));
        }
    }

    // ----- actions -----------------------------------------------------------

    fn rescan(self: &Rc<Self>) {
        {
            let mut st = self.state.borrow_mut();
            if st.scanning {
                return;
            }
            st.scanning = true;
        }
        self.set_status("Scanning Steam libraries…");
        self.update_actions();
        let a = self.clone();
        run_bg(steam::scan_games, move |res| {
            {
                let mut st = a.state.borrow_mut();
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
                            Entry { game, dlcs }
                        })
                        .collect();
                    if st.selected_entry().is_none() {
                        st.selected = None;
                    }
                }
            }
            match res {
                Ok(g) => a.set_status(format!("Found {} game(s) using the Steamworks API", g.len())),
                Err(e) => a.set_status(format!("Scan failed: {e}")),
            }
            a.refresh_games();
            a.refresh_detail();
            a.refresh_dlcs();
        });
    }

    fn select(self: &Rc<Self>, appid: u32) {
        let needs_load = {
            let mut st = self.state.borrow_mut();
            st.selected = Some(appid);
            matches!(st.entry(appid).map(|e| &e.dlcs), Some(DlcState::NotLoaded))
        };
        self.refresh_detail();
        if needs_load {
            self.load_dlcs(false);
        } else {
            self.refresh_dlcs();
        }
    }

    fn load_dlcs(self: &Rc<Self>, refresh: bool) {
        let Some(appid) = self.state.borrow().selected else { return };
        if let Some(e) = self.state.borrow_mut().entry_mut(appid) {
            e.dlcs = DlcState::Loading;
        }
        self.refresh_dlcs();
        let a = self.clone();
        run_bg(move || dlc::load_dlcs(appid, refresh), move |res| {
            if let Some(e) = a.state.borrow_mut().entry_mut(appid) {
                e.dlcs = match res {
                    Ok((list, source)) => DlcState::Loaded(list, source.to_string()),
                    Err(err) => DlcState::Failed(err),
                };
            }
            if a.state.borrow().selected == Some(appid) {
                a.refresh_dlcs();
            }
        });
    }

    fn toggle_dlc(&self, id: u32, enabled: bool) {
        if self.suppress.get() {
            return;
        }
        {
            let mut st = self.state.borrow_mut();
            let Some(appid) = st.selected else { return };
            if let Some(Entry { dlcs: DlcState::Loaded(list, _), .. }) = st.entry_mut(appid) {
                if let Some(d) = list.iter_mut().find(|d| d.id == id) {
                    d.enabled = enabled;
                }
                dlc::save_selection(appid, list);
            }
        }
        self.update_dlc_count();
    }

    fn set_all_dlcs(&self, enabled: bool) {
        let checks = {
            let mut st = self.state.borrow_mut();
            let Some(appid) = st.selected else { return };
            if let Some(Entry { dlcs: DlcState::Loaded(list, _), .. }) = st.entry_mut(appid) {
                list.iter_mut().for_each(|d| d.enabled = enabled);
                dlc::save_selection(appid, list);
            }
            st.dlc_checks.iter().map(|(_, cb)| cb.clone()).collect::<Vec<_>>()
        };
        self.suppress.set(true);
        checks.iter().for_each(|cb| cb.set_active(enabled));
        self.suppress.set(false);
        self.update_dlc_count();
    }

    fn set_busy(&self, busy: bool) {
        self.state.borrow_mut().busy = busy;
        self.update_actions();
    }

    /// Store a game updated by a background job and refresh the views.
    fn finish_job(self: &Rc<Self>, msg: String, games: Vec<Game>) {
        {
            let mut st = self.state.borrow_mut();
            st.busy = false;
            for g in games {
                if let Some(e) = st.entry_mut(g.appid) {
                    e.game = g;
                }
            }
        }
        self.set_status(msg);
        self.refresh_games();
        self.refresh_detail();
    }

    fn install(self: &Rc<Self>) {
        let kind = self.selected_kind();
        let method = self.selected_method();
        let proxy = self.selected_proxy().unwrap_or_default();
        let (game, dlcs) = {
            let st = self.state.borrow();
            let Some(e) = st.selected_entry() else { return };
            let dlcs = match &e.dlcs {
                DlcState::Loaded(d, _) => d.clone(),
                _ if kind == Kind::CreamApi => {
                    drop(st);
                    self.set_status("CreamAPI needs the DLC list; wait for it to load (or retry)");
                    return;
                }
                _ => Vec::new(),
            };
            (e.game.clone(), dlcs)
        };
        let req = InstallRequest { unlocker: kind, method, proxy, dlcs };
        self.set_busy(true);
        self.set_status(format!("Installing {kind} into {}…", game.name));
        let a = self.clone();
        run_bg(
            move || {
                let res = unlocker::install(&game, &req);
                let mut game = game;
                game.refresh_installed();
                (res, game)
            },
            move |(res, game)| {
                a.finish_job(res.unwrap_or_else(|e| format!("Install failed: {e}")), vec![game]);
            },
        );
    }

    fn uninstall(self: &Rc<Self>) {
        let Some(game) = self.state.borrow().selected_entry().map(|e| e.game.clone()) else {
            return;
        };
        self.set_busy(true);
        self.set_status(format!("Uninstalling from {}…", game.name));
        let a = self.clone();
        run_bg(
            move || {
                let res = unlocker::uninstall(&game);
                let mut game = game;
                game.refresh_installed();
                (res, game)
            },
            move |(res, game)| {
                a.finish_job(res.unwrap_or_else(|e| format!("Uninstall failed: {e}")), vec![game]);
            },
        );
    }

    fn confirm_uninstall_all(self: &Rc<Self>) {
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message("Uninstall from all games?")
            .detail("Every detected unlocker will be removed and the original files restored.")
            .buttons(["Cancel", "Uninstall all"])
            .cancel_button(0)
            .default_button(0)
            .build();
        let a = self.clone();
        dialog.choose(Some(&self.window), gio::Cancellable::NONE, move |res| {
            if res == Ok(1) {
                a.uninstall_all();
            }
        });
    }

    fn uninstall_all(self: &Rc<Self>) {
        let games: Vec<Game> = self
            .state
            .borrow()
            .entries
            .iter()
            .filter(|e| !e.game.installed.is_empty())
            .map(|e| e.game.clone())
            .collect();
        if games.is_empty() {
            return;
        }
        self.set_busy(true);
        self.set_status(format!("Uninstalling from {} game(s)…", games.len()));
        let a = self.clone();
        run_bg(
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
            move |(msg, games)| a.finish_job(msg, games),
        );
    }

    /// Update the folder rows of the settings window, if it is open.
    fn refresh_folders(&self) {
        let custom = {
            let mut s = self.settings.borrow_mut();
            [Folder::Data, Folder::Cache].map(|f| (f, f.slot(&mut s).is_some()))
        };
        if let Some(ui) = self.settings_ui.borrow().as_ref() {
            for (folder, path, reset) in &ui.folders {
                path.set_text(&folder.current().display().to_string());
                reset.set_sensitive(custom.contains(&(*folder, true)));
            }
        }
    }

    fn pick_folder(self: &Rc<Self>, folder: Folder) {
        let dialog = gtk::FileDialog::builder()
            .title(format!("Select the {} folder", folder.title().to_lowercase()))
            .modal(true)
            .initial_folder(&gio::File::for_path(folder.current()))
            .build();
        let a = self.clone();
        dialog.select_folder(Some(&self.dialog_parent()), gio::Cancellable::NONE, move |res| {
            if let Some(path) = res.ok().and_then(|f| f.path()) {
                a.set_folder(folder, Some(path));
            }
        });
    }

    /// Move a folder to `path`, or back to its default location.
    fn set_folder(&self, folder: Folder, path: Option<PathBuf>) {
        if self.change_settings(|s| *folder.slot(s) = path) {
            self.set_status(format!(
                "{} folder is now {} (existing files were not moved)",
                folder.title(),
                folder.current().display()
            ));
        }
    }

    /// Ask GitHub whether a newer release exists and, if so, show a banner
    /// linking to it. Nothing is downloaded. `manual` checks (from the menu)
    /// also report "up to date" and errors; the startup check stays quiet.
    fn check_updates(self: &Rc<Self>, manual: bool) {
        if manual {
            self.set_status("Checking for updates…");
        }
        let a = self.clone();
        run_bg(github::check_update, move |res| match res {
            Ok(Some(version)) => {
                a.update_label.set_text(&format!(
                    "{APP_NAME} {version} is available (you have {}).",
                    env!("CARGO_PKG_VERSION")
                ));
                a.update_banner.set_visible(true);
                if manual {
                    a.set_status(format!("{APP_NAME} {version} is available"));
                }
            }
            Ok(None) if manual => a.set_status(format!(
                "{APP_NAME} {} is the latest version",
                env!("CARGO_PKG_VERSION")
            )),
            Err(e) if manual => a.set_status(format!("Update check failed: {e}")),
            _ => {}
        });
    }

    /// Shown when the Windows build is started under Wine or Proton.
    fn warn_wine(self: &Rc<Self>) {
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message("Running under Wine or Proton")
            .detail(format!(
                "This is the Windows version of {APP_NAME}, and it is running under Wine or Proton. \
                 It cannot see your Steam libraries properly from here.\n\n\
                 Run the native Linux version instead: it handles Proton games directly."
            ))
            .buttons(["Quit", "Continue anyway"])
            .cancel_button(1)
            .default_button(0)
            .build();
        let a = self.clone();
        dialog.choose(Some(&self.window), gio::Cancellable::NONE, move |res| {
            if res == Ok(0) {
                a.window.close();
            }
        });
    }

    fn show_about(&self) {
        gtk::AboutDialog::builder()
            .transient_for(&self.window)
            .modal(true)
            .program_name(APP_NAME)
            .version(env!("CARGO_PKG_VERSION"))
            .comments("Installs SmokeAPI and CreamAPI DLC unlockers into Steam games.")
            .build()
            .present();
    }

    fn open_settings(self: &Rc<Self>) {
        if let Some(ui) = self.settings_ui.borrow().as_ref() {
            ui.window.present();
            return;
        }
        let window = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Settings")
            .default_width(600)
            .build();

        // --- unlockers ---
        let (unlockers_frame, unlockers) = boxed_list();
        let mut import_labels = Vec::new();
        for kind in IMPORTABLE {
            let status = label("");
            status.set_wrap(true);
            status.set_wrap_mode(pango::WrapMode::WordChar);
            let mut controls: Vec<gtk::Widget> = Vec::new();
            if let Some(repo) = github::repo(kind) {
                let d = gtk::Button::with_label("Download");
                d.set_tooltip_text(Some(&format!(
                    "Download the latest release from github.com/{repo}"
                )));
                let a = self.clone();
                d.connect_clicked(move |b| a.download(kind, b));
                controls.push(d.upcast());
            }
            let b = gtk::Button::with_label("Import…");
            b.set_tooltip_text(Some(&format!(
                "Import a {kind} release archive (zip, 7z, rar or tar)"
            )));
            let a = self.clone();
            b.connect_clicked(move |_| a.pick_archive(kind));
            controls.push(b.upcast());
            let refs: Vec<&gtk::Widget> = controls.iter().collect();
            unlockers.append(&setting_row(kind.name(), Some(&status), &refs));
            import_labels.push((kind, status));
        }
        let unlockers_note = wrapping_label();
        unlockers_note.set_selectable(false);
        unlockers_note.add_css_class("dim-label");
        unlockers_note.add_css_class("caption");
        unlockers_note.set_text(
            "No unlocker files ship with the app. Download the latest official release, or import \
             an archive you downloaded yourself. Koaloader is only needed for the Koaloader \
             install method.",
        );

        // --- defaults ---
        let (defaults_frame, defaults) = boxed_list();
        let unlocker_dd = gtk::DropDown::from_strings(&["SmokeAPI", "CreamAPI"]);
        let method_dd = gtk::DropDown::from_strings(&["Direct", "Koaloader"]);
        {
            let s = self.settings.borrow();
            let i = UNLOCKERS.iter().position(|k| *k == s.unlocker).unwrap_or(0);
            unlocker_dd.set_selected(i as u32);
            method_dd.set_selected(if s.method == Method::Koaloader { 1 } else { 0 });
        }
        let a = self.clone();
        unlocker_dd.connect_selected_notify(move |dd| {
            a.change_settings(|s| s.unlocker = UNLOCKERS[(dd.selected() as usize).min(1)]);
        });
        let a = self.clone();
        method_dd.connect_selected_notify(move |dd| {
            a.change_settings(|s| {
                s.method = if dd.selected() == 1 { Method::Koaloader } else { Method::Direct };
            });
        });
        defaults.append(&setting_row(
            "Default unlocker",
            Some(&label("Preselected for games without an unlocker")),
            &[unlocker_dd.upcast_ref()],
        ));
        defaults.append(&setting_row(
            "Default install method",
            Some(&label("Koaloader applies to SmokeAPI on Windows games only")),
            &[method_dd.upcast_ref()],
        ));

        let updates_switch = gtk::Switch::new();
        updates_switch.set_active(self.settings.borrow().check_updates);
        let a = self.clone();
        updates_switch.connect_active_notify(move |sw| {
            a.change_settings(|s| s.check_updates = sw.is_active());
        });
        defaults.append(&setting_row(
            "Check for updates on startup",
            Some(&label("Shows a link to the new release; nothing is downloaded")),
            &[updates_switch.upcast_ref()],
        ));

        // --- folders ---
        let (folders_frame, folders_list) = boxed_list();
        let mut folders = Vec::new();
        for (folder, tooltip) in [
            (Folder::Data, "Where imported unlocker archives are extracted to"),
            (Folder::Cache, "DLC lists and downloaded release archives"),
        ] {
            let path = label("");
            path.set_ellipsize(pango::EllipsizeMode::Middle);
            path.set_selectable(true);
            let change = gtk::Button::with_label("Change…");
            let a = self.clone();
            change.connect_clicked(move |_| a.pick_folder(folder));
            let reset = gtk::Button::with_label("Reset");
            reset.set_tooltip_text(Some("Use the default folder again"));
            let a = self.clone();
            reset.connect_clicked(move |_| a.set_folder(folder, None));
            let row = setting_row(folder.title(), Some(&path), &[change.upcast_ref(), reset.upcast_ref()]);
            row.set_tooltip_text(Some(tooltip));
            folders_list.append(&row);
            folders.push((folder, path, reset));
        }
        let settings_path = label(&settings::location());
        settings_path.set_ellipsize(pango::EllipsizeMode::Middle);
        settings_path.set_selectable(true);
        folders_list.append(&setting_row("Settings are stored in", Some(&settings_path), &[]));
        let folders_note = wrapping_label();
        folders_note.set_selectable(false);
        folders_note.add_css_class("dim-label");
        folders_note.add_css_class("caption");
        folders_note.set_text(&if cfg!(windows) {
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
        });

        let status = wrapping_label();
        status.add_css_class("dim-label");
        status.set_text(&self.status.text());

        let body = vbox(8);
        body.set_margin_top(18);
        body.set_margin_bottom(18);
        body.set_margin_start(18);
        body.set_margin_end(18);
        body.append(&section_title("Unlockers"));
        body.append(&unlockers_frame);
        body.append(&unlockers_note);
        let defaults_title = section_title("General");
        defaults_title.set_margin_top(12);
        body.append(&defaults_title);
        body.append(&defaults_frame);
        let folders_title = section_title("Folders");
        folders_title.set_margin_top(12);
        body.append(&folders_title);
        body.append(&folders_frame);
        body.append(&folders_note);
        status.set_margin_top(12);
        body.append(&status);
        let scroll = gtk::ScrolledWindow::builder()
            .child(&body)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(760)
            .build();
        window.set_child(Some(&scroll));

        let a = self.clone();
        window.connect_close_request(move |_| {
            a.settings_ui.replace(None);
            glib::Propagation::Proceed
        });
        self.settings_ui
            .replace(Some(SettingsUi { window: window.clone(), import_labels, folders, status }));
        self.refresh_imports();
        window.present();
    }

    /// Change and save the settings, then re-apply them to the views.
    /// Returns whether they could be saved.
    fn change_settings(&self, change: impl FnOnce(&mut Settings)) -> bool {
        let saved = {
            let mut s = self.settings.borrow_mut();
            change(&mut s);
            s.save()
        };
        // the unlocker folder may have changed
        self.refresh_imports();
        self.refresh_detail();
        match saved {
            Ok(()) => true,
            Err(e) => {
                self.set_status(format!("Could not save the settings: {e}"));
                false
            }
        }
    }

    fn pick_archive(self: &Rc<Self>, kind: Kind) {
        let archives = gtk::FileFilter::new();
        archives.set_name(Some("Archives (zip, 7z, rar, tar)"));
        for p in archive::PATTERNS {
            archives.add_pattern(p);
            archives.add_pattern(&p.to_ascii_uppercase());
        }
        let all = gtk::FileFilter::new();
        all.set_name(Some("All files"));
        all.add_pattern("*");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&archives);
        filters.append(&all);
        let dialog = gtk::FileDialog::builder()
            .title(format!("Select the {kind} release archive"))
            .modal(true)
            .filters(&filters)
            .default_filter(&archives)
            .build();
        let a = self.clone();
        dialog.open(Some(&self.dialog_parent()), gio::Cancellable::NONE, move |res| {
            if let Some(path) = res.ok().and_then(|f| f.path()) {
                a.import_archive(kind, path, None);
            }
        });
    }

    fn download(self: &Rc<Self>, kind: Kind, button: &gtk::Button) {
        button.set_sensitive(false);
        self.set_status(format!("Downloading the latest {kind} release from GitHub…"));
        let (a, button) = (self.clone(), button.clone());
        run_bg(
            move || {
                let (tag, path) = github::download_latest(kind)?;
                let msg = unlocker::import_archive(kind, &path, None).map_err(|e| e.to_string())?;
                unlocker::set_source(kind, &tag);
                Ok::<_, String>(format!("Downloaded {kind} {tag}. {msg}"))
            },
            move |res| {
                button.set_sensitive(true);
                a.set_status(res.unwrap_or_else(|e| format!("{kind} download failed: {e}")));
                a.refresh_imports();
            },
        );
    }

    fn import_archive(self: &Rc<Self>, kind: Kind, path: PathBuf, password: Option<String>) {
        self.set_status(format!("Importing {kind} from {}…", path.display()));
        let a = self.clone();
        let (p, pw) = (path.clone(), password.clone());
        run_bg(
            move || unlocker::import_archive(kind, &p, pw.as_deref()),
            move |res| {
                match res {
                    Ok(msg) => a.set_status(msg),
                    Err(ImportError::NeedPassword | ImportError::WrongPassword) => {
                        a.set_status(format!("{kind} archive is password-protected"));
                        a.ask_password(kind, path, password.is_some());
                    }
                    Err(e) => a.set_status(format!("Import failed: {e}")),
                }
                a.refresh_imports();
            },
        );
    }

    fn ask_password(self: &Rc<Self>, kind: Kind, path: PathBuf, wrong: bool) {
        let win = gtk::Window::builder()
            .transient_for(&self.dialog_parent())
            .modal(true)
            .title(format!("{kind} archive password"))
            .default_width(380)
            .resizable(false)
            .build();
        let body = vbox(10);
        body.set_margin_top(12);
        body.set_margin_bottom(12);
        body.set_margin_start(12);
        body.set_margin_end(12);
        body.append(&label(if wrong {
            "Wrong password, try again:"
        } else {
            "This archive is password-protected. Enter its password:"
        }));
        let entry = gtk::PasswordEntry::new();
        entry.set_show_peek_icon(true);
        body.append(&entry);
        let buttons = hbox(8);
        buttons.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label("Cancel");
        let ok = gtk::Button::with_label("Import");
        ok.add_css_class("suggested-action");
        buttons.append(&cancel);
        buttons.append(&ok);
        body.append(&buttons);
        win.set_child(Some(&body));

        let submit = {
            let (a, win, entry) = (self.clone(), win.clone(), entry.clone());
            move || {
                let pw = entry.text().to_string();
                win.close();
                a.import_archive(kind, path.clone(), Some(pw));
            }
        };
        let s = submit.clone();
        ok.connect_clicked(move |_| s());
        entry.connect_activate(move |_| submit());
        let w = win.clone();
        cancel.connect_clicked(move |_| w.close());
        win.present();
        entry.grab_focus();
    }
}
