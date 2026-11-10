//! Per-user directories used by the application.
//!
//! | | Linux default | Windows default |
//! |---|---|---|
//! | config | `~/.config/UnlockInstaller` | `%APPDATA%\UnlockInstaller` |
//! | data (extracted unlockers) | `~/.local/share/UnlockInstaller` | `%APPDATA%\UnlockInstaller` |
//! | cache | `~/.cache/UnlockInstaller` | `%LOCALAPPDATA%\UnlockInstaller` |
//!
//! The data and cache folders can be changed in the app's settings (see
//! [`set_overrides`]). On Linux the config folder can be changed with the
//! [`CONFIG_DIR_ENV`] environment variable.

use std::fs;
use std::path::PathBuf;
use std::sync::RwLock;

pub const APP_NAME: &str = "UnlockInstaller";
/// Directory name used by earlier versions; moved over on first use.
const LEGACY_NAME: &str = "CreamInstaller";
/// Environment variable naming the config folder (not used on Windows,
/// where the settings live in the registry).
pub const CONFIG_DIR_ENV: &str = "UNLOCKINSTALLER_CONFIG_DIR";

/// (data, cache) folders chosen in the settings, replacing the defaults.
static OVERRIDES: RwLock<(Option<PathBuf>, Option<PathBuf>)> = RwLock::new((None, None));

/// Use custom data / cache folders; `None` means the default one.
pub fn set_overrides(data: Option<PathBuf>, cache: Option<PathBuf>) {
    *OVERRIDES.write().unwrap_or_else(|e| e.into_inner()) = (data, cache);
}

fn overrides() -> (Option<PathBuf>, Option<PathBuf>) {
    OVERRIDES.read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn app_dir(base: Option<PathBuf>, fallback: impl FnOnce() -> PathBuf) -> PathBuf {
    let base = base.unwrap_or_else(fallback);
    let dir = base.join(APP_NAME);
    if !dir.exists() {
        let legacy = base.join(LEGACY_NAME);
        if legacy.is_dir() {
            let _ = fs::rename(legacy, &dir);
        }
    }
    dir
}

/// The config folder named by [`CONFIG_DIR_ENV`], if any.
fn env_config_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    std::env::var_os(CONFIG_DIR_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Whether the config folder comes from [`CONFIG_DIR_ENV`].
pub fn config_dir_from_env() -> bool {
    env_config_dir().is_some()
}

/// Settings (on Linux) and saved DLC selections.
pub fn config_dir() -> PathBuf {
    env_config_dir().unwrap_or_else(|| app_dir(dirs::config_dir(), || PathBuf::from(".")))
}

pub fn default_data_dir() -> PathBuf {
    app_dir(dirs::data_dir(), || PathBuf::from("."))
}

pub fn default_cache_dir() -> PathBuf {
    app_dir(dirs::cache_dir(), std::env::temp_dir)
}

/// Where imported unlocker archives are extracted to.
pub fn data_dir() -> PathBuf {
    overrides().0.unwrap_or_else(default_data_dir)
}

/// DLC list cache and downloaded release archives.
pub fn cache_dir() -> PathBuf {
    overrides().1.unwrap_or_else(default_cache_dir)
}
