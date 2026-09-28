//! Small, non-sensitive application preferences.
//!
//! The saved SoundCloud session has its own protected store. These settings
//! contain only UI choices and are kept in a separate JSON file so future
//! TrimUI applications can reuse the same persistence pattern.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const STORE_VERSION: u8 = 1;
const STORE_FILE: &str = "preferences.json";
const TEMP_FILE: &str = "preferences.json.tmp";
const MAX_STORE_BYTES: u64 = 4 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Preferences {
    #[serde(default = "default_show_battery_percentage")]
    pub show_battery_percentage: bool,
    #[serde(default)]
    pub always_keep_screen_on: bool,
    #[serde(default)]
    pub minimal_interface: bool,
}

const fn default_show_battery_percentage() -> bool {
    true
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            show_battery_percentage: true,
            always_keep_screen_on: false,
            minimal_interface: false,
        }
    }
}

#[derive(Debug)]
pub enum PreferencesError {
    Io,
    InvalidData,
    PlatformUnavailable,
}

#[derive(Deserialize, Serialize)]
struct StoredPreferences {
    version: u8,
    preferences: Preferences,
}

fn data_dir() -> Result<PathBuf, PreferencesError> {
    if let Some(configured) = std::env::var_os("BRICKWAVE_DATA_DIR")
        && !configured.is_empty()
    {
        return Ok(PathBuf::from(configured));
    }
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("LOCALAPPDATA")
        && !root.is_empty()
    {
        return Ok(PathBuf::from(root).join("Brickwave"));
    }
    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME")
            && !xdg.is_empty()
        {
            return Ok(PathBuf::from(xdg).join("brickwave"));
        }
        if let Some(home) = std::env::var_os("HOME")
            && !home.is_empty()
        {
            return Ok(PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("brickwave"));
        }
    }
    Err(PreferencesError::PlatformUnavailable)
}

fn load_at(path: &Path) -> Result<Preferences, PreferencesError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Preferences::default());
        }
        Err(_) => return Err(PreferencesError::Io),
    };
    if !metadata.is_file() || metadata.len() > MAX_STORE_BYTES {
        return Err(PreferencesError::InvalidData);
    }
    let bytes = fs::read(path).map_err(|_| PreferencesError::Io)?;
    let stored: StoredPreferences =
        serde_json::from_slice(&bytes).map_err(|_| PreferencesError::InvalidData)?;
    if stored.version != STORE_VERSION {
        return Err(PreferencesError::InvalidData);
    }
    Ok(stored.preferences)
}

fn save_at(path: &Path, preferences: Preferences) -> Result<(), PreferencesError> {
    let parent = path.parent().ok_or(PreferencesError::Io)?;
    fs::create_dir_all(parent).map_err(|_| PreferencesError::Io)?;
    let bytes = serde_json::to_vec_pretty(&StoredPreferences {
        version: STORE_VERSION,
        preferences,
    })
    .map_err(|_| PreferencesError::InvalidData)?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        return Err(PreferencesError::InvalidData);
    }

    let temporary = parent.join(TEMP_FILE);
    let _ = fs::remove_file(&temporary);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| PreferencesError::Io)?;
    file.write_all(&bytes).map_err(|_| PreferencesError::Io)?;
    file.sync_all().map_err(|_| PreferencesError::Io)?;
    drop(file);

    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path).map_err(|_| PreferencesError::Io)?;
    }
    fs::rename(&temporary, path).map_err(|_| PreferencesError::Io)
}

pub fn load() -> Result<Preferences, PreferencesError> {
    load_at(&data_dir()?.join(STORE_FILE))
}

pub fn save(preferences: Preferences) -> Result<(), PreferencesError> {
    save_at(&data_dir()?.join(STORE_FILE), preferences)
}

#[cfg(test)]
mod tests {
    use super::{Preferences, load_at, save_at};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_path(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "brickwave-preferences-{name}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory.join("preferences.json")
    }

    #[test]
    fn missing_store_uses_safe_default() {
        let path = test_path("missing");
        fs::remove_file(&path).ok();
        assert_eq!(load_at(&path).unwrap(), Preferences::default());
    }

    #[test]
    fn battery_choice_round_trips() {
        let path = test_path("round-trip");
        let preferences = Preferences {
            show_battery_percentage: false,
            always_keep_screen_on: true,
            minimal_interface: true,
        };
        save_at(&path, preferences).unwrap();
        assert_eq!(load_at(&path).unwrap(), preferences);
        let text = fs::read_to_string(path).unwrap();
        assert!(text.contains("show_battery_percentage"));
        assert!(text.contains("always_keep_screen_on"));
        assert!(text.contains("minimal_interface"));
    }

    #[test]
    fn old_store_defaults_battery_display_to_enabled() {
        let path = test_path("old-store");
        fs::write(
            &path,
            r#"{"version":1,"preferences":{"retired_option":false}}"#,
        )
        .unwrap();
        assert!(load_at(&path).unwrap().show_battery_percentage);
        assert!(!load_at(&path).unwrap().always_keep_screen_on);
        assert!(!load_at(&path).unwrap().minimal_interface);
    }
}
