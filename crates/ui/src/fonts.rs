//! Bundled static fonts plus cross-platform CJK fallback chains.
//!
//! Western UI and code faces (`Inter`, `JetBrains Mono`) are bundled directly
//! in the binary; Chinese glyphs in UI labels and JSON/XML payloads resolve
//! through explicit platform CJK system fallbacks so characters never render as
//! missing-glyph boxes (`□`).

use std::{borrow::Cow, sync::OnceLock};

use gpui::{App, FontFallbacks};

pub const SANS_FAMILY: &str = "Inter";
pub const MONO_FAMILY: &str = "JetBrains Mono";

const INTER_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
const INTER_MEDIUM: &[u8] = include_bytes!("../../../assets/fonts/Inter-Medium.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf");
const MONO_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf");
const MONO_MEDIUM: &[u8] = include_bytes!("../../../assets/fonts/JetBrainsMono-Medium.ttf");

struct FontSet {
    sans: String,
    mono: String,
    fallbacks: FontFallbacks,
}

static FONTS: OnceLock<FontSet> = OnceLock::new();

fn default_cjk_fallbacks() -> FontFallbacks {
    FontFallbacks::from_fonts(vec![
        "PingFang SC".to_string(),
        "Hiragino Sans GB".to_string(),
        "Heiti SC".to_string(),
        "Noto Sans CJK SC".to_string(),
        "Source Han Sans SC".to_string(),
        "WenQuanYi Micro Hei".to_string(),
        "Droid Sans Fallback".to_string(),
        "Microsoft YaHei UI".to_string(),
        "Microsoft YaHei".to_string(),
    ])
}

/// Register the bundled faces and remember the resolved family names.
pub fn install(cx: &mut App) {
    let result = cx.text_system().add_fonts(vec![
        Cow::Borrowed(INTER_REGULAR),
        Cow::Borrowed(INTER_MEDIUM),
        Cow::Borrowed(INTER_SEMIBOLD),
        Cow::Borrowed(MONO_REGULAR),
        Cow::Borrowed(MONO_MEDIUM),
    ]);

    let names = cx.text_system().all_font_names();
    let has_sans = names.iter().any(|name| name == SANS_FAMILY);
    let has_mono = names.iter().any(|name| name == MONO_FAMILY);

    if let Err(error) = &result {
        eprintln!("lenslet: could not load bundled fonts: {error}");
    }
    if !has_sans || !has_mono {
        eprintln!("lenslet: font fallback in use (Inter={has_sans}, JetBrains Mono={has_mono})");
    }

    let _ = FONTS.set(FontSet {
        sans: if has_sans {
            SANS_FAMILY.to_string()
        } else {
            "sans-serif".to_string()
        },
        mono: if has_mono {
            MONO_FAMILY.to_string()
        } else {
            "monospace".to_string()
        },
        fallbacks: default_cjk_fallbacks(),
    });
}

pub fn sans() -> &'static str {
    FONTS
        .get()
        .map(|fonts| fonts.sans.as_str())
        .unwrap_or(SANS_FAMILY)
}

pub fn mono() -> &'static str {
    FONTS
        .get()
        .map(|fonts| fonts.mono.as_str())
        .unwrap_or(MONO_FAMILY)
}

pub fn fallbacks() -> FontFallbacks {
    FONTS
        .get()
        .map(|fonts| fonts.fallbacks.clone())
        .unwrap_or_else(default_cjk_fallbacks)
}

pub fn sans_font() -> gpui::Font {
    gpui::Font {
        family: sans().into(),
        features: gpui::FontFeatures::default(),
        fallbacks: Some(fallbacks()),
        weight: gpui::FontWeight::NORMAL,
        style: gpui::FontStyle::Normal,
    }
}
