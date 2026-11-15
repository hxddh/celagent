//! Bundled static fonts. Loaded once through `cx.text_system().add_fonts(...)`.

use std::{borrow::Cow, sync::OnceLock};

use gpui::App;

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
}

static FONTS: OnceLock<FontSet> = OnceLock::new();

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
    });
}

pub fn sans() -> &'static str {
    FONTS
        .get()
        .map(|fonts| fonts.sans.as_str())
        .unwrap_or("sans-serif")
}

pub fn mono() -> &'static str {
    FONTS
        .get()
        .map(|fonts| fonts.mono.as_str())
        .unwrap_or("monospace")
}
