//! Selective desktop UI localization (`En` / `Zh`).
//!
//! Only desktop UI chrome, status guidance, buttons and empty-state explanations
//! are localized in Chinese mode. Professional AWS, IAM and HTTP parameters and
//! protocol terms (`Service`, `Operation`, `Method`, `Path`, `Status`, `Host`,
//! `Region`, `Request ID`, `Trace ID`, `Content-Type`, `Headers`, `Body`,
//! `Action`, `Resource`, `SigV4`, `Presigned URL`, `JSON`, `XML`, `Hex`,
//! `SHA-256`, `curl`, `lenslet.lock`, CLI flags) intentionally stay in English.

use std::sync::atomic::{AtomicU8, Ordering};

use crate::prefs::{Lang, ThemeMode};

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

pub fn theme_button_label(mode: ThemeMode) -> &'static str {
    match (mode, lang()) {
        (ThemeMode::System, Lang::En) => "Auto",
        (ThemeMode::Light, Lang::En) => "Light",
        (ThemeMode::Dark, Lang::En) => "Dark",
        (ThemeMode::System, Lang::Zh) => "自动",
        (ThemeMode::Light, Lang::Zh) => "浅色",
        (ThemeMode::Dark, Lang::Zh) => "深色",
    }
}

pub fn theme_toast(mode: ThemeMode) -> String {
    match (mode, lang()) {
        (ThemeMode::System, Lang::En) => "theme: system".to_string(),
        (ThemeMode::Light, Lang::En) => "theme: light".to_string(),
        (ThemeMode::Dark, Lang::En) => "theme: dark".to_string(),
        (ThemeMode::System, Lang::Zh) => "主题：跟随系统".to_string(),
        (ThemeMode::Light, Lang::Zh) => "主题：浅色".to_string(),
        (ThemeMode::Dark, Lang::Zh) => "主题：深色".to_string(),
    }
}

pub fn lang_button_label() -> &'static str {
    match lang() {
        Lang::En => "中文",
        Lang::Zh => "EN",
    }
}

pub fn lang_toast() -> String {
    match lang() {
        Lang::En => "language: English".to_string(),
        Lang::Zh => "界面语言：中文".to_string(),
    }
}

pub fn copied_toast(item: &str) -> String {
    if is_zh() {
        format!("已复制 {item}")
    } else {
        format!("copied {item}")
    }
}

pub fn call_count(count: usize) -> String {
    if is_zh() {
        format!("{count} 次调用")
    } else if count == 1 {
        "1 call".to_string()
    } else {
        format!("{count} calls")
    }
}

pub fn error_count(count: usize) -> String {
    if is_zh() {
        format!("{count} 个错误")
    } else if count == 1 {
        "1 error".to_string()
    } else {
        format!("{count} errors")
    }
}

pub fn service_count(count: usize) -> String {
    if is_zh() {
        format!("{count} 个服务")
    } else if count == 1 {
        "1 service".to_string()
    } else {
        format!("{count} services")
    }
}
