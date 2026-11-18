//! Design tokens for Lenslet: precision-instrument dark and light surfaces
//! with 6 contrast-calibrated accent palettes.

use std::sync::atomic::{AtomicU8, Ordering};

use gpui::{Hsla, Rgba, WindowAppearance, rgb};

use crate::prefs::{AccentColor, Density, ThemeMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    Dark,
    Light,
}

static ACTIVE_APPEARANCE: AtomicU8 = AtomicU8::new(0);
static ACTIVE_ACCENT: AtomicU8 = AtomicU8::new(0);

pub fn set_appearance(appearance: Appearance) {
    let val = match appearance {
        Appearance::Dark => 0,
        Appearance::Light => 1,
    };
    ACTIVE_APPEARANCE.store(val, Ordering::Relaxed);
}

pub fn appearance() -> Appearance {
    match ACTIVE_APPEARANCE.load(Ordering::Relaxed) {
        1 => Appearance::Light,
        _ => Appearance::Dark,
    }
}

pub fn set_accent(accent: AccentColor) {
    let val = match accent {
        AccentColor::Sky => 0,
        AccentColor::Indigo => 1,
        AccentColor::Violet => 2,
        AccentColor::Emerald => 3,
        AccentColor::Amber => 4,
        AccentColor::Rose => 5,
    };
    ACTIVE_ACCENT.store(val, Ordering::Relaxed);
}

pub fn accent() -> AccentColor {
    match ACTIVE_ACCENT.load(Ordering::Relaxed) {
        1 => AccentColor::Indigo,
        2 => AccentColor::Violet,
        3 => AccentColor::Emerald,
        4 => AccentColor::Amber,
        5 => AccentColor::Rose,
        _ => AccentColor::Sky,
    }
}

pub fn resolve(mode: ThemeMode, window: WindowAppearance) -> Appearance {
    match mode {
        ThemeMode::Dark => Appearance::Dark,
        ThemeMode::Light => Appearance::Light,
        ThemeMode::System => match window {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Appearance::Dark,
        },
    }
}

/// `(accent_hex, row_selected_hex, row_selected_hover_hex, mark_bg_hex)` for `(mode, accent)`.
pub const fn accent_tokens(mode: Appearance, accent: AccentColor) -> (u32, u32, u32, u32) {
    match (mode, accent) {
        (Appearance::Dark, AccentColor::Sky) => (0x7DD3FC, 0x152230, 0x192838, 0x131C27),
        (Appearance::Dark, AccentColor::Indigo) => (0xA5B4FC, 0x181C32, 0x1D223C, 0x15182A),
        (Appearance::Dark, AccentColor::Violet) => (0xC4B5FD, 0x1E1932, 0x241E3C, 0x1A162B),
        (Appearance::Dark, AccentColor::Emerald) => (0x6EE7B7, 0x122622, 0x162E29, 0x11201D),
        (Appearance::Dark, AccentColor::Amber) => (0xFCD34D, 0x262013, 0x2F2717, 0x201B11),
        (Appearance::Dark, AccentColor::Rose) => (0xFDA4AF, 0x281720, 0x311C27, 0x22141B),

        (Appearance::Light, AccentColor::Sky) => (0x0369A1, 0xE6F2FE, 0xDAEBFB, 0xE2EEF9),
        (Appearance::Light, AccentColor::Indigo) => (0x4338CA, 0xEAEAFE, 0xE0E0FC, 0xE6E7FB),
        (Appearance::Light, AccentColor::Violet) => (0x6D28D9, 0xF0E9FE, 0xE7DDFD, 0xECE5FB),
        (Appearance::Light, AccentColor::Emerald) => (0x047857, 0xE4F7F0, 0xD6F1E7, 0xE0F3EC),
        (Appearance::Light, AccentColor::Amber) => (0xB45309, 0xFEF1DF, 0xFDE8CC, 0xFAECDA),
        (Appearance::Light, AccentColor::Rose) => (0xBE123C, 0xFEE8EE, 0xFCDCE5, 0xFAE4EA),
    }
}

/// Swatch preview color for the Settings modal.
pub fn accent_swatch(accent: AccentColor) -> u32 {
    accent_tokens(appearance(), accent).0
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub canvas: u32,
    pub header: u32,
    pub toolbar: u32,
    pub strip: u32,
    pub table_head: u32,
    pub panel: u32,
    pub panel_raised: u32,
    pub code: u32,
    pub input: u32,
    pub input_focused: u32,

    pub row: u32,
    pub row_hover: u32,
    pub row_selected: u32,
    pub row_selected_hover: u32,
    pub mark_bg: u32,

    pub border: u32,
    pub border_subtle: u32,
    pub border_strong: u32,

    pub text: u32,
    pub text_soft: u32,
    pub text_body: u32,
    pub text_dim: u32,
    pub text_muted: u32,
    pub text_faint: u32,

    pub accent: u32,
    pub green: u32,
    pub amber: u32,
    pub red: u32,
    pub red_4xx: u32,
    pub purple: u32,
    pub orange: u32,
    pub blue: u32,
}

pub const DARK: Palette = Palette {
    canvas: 0x0A0C10,
    header: 0x0E1117,
    toolbar: 0x0C0F15,
    strip: 0x0B0E14,
    table_head: 0x0C0F14,
    panel: 0x0E1117,
    panel_raised: 0x11161E,
    code: 0x090C11,
    input: 0x0B0F15,
    input_focused: 0x0A0E14,

    row: 0x0E1117,
    row_hover: 0x131A24,
    row_selected: 0x152230,
    row_selected_hover: 0x192838,
    mark_bg: 0x131C27,

    border: 0x1E2632,
    border_subtle: 0x171E28,
    border_strong: 0x2A3442,

    text: 0xE7EDF6,
    text_soft: 0xC5D0DF,
    text_body: 0xB9C6D8,
    text_dim: 0x8C99AC,
    text_muted: 0x68768B,
    text_faint: 0x4C586B,

    accent: 0x7DD3FC,
    green: 0x62D6A7,
    amber: 0xF5B76B,
    red: 0xFF7188,
    red_4xx: 0xFF8A9B,
    purple: 0xB79CFF,
    orange: 0xF3A95F,
    blue: 0x67A9FF,
};

pub const LIGHT: Palette = Palette {
    canvas: 0xF5F7FA,
    header: 0xEEF2F6,
    toolbar: 0xF4F7FA,
    strip: 0xECF0F5,
    table_head: 0xF0F4F8,
    panel: 0xFFFFFF,
    panel_raised: 0xFAFBFD,
    code: 0xF3F6FA,
    input: 0xFFFFFF,
    input_focused: 0xFFFFFF,

    row: 0xFFFFFF,
    row_hover: 0xF1F5FA,
    row_selected: 0xE6F2FE,
    row_selected_hover: 0xDAEBFB,
    mark_bg: 0xE2EEF9,

    border: 0xD2DCE7,
    border_subtle: 0xE3EAF2,
    border_strong: 0xB4C3D4,

    text: 0x0F172A,
    text_soft: 0x1E293B,
    text_body: 0x334155,
    text_dim: 0x475569,
    text_muted: 0x5B6B82,
    text_faint: 0x75859B,

    accent: 0x0369A1,
    green: 0x047857,
    amber: 0xB45309,
    red: 0xDC2626,
    red_4xx: 0xBE123C,
    purple: 0x6D28D9,
    orange: 0xC2410C,
    blue: 0x1D4ED8,
};

/// Active palette for the current render pass, incorporating the active accent.
pub fn p() -> Palette {
    let mode = appearance();
    let mut base = match mode {
        Appearance::Dark => DARK,
        Appearance::Light => LIGHT,
    };
    let (acc, sel, sel_hover, mark) = accent_tokens(mode, accent());
    base.accent = acc;
    base.row_selected = sel;
    base.row_selected_hover = sel_hover;
    base.mark_bg = mark;
    base
}

// Workbench 3-zone layout rhythm.
pub const HEADER_H: f32 = 44.0;
pub const SUBBAR_H: f32 = 38.0;
pub const TABLE_HEAD_H: f32 = 28.0;
pub const SPLITTER_W: f32 = 5.0;

pub fn row_height(density: Density) -> f32 {
    match density {
        Density::Comfortable => 54.0,
        Density::Compact => 40.0,
    }
}

// Stream columns.
pub const ROW_PAD_X: f32 = 14.0;
pub const COL_TIME: f32 = 88.0;
pub const COL_SERVICE: f32 = 88.0;
pub const COL_STATUS: f32 = 52.0;
pub const COL_LATENCY: f32 = 66.0;
pub const COL_AUTH: f32 = 30.0;
pub const COL_GAP: f32 = 10.0;

pub fn h(hex: u32, alpha: f32) -> Hsla {
    let mut color: Hsla = rgb(hex).into();
    color.a = alpha;
    color
}

/// A hairline in an accent color (`hex` is `0xRRGGBB`, like every token here).
pub fn line(hex: u32, alpha: f32) -> Hsla {
    h(hex, alpha)
}

pub fn tint(color: Rgba, alpha: f32) -> Hsla {
    let mut color: Hsla = color.into();
    color.a = alpha;
    color
}

/// Color for a service label, calibrated per appearance so both dark and light
/// modes maintain crisp legibility.
pub fn service_color(label: &str) -> Rgba {
    service_color_for(label, appearance())
}

pub fn service_color_for(label: &str, mode: Appearance) -> Rgba {
    let label = label.trim().to_ascii_uppercase();
    match mode {
        Appearance::Dark => match label.as_str() {
            "S3" => rgb(DARK.orange),
            "SQS" => rgb(DARK.purple),
            "DYNAMODB" => rgb(DARK.blue),
            "SNS" => rgb(0xF58AC2),
            "LAMBDA" => rgb(0xFFB26E),
            "CLOUDWATCH" => rgb(0x7BD8C8),
            "STS" => rgb(0xE0B76B),
            "IAM" => rgb(0xD6A2FF),
            "" | "AWS" => rgb(0x8B94A6),
            _ => {
                const PALETTE: [u32; 8] = [
                    0x7BD8C8, 0xF0A3C8, 0xA9D18E, 0xE89B7C, 0x8FB8FF, 0xE0B76B, 0xC7A6F2, 0x84C7A8,
                ];
                rgb(PALETTE[fnv_index(&label, PALETTE.len())])
            }
        },
        Appearance::Light => match label.as_str() {
            "S3" => rgb(0xC2410C),
            "SQS" => rgb(0x6D28D9),
            "DYNAMODB" => rgb(0x1D4ED8),
            "SNS" => rgb(0xBE185D),
            "LAMBDA" => rgb(0xB45309),
            "CLOUDWATCH" => rgb(0x0F766E),
            "STS" => rgb(0x92400E),
            "IAM" => rgb(0x7E22CE),
            "" | "AWS" => rgb(0x475569),
            _ => {
                const PALETTE: [u32; 8] = [
                    0x0F766E, 0xBE185D, 0x3F6212, 0x9A3412, 0x1D4ED8, 0x92400E, 0x6D28D9, 0x047857,
                ];
                rgb(PALETTE[fnv_index(&label, PALETTE.len())])
            }
        },
    }
}

fn fnv_index(label: &str, len: usize) -> usize {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in label.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash as usize % len
}

pub fn status_color(status: u16) -> Rgba {
    let t = p();
    if status == 0 {
        rgb(t.text_muted)
    } else if status >= 500 {
        rgb(t.red)
    } else if status >= 400 {
        rgb(t.red_4xx)
    } else if status >= 300 {
        rgb(t.amber)
    } else {
        rgb(t.green)
    }
}

pub fn latency_color(ms: u64) -> Rgba {
    let t = p();
    if ms >= 2_000 {
        rgb(t.red)
    } else if ms >= 500 {
        rgb(t.amber)
    } else {
        rgb(t.text_muted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(hex: u32) -> f64 {
        let chan = |shift: u32| -> f64 {
            let srgb = ((hex >> shift) & 0xff) as f64 / 255.0;
            if srgb <= 0.03928 {
                srgb / 12.92
            } else {
                ((srgb + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * chan(16) + 0.7152 * chan(8) + 0.0722 * chan(0)
    }

    fn contrast(a: u32, b: u32) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn unknown_services_get_a_stable_color() {
        for mode in [Appearance::Dark, Appearance::Light] {
            let kinesis = service_color_for("kinesis", mode);
            assert_eq!(kinesis, service_color_for("KINESIS", mode));
            assert_ne!(kinesis, service_color_for("", mode));
        }
    }

    #[test]
    fn light_and_dark_accent_palettes_maintain_wcag_aa_contrast() {
        for token in [
            LIGHT.text,
            LIGHT.text_soft,
            LIGHT.text_body,
            LIGHT.text_dim,
            LIGHT.text_muted,
            LIGHT.green,
            LIGHT.amber,
            LIGHT.red,
            LIGHT.red_4xx,
            LIGHT.blue,
        ] {
            let ratio = contrast(token, LIGHT.panel);
            assert!(
                ratio >= 4.5,
                "token #{token:06x} has insufficient contrast {ratio:.2} on white"
            );
        }

        for accent in AccentColor::ALL {
            let (dark_acc, _, _, _) = accent_tokens(Appearance::Dark, accent);
            let dark_ratio = contrast(dark_acc, DARK.panel);
            assert!(
                dark_ratio >= 4.5,
                "{accent:?} dark accent #{dark_acc:06x} contrast {dark_ratio:.2} < 4.5"
            );
            let (light_acc, _, _, _) = accent_tokens(Appearance::Light, accent);
            let light_ratio = contrast(light_acc, LIGHT.panel);
            assert!(
                light_ratio >= 4.5,
                "{accent:?} light accent #{light_acc:06x} contrast {light_ratio:.2} < 4.5"
            );
        }
    }
}
