// No console window for release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod archive;
mod binfmt;
mod dlc;
mod github;
mod paths;
mod settings;
mod steam;
mod ui;
mod unlocker;
mod vdf;
mod wine;

fn main() -> gtk::glib::ExitCode {
    if wine::detected() {
        // Wine lacks parts of the graphics stack GTK uses on Windows
        // (DirectComposition, and GL with some setups), and GTK crashes on
        // its first window. Fall back to plain software drawing so that the
        // "run the native version" warning can be shown at all.
        for (name, value) in [("GDK_DISABLE", "dcomp"), ("GSK_RENDERER", "cairo")] {
            if std::env::var_os(name).is_none() {
                // SAFETY: nothing else is running yet, so no thread reads the environment.
                unsafe { std::env::set_var(name, value) };
            }
        }
    }
    ui::run()
}
