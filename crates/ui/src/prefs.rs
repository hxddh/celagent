//! Persistent window preferences in `~/.lenslet/ui.json`.
//!
//! Kept strictly in the user's home directory so appearance and language
//! choices never touch project repositories or require configuration files.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeMode {
    pub fn cycle(self) -> Self {
        match self {
            ThemeMode::System => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
            ThemeMode::Dark => ThemeMode::System,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    En,
    Zh,
}

impl Default for Lang {
    fn default() -> Self {
        detect_system_lang()
    }
}

impl Lang {
    pub fn toggle(self) -> Self {
        match self {
            Lang::En => Lang::Zh,
            Lang::Zh => Lang::En,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiPrefs {
    #[serde(default)]
    pub theme: ThemeMode,
    #[serde(default)]
    pub lang: Lang,
}

pub fn path(home: &Path) -> PathBuf {
    home.join(".lenslet/ui.json")
}

pub fn load(home: &Path) -> UiPrefs {
    let mut prefs = fs::read_to_string(path(home))
        .ok()
        .and_then(|text| serde_json::from_str::<UiPrefs>(&text).ok())
        .unwrap_or_default();
    if let Some(forced) = env_lang() {
        prefs.lang = forced;
    }
    if let Some(forced) = env_theme() {
        prefs.theme = forced;
    }
    prefs
}

pub fn save(home: &Path, prefs: &UiPrefs) {
    let target = path(home);
    if let Some(parent) = target.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(prefs) {
        let _ = fs::write(&target, format!("{text}\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o600));
        }
    }
}

fn detect_system_lang() -> Lang {
    if let Some(forced) = env_lang() {
        return forced;
    }
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(val) = std::env::var_os(var) {
            let lower = val.to_string_lossy().to_ascii_lowercase();
            if !lower.is_empty() && lower != "c" && !lower.starts_with("c.") {
                return if lower.starts_with("zh") {
                    Lang::Zh
                } else {
                    Lang::En
                };
            }
        }
    }
    Lang::En
}

fn env_lang() -> Option<Lang> {
    let raw = std::env::var_os("LENSLET_LANG")?;
    let lower = raw.to_string_lossy().trim().to_ascii_lowercase();
    if lower.starts_with("zh") {
        Some(Lang::Zh)
    } else if lower.starts_with("en") {
        Some(Lang::En)
    } else {
        None
    }
}

fn env_theme() -> Option<ThemeMode> {
    let raw = std::env::var_os("LENSLET_THEME")?;
    match raw.to_string_lossy().trim().to_ascii_lowercase().as_str() {
        "light" => Some(ThemeMode::Light),
        "dark" => Some(ThemeMode::Dark),
        "system" | "auto" => Some(ThemeMode::System),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_theme_and_round_trips_preferences() {
        assert_eq!(ThemeMode::System.cycle(), ThemeMode::Light);
        assert_eq!(ThemeMode::Light.cycle(), ThemeMode::Dark);
        assert_eq!(ThemeMode::Dark.cycle(), ThemeMode::System);
        assert_eq!(Lang::En.toggle(), Lang::Zh);

        let dir = std::env::temp_dir().join(format!("lenslet-ui-prefs-{}", std::process::id()));
        let prefs = UiPrefs {
            theme: ThemeMode::Light,
            lang: Lang::Zh,
        };
        save(&dir, &prefs);
        let text = fs::read_to_string(path(&dir)).unwrap();
        let parsed: UiPrefs = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, prefs);
        let _ = fs::remove_dir_all(&dir);
    }
}
