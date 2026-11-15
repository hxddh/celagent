//! Design tokens for Lenslet: a dense, dark, precision-instrument surface.

use gpui::{Hsla, Rgba, rgb, rgba};

// Canvas and layered surfaces.
pub const CANVAS: u32 = 0x0A0C10;
pub const HEADER: u32 = 0x0E1117;
pub const TOOLBAR: u32 = 0x0C0F15;
pub const PANEL: u32 = 0x0E1117;
pub const PANEL_RAISED: u32 = 0x11161E;
pub const CODE: u32 = 0x090C11;
pub const FOOTER: u32 = 0x080A0E;

pub const ROW: u32 = 0x0E1117;
pub const ROW_HOVER: u32 = 0x131A24;
pub const ROW_SELECTED: u32 = 0x16212C;
pub const ROW_SELECTED_HOVER: u32 = 0x18242F;

// Hairlines.
pub const BORDER: u32 = 0x1E2632;
pub const BORDER_SUBTLE: u32 = 0x171E28;
pub const BORDER_STRONG: u32 = 0x2A3442;

// Type.
pub const TEXT: u32 = 0xE7EDF6;
pub const TEXT_SOFT: u32 = 0xC5D0DF;
pub const TEXT_BODY: u32 = 0xB9C6D8;
pub const TEXT_DIM: u32 = 0x8C99AC;
pub const TEXT_MUTED: u32 = 0x68768B;
pub const TEXT_FAINT: u32 = 0x4C586B;

// Accents.
pub const ACCENT: u32 = 0x7DD3FC;
pub const GREEN: u32 = 0x62D6A7;
pub const AMBER: u32 = 0xF5B76B;
pub const RED: u32 = 0xFF7188;
pub const PURPLE: u32 = 0xB79CFF;
pub const ORANGE: u32 = 0xF3A95F;
pub const BLUE: u32 = 0x67A9FF;

// Layout rhythm.
pub const HEADER_H: f32 = 46.0;
pub const TOOLBAR_H: f32 = 50.0;
pub const STREAM_HEAD_H: f32 = 34.0;
pub const TABLE_HEAD_H: f32 = 30.0;
pub const ROW_H: f32 = 57.0;
pub const INSPECTOR_W: f32 = 424.0;
pub const FOOTER_H: f32 = 28.0;

// Stream columns.
pub const ROW_PAD_X: f32 = 14.0;
pub const COL_TIME: f32 = 76.0;
pub const COL_SERVICE: f32 = 76.0;
pub const COL_STATUS: f32 = 46.0;
pub const COL_LATENCY: f32 = 54.0;
pub const COL_AUTH: f32 = 24.0;
pub const COL_GAP: f32 = 11.0;

pub fn h(hex: u32, alpha: f32) -> Hsla {
    let mut color: Hsla = rgb(hex).into();
    color.a = alpha;
    color
}

pub fn line(hex: u32, alpha: f32) -> Hsla {
    let mut color: Hsla = rgba(hex).into();
    color.a = alpha;
    color
}

pub fn tint(color: Rgba, alpha: f32) -> Hsla {
    let mut color: Hsla = color.into();
    color.a = alpha;
    color
}

/// Color for a service label. Known services get a designed hue; any other
/// service gets a stable hashed hue from a shared palette, so new AWS
/// services are never rendered as an undifferentiated gray.
pub fn service_color(label: &str) -> Rgba {
    let label = label.trim().to_ascii_uppercase();
    match label.as_str() {
        "S3" => rgb(ORANGE),
        "SQS" => rgb(PURPLE),
        "DYNAMODB" => rgb(BLUE),
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
            let mut hash = 0xcbf2_9ce4_8422_2325_u64;
            for byte in label.as_bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            rgb(PALETTE[hash as usize % PALETTE.len()])
        }
    }
}

pub fn status_color(status: u16) -> Rgba {
    if status == 0 {
        rgb(TEXT_MUTED)
    } else if status >= 500 {
        rgb(RED)
    } else if status >= 400 {
        rgb(0xFF8A9B)
    } else if status >= 300 {
        rgb(AMBER)
    } else {
        rgb(GREEN)
    }
}

pub fn latency_color(ms: u64) -> Rgba {
    if ms >= 2_000 {
        rgb(RED)
    } else if ms >= 500 {
        rgb(AMBER)
    } else {
        rgb(TEXT_MUTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_services_get_a_stable_color() {
        let kinesis = service_color("kinesis");
        assert_eq!(kinesis, service_color("KINESIS"));
        assert_ne!(kinesis, service_color("s3"));
        assert_ne!(kinesis, service_color(""));
        assert_eq!(service_color(""), rgb(0x8B94A6));
        assert_eq!(service_color("s3"), rgb(ORANGE));
    }
}
