//! Small icons and the Lenslet aperture mark, drawn with GPUI primitives.

use std::f32::consts::PI;

use gpui::{Hsla, PathBuilder, Window, canvas, div, point, prelude::*, px, rgb};

use crate::theme;

/// The lens/aperture mark: a machined tile with an iris and six blades.
pub fn lens_mark(size: f32) -> impl IntoElement {
    let t = theme::p();
    let ring = (size * 0.50).max(6.0);
    let dot = (size * 0.15).max(2.0);
    let blade = theme::h(t.accent, 0.82);
    let blade_dim = theme::h(t.accent, 0.34);

    div()
        .relative()
        .flex_shrink_0()
        .w(px(size))
        .h(px(size))
        .rounded(px((size * 0.28).max(4.0)))
        .bg(rgb(t.mark_bg))
        .border_1()
        .border_color(theme::line(t.accent, 0.22))
        .overflow_hidden()
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    let cx = bounds.left().to_f64() as f32 + size / 2.0;
                    let cy = bounds.top().to_f64() as f32 + size / 2.0;
                    let inner = size * 0.14;
                    let outer = size * 0.37;
                    for index in 0..6 {
                        let angle = index as f32 * PI / 3.0 + 0.42;
                        let tail = angle + 0.62;
                        paint_line(
                            window,
                            (cx + inner * angle.cos(), cy + inner * angle.sin()),
                            (cx + outer * tail.cos(), cy + outer * tail.sin()),
                            (size * 0.055).max(1.0),
                            if index % 2 == 0 { blade } else { blade_dim },
                        );
                    }
                },
            )
            .absolute()
            .inset_0(),
        )
        .child(
            div()
                .absolute()
                .left(px((size - ring) / 2.0))
                .top(px((size - ring) / 2.0))
                .w(px(ring))
                .h(px(ring))
                .rounded_full()
                .border_1()
                .border_color(theme::h(t.accent, 0.55)),
        )
        .child(
            div()
                .absolute()
                .left(px((size - dot) / 2.0))
                .top(px((size - dot) / 2.0))
                .w(px(dot))
                .h(px(dot))
                .rounded_full()
                .bg(theme::h(t.accent, 0.95)),
        )
}

pub fn search_icon(color: Hsla) -> impl IntoElement {
    div()
        .relative()
        .flex_shrink_0()
        .w(px(11.0))
        .h(px(11.0))
        .child(
            div()
                .absolute()
                .left(px(0.5))
                .top(px(0.5))
                .w(px(7.0))
                .h(px(7.0))
                .rounded_full()
                .border_1()
                .border_color(color),
        )
        .child(
            canvas(
                |_, _, _| (),
                move |_, _, window, _| {
                    paint_line(window, (6.6, 6.6), (10.0, 10.0), 1.1, color);
                },
            )
            .absolute()
            .inset_0(),
        )
}

/// A tiny key silhouettes whether SigV4 authorization was present.
pub fn auth_mark(present: bool) -> impl IntoElement {
    let t = theme::p();
    let color = if present {
        theme::h(t.green, 0.9)
    } else {
        theme::h(t.text_faint, 0.65)
    };
    div()
        .relative()
        .flex_shrink_0()
        .w(px(12.0))
        .h(px(8.0))
        .child(
            div()
                .absolute()
                .left(px(0.0))
                .top(px(0.5))
                .w(px(5.5))
                .h(px(5.5))
                .rounded_full()
                .border_1()
                .border_color(color),
        )
        .child(
            div()
                .absolute()
                .left(px(5.0))
                .top(px(2.5))
                .w(px(6.5))
                .h(px(1.0))
                .bg(color),
        )
        .child(
            div()
                .absolute()
                .left(px(8.5))
                .top(px(2.5))
                .w(px(1.0))
                .h(px(3.0))
                .bg(color),
        )
}

pub fn pause_icon(color: Hsla) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(2.0))
        .child(div().w(px(2.0)).h(px(8.0)).rounded(px(0.5)).bg(color))
        .child(div().w(px(2.0)).h(px(8.0)).rounded(px(0.5)).bg(color))
}

pub fn play_icon(color: Hsla) -> impl IntoElement {
    div().w(px(8.0)).h(px(9.0)).child(
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let cx = bounds.left().to_f64() as f32;
                let cy = bounds.top().to_f64() as f32;
                let mut path = PathBuilder::fill();
                path.move_to(point(px(cx + 0.5), px(cy)));
                path.line_to(point(px(cx + 0.5), px(cy + 9.0)));
                path.line_to(point(px(cx + 7.5), px(cy + 4.5)));
                path.close();
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            },
        )
        .absolute()
        .inset_0(),
    )
}

pub fn dot(color: impl Into<Hsla>, size: f32) -> impl IntoElement {
    let color: Hsla = color.into();
    div()
        .flex_shrink_0()
        .w(px(size))
        .h(px(size))
        .rounded_full()
        .bg(color)
}

fn paint_line(window: &mut Window, from: (f32, f32), to: (f32, f32), thickness: f32, color: Hsla) {
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    let length = (dx * dx + dy * dy).sqrt();
    if length <= f32::EPSILON {
        return;
    }
    let nx = -dy / length * thickness / 2.0;
    let ny = dx / length * thickness / 2.0;
    let mut path = PathBuilder::fill();
    path.move_to(point(px(from.0 + nx), px(from.1 + ny)));
    path.line_to(point(px(to.0 + nx), px(to.1 + ny)));
    path.line_to(point(px(to.0 - nx), px(to.1 - ny)));
    path.line_to(point(px(from.0 - nx), px(from.1 - ny)));
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}
