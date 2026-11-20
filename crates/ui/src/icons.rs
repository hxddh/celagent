//! Embedded crisp 16×16 vector SVG icon set (`gpui::AssetSource`).
//!
//! Eliminates low-res `PathBuilder` line approximations with pixel-aligned
//! geometric vector icons rendered natively by GPUI's SVG rasterizer.

use std::borrow::Cow;

use gpui::{AssetSource, Hsla, Result, SharedString, Svg, prelude::*, px, svg};

pub struct Assets;

const ICONS: &[(&str, &str)] = &[
    (
        "icons/search.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="7" cy="7" r="4.5"/><path d="M10.4 10.4L14 14"/></svg>"#,
    ),
    (
        "icons/sidebar-left.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="2.5" width="12" height="11" rx="2"/><path d="M6.2 2.5v11"/></svg>"#,
    ),
    (
        "icons/sidebar-right.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="2.5" width="12" height="11" rx="2"/><path d="M9.8 2.5v11"/></svg>"#,
    ),
    (
        "icons/settings.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="8" r="2.3"/><path d="M8 1.8v1.6M8 12.6v1.6M1.8 8h1.6M12.6 8h1.6M3.6 3.6l1.15 1.15M11.25 11.25l1.15 1.15M12.4 3.6l-1.15 1.15M4.75 11.25L3.6 12.4"/></svg>"#,
    ),
    (
        "icons/play.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="black"><path d="M4.5 3.2c0-.6.65-.97 1.17-.66l7.2 4.3c.5.3.5 1.02 0 1.32l-7.2 4.3c-.52.31-1.17-.06-1.17-.66V3.2z"/></svg>"#,
    ),
    (
        "icons/stop.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="black"><rect x="3.5" y="3.5" width="9" height="9" rx="1.8"/></svg>"#,
    ),
    (
        "icons/refresh.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M13.5 8a5.5 5.5 0 1 1-1.45-3.72"/><path d="M13.5 2.5v2.6h-2.6"/></svg>"#,
    ),
    (
        "icons/copy.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><rect x="5.5" y="5.5" width="8" height="8" rx="1.6"/><path d="M3.5 10.5H3a1.5 1.5 0 0 1-1.5-1.5V3A1.5 1.5 0 0 1 3 1.5h6A1.5 1.5 0 0 1 10.5 3v.5"/></svg>"#,
    ),
    (
        "icons/folder.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M2 4.2A1.5 1.5 0 0 1 3.5 2.7h2.8l1.4 1.6h4.8A1.5 1.5 0 0 1 14 5.8v6A1.5 1.5 0 0 1 12.5 13.3h-9A1.5 1.5 0 0 1 2 11.8V4.2z"/></svg>"#,
    ),
    (
        "icons/download.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M8 2.5v7.5"/><path d="M5 7.3L8 10.3l3-3"/><path d="M2.8 13.2h10.4"/></svg>"#,
    ),
    (
        "icons/shield.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M8 1.8l5 2v3.9c0 3.2-2.1 5.6-5 6.6-2.9-1-5-3.4-5-6.6V3.8l5-2z"/></svg>"#,
    ),
    (
        "icons/pin.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="black"><path d="M8 1.7l1.85 3.75 4.15.6-3 2.92.71 4.13L8 11.15 4.29 13.1 5 8.97 2 6.05l4.15-.6L8 1.7z"/></svg>"#,
    ),
    (
        "icons/edit.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M11.2 2.4l2.4 2.4-7.9 7.9-3.1.7.7-3.1 7.9-7.9z"/></svg>"#,
    ),
    (
        "icons/chevron-down.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M4.2 6.2L8 10l3.8-3.8"/></svg>"#,
    ),
    (
        "icons/chevron-right.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M6.2 4.2L10 8l-3.8 3.8"/></svg>"#,
    ),
    (
        "icons/diff.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.5" stroke-linecap="round"><path d="M8 3.2v5.6M5.2 6h5.6M5.2 12.2h5.6"/></svg>"#,
    ),
    (
        "icons/menu.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.5" stroke-linecap="round"><path d="M2.5 4.2h11M2.5 8h11M2.5 11.8h11"/></svg>"#,
    ),
    (
        "icons/plus.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.6" stroke-linecap="round"><path d="M8 3v10M3 8h10"/></svg>"#,
    ),
    (
        "icons/terminal.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><rect x="1.8" y="2.5" width="12.4" height="11" rx="2"/><path d="M4.5 6l2.4 2-2.4 2M8.2 10h3.2"/></svg>"#,
    ),
];

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, data)| Cow::Borrowed(data.as_bytes())))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}

pub fn icon(path: &'static str, size_px: f32, color: impl Into<Hsla>) -> Svg {
    svg()
        .path(path)
        .size(px(size_px))
        .flex_shrink_0()
        .text_color(color.into())
}
