//! User preferences.
//!
//! On Windows they are stored in the registry under
//! `HKEY_CURRENT_USER\Software\UnlockInstaller`; elsewhere as `settings.json`
//! in the config folder (see [`crate::paths`]).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths;
use crate::unlocker::{Kind, Method};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Unlocker preselected for games that have none installed.
    pub unlocker: Kind,
    /// Install method preselected for games that have none installed
    /// (falls back to direct where Koaloader is not possible).
    pub method: Method,
    /// Folder imported unlockers are extracted to; `None` for the default.
    pub data_dir: Option<PathBuf>,
    /// Folder for the DLC cache and downloads; `None` for the default.
    pub cache_dir: Option<PathBuf>,
    /// Look for a newer release on GitHub when the app starts.
    pub check_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            unlocker: Kind::SmokeApi,
            method: Method::Direct,
            data_dir: None,
            cache_dir: None,
            check_updates: true,
        }
    }
}

impl Settings {
    /// Load the saved settings and start using their folders.
    pub fn load() -> Settings {
        let s = backend::load();
        s.apply();
        s
    }

    /// Save the settings and start using their folders.
    pub fn save(&self) -> Result<(), String> {
        self.apply();
        backend::save(self)
    }

    fn apply(&self) {
        paths::set_overrides(self.data_dir.clone(), self.cache_dir.clone());
    }
}

/// Human-readable description of where the settings are stored.
pub fn location() -> String {
    backend::location()
}

#[cfg(not(windows))]
mod backend {
    use std::fs;
    use std::path::PathBuf;

    use super::Settings;
    use crate::paths;

    fn path() -> PathBuf {
        paths::config_dir().join("settings.json")
    }

    pub fn location() -> String {
        path().display().to_string()
    }

    pub fn load() -> Settings {
        fs::read(path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(s: &Settings) -> Result<(), String> {
        let p = path();
        let err = |e: std::io::Error| format!("{}: {e}", p.display());
        if let Some(dir) = p.parent() {
            fs::create_dir_all(dir).map_err(err)?;
        }
        let json = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
        fs::write(&p, json).map_err(err)
    }
}

#[cfg(windows)]
mod backend {
    use std::path::PathBuf;

    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    use super::Settings;
    use crate::unlocker::{Kind, Method};

    #[cfg(not(test))]
    pub const KEY: &str = r"Software\UnlockInstaller";
    /// Tests must not touch the real settings.
    #[cfg(test)]
    pub const KEY: &str = r"Software\UnlockInstaller-test";

    const UNLOCKER: &str = "Unlocker";
    const METHOD: &str = "Method";
    const DATA_DIR: &str = "ExtractPath";
    const CACHE_DIR: &str = "CachePath";
    const CHECK_UPDATES: &str = "CheckUpdates";

    pub fn location() -> String {
        format!(r"the registry (HKEY_CURRENT_USER\{KEY})")
    }

    fn method_name(m: Method) -> &'static str {
        match m {
            Method::Direct => "Direct",
            Method::Koaloader => "Koaloader",
        }
    }

    pub fn load() -> Settings {
        let mut s = Settings::default();
        let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(KEY) else {
            return s;
        };
        let get = |name: &str| key.get_value::<String, _>(name).ok().filter(|v| !v.is_empty());
        if let Some(v) = get(UNLOCKER) {
            if let Some(k) = [Kind::SmokeApi, Kind::CreamApi].into_iter().find(|k| k.name() == v) {
                s.unlocker = k;
            }
        }
        if let Some(v) = get(METHOD) {
            if let Some(m) = [Method::Direct, Method::Koaloader].into_iter().find(|m| method_name(*m) == v) {
                s.method = m;
            }
        }
        s.data_dir = get(DATA_DIR).map(PathBuf::from);
        s.cache_dir = get(CACHE_DIR).map(PathBuf::from);
        if let Ok(v) = key.get_value::<u32, _>(CHECK_UPDATES) {
            s.check_updates = v != 0;
        }
        s
    }

    pub fn save(s: &Settings) -> Result<(), String> {
        let err = |e: std::io::Error| format!("Cannot write the registry: {e}");
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY).map_err(err)?;
        key.set_value(UNLOCKER, &s.unlocker.name()).map_err(err)?;
        key.set_value(METHOD, &method_name(s.method)).map_err(err)?;
        key.set_value(CHECK_UPDATES, &(s.check_updates as u32)).map_err(err)?;
        for (name, dir) in [(DATA_DIR, &s.data_dir), (CACHE_DIR, &s.cache_dir)] {
            match dir {
                Some(d) => key.set_value(name, &d.as_os_str()).map_err(err)?,
                // an absent value means "use the default folder"
                None => match key.delete_value(name) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(err(e)),
                    _ => {}
                },
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, because the settings location and folder overrides are
    /// process-wide.
    #[test]
    fn save_load_roundtrip_and_folder_overrides() {
        #[cfg(not(windows))]
        let dir = {
            let dir = std::env::temp_dir().join(format!("ui-settings-{}", std::process::id()));
            // SAFETY: no other test reads the environment or the config folder.
            unsafe { std::env::set_var(paths::CONFIG_DIR_ENV, &dir) };
            assert_eq!(paths::config_dir(), dir);
            dir
        };

        assert_eq!(backend::load(), Settings::default());
        let custom = Settings {
            unlocker: Kind::CreamApi,
            method: Method::Koaloader,
            data_dir: Some(std::env::temp_dir().join("ui-extract")),
            cache_dir: None,
            check_updates: false,
        };
        custom.save().unwrap();
        assert_eq!(backend::load(), custom);
        assert_eq!(paths::data_dir(), custom.data_dir.clone().unwrap());
        assert_eq!(paths::cache_dir(), paths::default_cache_dir());

        // Going back to the defaults must clear the stored folders.
        Settings::default().save().unwrap();
        assert_eq!(backend::load(), Settings::default());
        assert_eq!(paths::data_dir(), paths::default_data_dir());

        #[cfg(not(windows))]
        {
            assert!(dir.join("settings.json").is_file());
            std::fs::remove_dir_all(&dir).unwrap();
            unsafe { std::env::remove_var(paths::CONFIG_DIR_ENV) };
        }
        #[cfg(windows)]
        winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
            .delete_subkey(backend::KEY)
            .unwrap();
    }
}
