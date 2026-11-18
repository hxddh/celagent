//! Selective desktop UI localization (`En` / `Zh`).
//!
//! Only desktop UI chrome, settings descriptions, buttons and empty-state
//! guidance are localized in Chinese mode. Professional AWS, IAM and HTTP
//! parameters and protocol terms (`Service`, `Operation`, `Method`, `Path`,
//! `Status`, `Host`, `Region`, `Request ID`, `Trace ID`, `Content-Type`,
//! `Headers`, `Body`, `Action`, `Resource`, `SigV4`, `Presigned URL`, `JSON`,
//! `XML`, `Hex`, `SHA-256`, `curl`, `lenslet.lock`, CLI flags) intentionally
//! stay in canonical English.

use std::sync::atomic::{AtomicU8, Ordering};

use crate::prefs::{AccentColor, Density, Lang, ThemeMode, TimeFormat};

static ACTIVE_LANG: AtomicU8 = AtomicU8::new(0);

pub fn set_lang(lang: Lang) {
    let val = match lang {
        Lang::En => 0,
        Lang::Zh => 1,
    };
    ACTIVE_LANG.store(val, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    match ACTIVE_LANG.load(Ordering::Relaxed) {
        1 => Lang::Zh,
        _ => Lang::En,
    }
}

pub fn is_zh() -> bool {
    lang() == Lang::Zh
}

pub fn pick<'a>(en: &'a str, zh: &'a str) -> &'a str {
    if is_zh() { zh } else { en }
}

pub fn theme_label(mode: ThemeMode) -> &'static str {
    match (mode, lang()) {
        (ThemeMode::System, Lang::En) => "System",
        (ThemeMode::Light, Lang::En) => "Light",
        (ThemeMode::Dark, Lang::En) => "Dark",
        (ThemeMode::System, Lang::Zh) => "跟随系统",
        (ThemeMode::Light, Lang::Zh) => "浅色",
        (ThemeMode::Dark, Lang::Zh) => "深色",
    }
}

pub fn accent_label(accent: AccentColor) -> &'static str {
    match (accent, lang()) {
        (AccentColor::Sky, Lang::En) => "Sky",
        (AccentColor::Indigo, Lang::En) => "Indigo",
        (AccentColor::Violet, Lang::En) => "Violet",
        (AccentColor::Emerald, Lang::En) => "Emerald",
        (AccentColor::Amber, Lang::En) => "Amber",
        (AccentColor::Rose, Lang::En) => "Rose",
        (AccentColor::Sky, Lang::Zh) => "经典青蓝",
        (AccentColor::Indigo, Lang::Zh) => "靛蓝",
        (AccentColor::Violet, Lang::Zh) => "紫罗兰",
        (AccentColor::Emerald, Lang::Zh) => "翠绿",
        (AccentColor::Amber, Lang::Zh) => "琥珀橙",
        (AccentColor::Rose, Lang::Zh) => "珊瑚红",
    }
}

pub fn density_label(density: Density) -> &'static str {
    match (density, lang()) {
        (Density::Comfortable, Lang::En) => "Comfortable",
        (Density::Compact, Lang::En) => "Compact",
        (Density::Comfortable, Lang::Zh) => "舒适",
        (Density::Compact, Lang::Zh) => "紧凑",
    }
}

pub fn time_format_label(fmt: TimeFormat) -> &'static str {
    match (fmt, lang()) {
        (TimeFormat::Relative, Lang::En) => "Relative (+ms)",
        (TimeFormat::Clock, Lang::En) => "Wall Clock",
        (TimeFormat::Relative, Lang::Zh) => "相对起点 (+ms)",
        (TimeFormat::Clock, Lang::Zh) => "本地时钟",
    }
}

pub fn copied_toast(item: &str) -> String {
    if is_zh() {
        format!("已复制 {item}")
    } else {
        format!("Copied {item}")
    }
}
