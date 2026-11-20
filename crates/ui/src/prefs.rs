//! Persistent window preferences in `~/.lenslet/ui.json`.
//!
//! Kept strictly in the user's home directory so appearance, accent color,
//! layout, formatting, and language choices never touch project repositories.

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
    pub const ALL: [ThemeMode; 3] = [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccentColor {
    #[default]
    Sky,
    Indigo,
    Violet,
    Emerald,
    Amber,
    Rose,
}

impl AccentColor {
    /// The accents Settings offers (older files may name Violet or Rose).
    pub const ALL: [AccentColor; 4] = [
        AccentColor::Sky,
        AccentColor::Indigo,
        AccentColor::Emerald,
        AccentColor::Amber,
    ];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    #[default]
    Comfortable,
    Compact,
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

fn default_true() -> bool {
    true
}

fn default_inspector_width() -> f32 {
    crate::theme::INSPECTOR_DEFAULT_W
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiPrefs {
    #[serde(default)]
    pub theme: ThemeMode,
    #[serde(default)]
    pub accent: AccentColor,
    #[serde(default)]
    pub density: Density,
    #[serde(default)]
    pub lang: Lang,
    /// Skip popover/palette animations.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reduce_motion: bool,
    #[serde(default = "default_inspector_width")]
    pub inspector_width: f32,
    #[serde(default = "default_true")]
    pub inspector_open: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sidebar_open: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_height: Option<f32>,
}

impl Default for UiPrefs {
    fn default() -> Self {
        UiPrefs {
            theme: ThemeMode::default(),
            accent: AccentColor::default(),
            density: Density::default(),
            lang: Lang::default(),
            reduce_motion: false,
            inspector_width: default_inspector_width(),
            inspector_open: true,
            sidebar_open: false,
            window_width: None,
            window_height: None,
        }
    }
}

impl UiPrefs {
    pub fn clamped_inspector_width(&self) -> f32 {
        self.inspector_width.clamp(320.0, 780.0)
    }
}

pub fn path(home: &Path) -> PathBuf {
    home.join(".lenslet/ui.json")
}

pub fn load(home: &Path) -> UiPrefs {
    crate::fsguard::assert_io_allowed();
    let mut prefs = fs::read_to_string(path(home))
        .ok()
        .and_then(|text| serde_json::from_str::<UiPrefs>(&text).ok())
        .unwrap_or_default();
    prefs.inspector_width = prefs.clamped_inspector_width();
    if let Some(forced) = env_lang() {
        prefs.lang = forced;
    }
    if let Some(forced) = env_theme() {
        prefs.theme = forced;
    }
    prefs
}

/// Write the preferences. Blocking IO: the window calls it from the
/// background executor, debounced.
pub fn save(home: &Path, prefs: &UiPrefs) {
    crate::fsguard::assert_io_allowed();
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
    fn round_trips_workbench_preferences_and_clamps_inspector_width() {
        let dir = std::env::temp_dir().join(format!("lenslet-ui-prefs-{}", std::process::id()));
        let prefs = UiPrefs {
            theme: ThemeMode::Light,
            accent: AccentColor::Indigo,
            density: Density::Compact,
            lang: Lang::Zh,
            reduce_motion: true,
            inspector_width: 520.0,
            inspector_open: false,
            sidebar_open: true,
            window_width: Some(1440.0),
            window_height: Some(900.0),
        };
        save(&dir, &prefs);
        let text = fs::read_to_string(path(&dir)).unwrap();
        let parsed: UiPrefs = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, prefs);

        let mut out_of_range = parsed;
        out_of_range.inspector_width = 120.0;
        assert_eq!(out_of_range.clamped_inspector_width(), 320.0);
        let _ = fs::remove_dir_all(&dir);
    }
}
