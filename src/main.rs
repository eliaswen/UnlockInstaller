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

fn main() {
    std::process::exit(ui::run());
}
