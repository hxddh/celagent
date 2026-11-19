//! Selective UI localization (`En` / `Zh`).
//!
//! Only window chrome, settings, buttons and empty-state guidance are
//! translated. AWS, IAM and HTTP terms (`Service`, `Operation`, `Status`,
//! `Headers`, `Action`, `Resource`, `curl`, `lenslet.lock`, flags) stay in
//! canonical English. The active language is part of the [`crate::theme::Ui`]
//! global; strings are picked with `ui.t(en, zh)`.

use crate::prefs::{AccentColor, Density, Lang, ThemeMode};

pub fn theme_label(mode: ThemeMode, lang: Lang) -> &'static str {
    match (mode, lang) {
        (ThemeMode::System, Lang::En) => "System",
        (ThemeMode::Light, Lang::En) => "Light",
        (ThemeMode::Dark, Lang::En) => "Dark",
        (ThemeMode::System, Lang::Zh) => "跟随系统",
        (ThemeMode::Light, Lang::Zh) => "浅色",
        (ThemeMode::Dark, Lang::Zh) => "深色",
    }
}

pub fn accent_label(accent: AccentColor, lang: Lang) -> &'static str {
    match (accent, lang) {
        (AccentColor::Sky, Lang::En) => "Sky",
        (AccentColor::Indigo, Lang::En) => "Indigo",
        (AccentColor::Violet, Lang::En) => "Violet",
        (AccentColor::Emerald, Lang::En) => "Emerald",
        (AccentColor::Amber, Lang::En) => "Amber",
        (AccentColor::Rose, Lang::En) => "Rose",
        (AccentColor::Sky, Lang::Zh) => "天蓝",
        (AccentColor::Indigo, Lang::Zh) => "靛蓝",
        (AccentColor::Violet, Lang::Zh) => "紫罗兰",
        (AccentColor::Emerald, Lang::Zh) => "翠绿",
        (AccentColor::Amber, Lang::Zh) => "琥珀",
        (AccentColor::Rose, Lang::Zh) => "玫红",
    }
}

pub fn density_label(density: Density, lang: Lang) -> &'static str {
    match (density, lang) {
        (Density::Comfortable, Lang::En) => "Comfortable",
        (Density::Compact, Lang::En) => "Compact",
        (Density::Comfortable, Lang::Zh) => "舒适",
        (Density::Compact, Lang::Zh) => "紧凑",
    }
}

pub fn copied(item: &str, lang: Lang) -> String {
    match lang {
        Lang::Zh => format!("已复制 {item}"),
        Lang::En => format!("Copied {item}"),
    }
}

/// `⌘K` on macOS, `Ctrl+K` elsewhere.
pub fn shortcut(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("⌘{key}")
    } else {
        format!("Ctrl+{key}")
    }
}
