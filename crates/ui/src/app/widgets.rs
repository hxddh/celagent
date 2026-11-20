//! Small shared building blocks: buttons, chips, segmented controls, badges,
//! tooltips, menus and popover chrome. Everything takes the [`Ui`] look.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, AnyView, App, Div, ElementId, FontWeight, IntoElement,
    ParentElement, Render, SharedString, Stateful, Styled, Window, div, ease_out_quint, prelude::*,
    px, rgb,
};

use crate::{
    fonts,
    theme::{self, Ui, text},
};

/// A tooltip: a label and, optionally, its shortcut.
pub struct Tip {
    text: SharedString,
    key: Option<SharedString>,
}

impl Render for Tip {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let _scope = crate::fsguard::RenderScope::enter();
        let ui = Ui::of(cx);
        let t = ui.p;
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(t.border))
            .bg(rgb(t.panel_raised))
            .shadow(ui.elevation())
            .font(fonts::sans_font())
            .text_size(px(text::SM))
            .text_color(rgb(t.text_soft))
            .child(self.text.clone())
            .when_some(self.key.clone(), |el, key| {
                el.child(
                    div()
                        .font(fonts::mono_font())
                        .text_size(px(text::XS))
                        .text_color(rgb(t.text_muted))
                        .child(key),
                )
            })
    }
}

/// Builder for `.tooltip(...)`.
pub fn tip(
    text: impl Into<SharedString>,
    key: Option<String>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    let key: Option<SharedString> = key.map(Into::into);
    move |_, cx| {
        cx.new(|_| Tip {
            text: text.clone(),
            key: key.clone(),
        })
        .into()
    }
}

/// A quiet bordered button with a label (≥ 24px tall).
pub fn button(ui: &Ui, id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    styled_button(ui, id, label, ui.p.text_soft, None)
}

/// The accent-tinted primary action.
pub fn primary_button(
    ui: &Ui,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
) -> Stateful<Div> {
    styled_button(ui, id, label, ui.p.accent, Some(ui.p.accent))
}

/// A destructive action.
pub fn danger_button(
    ui: &Ui,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
) -> Stateful<Div> {
    styled_button(ui, id, label, ui.p.red, None)
}

fn styled_button(
    ui: &Ui,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    color: u32,
    tint: Option<u32>,
) -> Stateful<Div> {
    let t = ui.p;
    let (border, bg, hover_bg) = match tint {
        Some(tint) => (
            theme::h(tint, 0.45),
            theme::h(tint, 0.14),
            theme::h(tint, 0.22),
        ),
        None => (
            theme::h(t.border, 1.0),
            theme::h(t.text, 0.03),
            theme::h(t.text, 0.08),
        ),
    };
    let hover_text = if tint.is_some() || color != t.text_soft {
        color
    } else {
        t.text
    };
    div()
        .id(id)
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .gap(px(6.0))
        .h(px(24.0))
        .px(px(8.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_size(px(text::SM))
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(color))
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg).text_color(rgb(hover_text)))
        .active(move |s| s.opacity(0.85))
        .child(label.into())
}

/// A square icon-only button; pair with `.tooltip(...)`.
pub fn icon_button(
    ui: &Ui,
    id: impl Into<ElementId>,
    active: bool,
    icon: impl IntoElement,
) -> Stateful<Div> {
    let t = ui.p;
    div()
        .id(id)
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(28.0))
        .rounded(px(6.0))
        .when(active, |el| el.bg(theme::h(t.accent, 0.12)))
        .cursor_pointer()
        .hover(move |s| s.bg(theme::h(t.text, 0.08)))
        .active(move |s| s.bg(theme::h(t.text, 0.12)))
        .child(icon)
}

/// A filter chip with an optional count; `color` tints it when active.
pub fn chip(
    ui: &Ui,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    count: Option<usize>,
    active: bool,
    color: u32,
) -> Stateful<Div> {
    let t = ui.p;
    div()
        .id(id)
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(6.0))
        .h(px(24.0))
        .px(px(8.0))
        .rounded(px(12.0))
        .border_1()
        .border_color(if active {
            theme::h(color, 0.55)
        } else {
            theme::h(t.border, 1.0)
        })
        .bg(if active {
            theme::h(color, 0.14)
        } else {
            theme::h(t.text, 0.0)
        })
        .text_size(px(text::SM))
        .font_weight(FontWeight::MEDIUM)
        .text_color(if active { rgb(color) } else { rgb(t.text_dim) })
        .cursor_pointer()
        .hover(move |s| s.bg(theme::h(t.text, 0.06)).text_color(rgb(t.text)))
        .child(label.into())
        .when_some(count, |el, count| {
            el.child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(text::XS))
                    .text_color(if active {
                        theme::h(color, 0.9)
                    } else {
                        theme::h(t.text_muted, 1.0)
                    })
                    .child(count.to_string()),
            )
        })
}

/// One segment of a segmented control.
pub struct Segment {
    pub id: &'static str,
    pub label: SharedString,
    pub active: bool,
    pub tooltip: Option<(String, Option<String>)>,
}

/// A compact segmented control; `on` is called with the segment index.
pub fn segmented(
    ui: &Ui,
    segments: Vec<Segment>,
    on: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    let t = ui.p;
    let mut row = div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .p(px(2.0))
        .gap(px(2.0))
        .h(px(28.0))
        .rounded(px(7.0))
        .bg(theme::h(t.text, 0.05))
        .border_1()
        .border_color(rgb(t.border_subtle));
    for (index, segment) in segments.into_iter().enumerate() {
        let on = on.clone();
        let mut item = div()
            .id(segment.id)
            .flex()
            .items_center()
            .justify_center()
            .h(px(22.0))
            .px(px(10.0))
            .rounded(px(5.0))
            .text_size(px(text::SM))
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .on_click(move |_, window, cx| on(index, window, cx))
            .child(segment.label);
        item = if segment.active {
            item.bg(rgb(t.panel_raised))
                .text_color(rgb(t.text))
                .border_1()
                .border_color(rgb(t.border))
        } else {
            item.text_color(rgb(t.text_dim))
                .hover(move |s| s.text_color(rgb(t.text)).bg(theme::h(t.text, 0.04)))
        };
        if let Some((text, key)) = segment.tooltip {
            item = item.tooltip(tip(text, key));
        }
        row = row.child(item);
    }
    row
}

/// A small outlined badge in a semantic color.
pub fn badge(ui: &Ui, label: impl Into<SharedString>, color: u32) -> Div {
    let _ = ui;
    div()
        .flex_shrink_0()
        .h(px(16.0))
        .px(px(5.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme::h(color, 0.38))
        .bg(theme::h(color, 0.10))
        .font(fonts::mono_font())
        .text_size(px(9.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(color))
        .whitespace_nowrap()
        .child(label.into())
}

/// Uppercase section label.
pub fn label(ui: &Ui, text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(text::XS))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(ui.p.text_muted))
        .child(text.into())
}

/// A monospace value that ellipsizes (with contextual ligatures disabled for ARNs).
pub fn mono(ui: &Ui, value: impl Into<SharedString>) -> Div {
    div()
        .min_w(px(0.0))
        .font(fonts::mono_font())
        .text_size(px(text::SM))
        .text_color(rgb(ui.p.text_body))
        .whitespace_nowrap()
        .overflow_hidden()
        .text_ellipsis()
        .child(value.into())
}

/// `label: value` row for metadata lists.
pub fn meta_row(ui: &Ui, name: impl Into<SharedString>, value: impl IntoElement) -> Div {
    let t = ui.p;
    div()
        .flex()
        .items_center()
        .gap(px(12.0))
        .min_h(px(24.0))
        .border_b_1()
        .border_color(rgb(t.border_subtle))
        .child(
            div()
                .w(px(104.0))
                .flex_shrink_0()
                .text_size(px(text::SM))
                .text_color(rgb(t.text_muted))
                .child(name.into()),
        )
        .child(div().flex_1().min_w(px(0.0)).child(value))
}

/// A popover/menu card. Fades in quickly unless motion is reduced.
pub fn popover_card(ui: &Ui, id: &'static str, width: f32) -> Stateful<Div> {
    let t = ui.p;
    div()
        .id(id)
        .w(px(width))
        .flex()
        .flex_col()
        .p(px(4.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(rgb(t.border))
        .bg(rgb(t.panel_raised))
        .shadow(ui.elevation())
        .font(fonts::sans_font())
        .occlude()
}

/// Fade (and settle 4px) in over ~120ms.
pub fn appear(ui: &Ui, id: impl Into<ElementId>, element: Stateful<Div>) -> AnyElement {
    if ui.reduce_motion {
        return element.into_any_element();
    }
    element
        .with_animation(
            id,
            Animation::new(Duration::from_millis(120)).with_easing(ease_out_quint()),
            |el, delta| el.opacity(delta).mt(px((1.0 - delta) * -4.0)),
        )
        .into_any_element()
}

/// One menu row: label, optional shortcut hint, hover, ≥ 24px.
pub fn menu_item(
    ui: &Ui,
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    hint: Option<String>,
    selected: bool,
    destructive: bool,
) -> Stateful<Div> {
    let t = ui.p;
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(12.0))
        .h(px(28.0))
        .flex_shrink_0()
        .px(px(8.0))
        .rounded(px(5.0))
        .text_size(px(text::MD))
        .text_color(if destructive {
            rgb(t.red)
        } else {
            rgb(t.text_soft)
        })
        .when(selected, |el| {
            el.bg(theme::h(t.accent, 0.16)).text_color(rgb(t.text))
        })
        .cursor_pointer()
        .hover(move |s| s.bg(theme::h(t.accent, 0.14)).text_color(rgb(t.text)))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(title.into()),
        )
        .when_some(hint, |el, hint| {
            el.child(
                div()
                    .flex_shrink_0()
                    .font_family(fonts::mono())
                    .text_size(px(text::XS))
                    .text_color(rgb(t.text_muted))
                    .child(hint),
            )
        })
}

pub fn separator(ui: &Ui) -> Div {
    div()
        .h(px(1.0))
        .mx(px(4.0))
        .my(px(4.0))
        .bg(rgb(ui.p.border_subtle))
}

/// Centered empty/loading/error state with a title, guidance and an
/// optional command to copy.
pub fn empty_state(
    ui: &Ui,
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    command: Option<AnyElement>,
) -> Div {
    let t = ui.p;
    div()
        .flex_1()
        .min_h(px(0.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(12.0))
        .p(px(32.0))
        .child(crate::glyph::lens_mark(&t, 32.0))
        .child(
            div()
                .text_size(px(text::XL))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(t.text))
                .child(title.into()),
        )
        .child(
            div()
                .max_w(px(440.0))
                .text_center()
                .text_size(px(text::MD))
                .line_height(px(18.0))
                .text_color(rgb(t.text_dim))
                .child(detail.into()),
        )
        .children(command)
}

/// A monospace command line with a Copy action.
pub fn command_line(
    ui: &Ui,
    command: &str,
    on_copy: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let t = ui.p;
    div()
        .flex()
        .items_center()
        .gap(px(12.0))
        .pl(px(12.0))
        .pr(px(4.0))
        .h(px(32.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(rgb(t.border))
        .bg(rgb(t.code))
        .child(
            div()
                .font_family(fonts::mono())
                .text_size(px(text::MD))
                .text_color(rgb(t.text_soft))
                .child(format!("$ {command}")),
        )
        .child(
            button(ui, "copy-command", ui.t("Copy", "复制"))
                .on_click(move |_, window, cx| on_copy(window, cx)),
        )
        .into_any_element()
}

/// Skeleton rows shown while a run loads, so the first frame is never blank.
pub fn skeleton_rows(ui: &Ui, count: usize) -> Div {
    let t = ui.p;
    let mut col = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .overflow_hidden();
    for index in 0..count {
        let width = 40.0 + ((index * 37) % 45) as f32;
        col = col.child(
            div()
                .flex()
                .items_center()
                .gap(px(theme::COL_GAP))
                .h(px(ui.row_height()))
                .px(px(theme::ROW_PAD_X))
                .border_b_1()
                .border_color(rgb(t.border_subtle))
                .child(
                    div()
                        .w(px(64.0))
                        .h(px(8.0))
                        .rounded(px(4.0))
                        .bg(theme::h(t.text, 0.06)),
                )
                .child(
                    div()
                        .w(px(56.0))
                        .h(px(8.0))
                        .rounded(px(4.0))
                        .bg(theme::h(t.text, 0.06)),
                )
                .child(
                    div()
                        .w(gpui::relative(width / 100.0))
                        .h(px(8.0))
                        .rounded(px(4.0))
                        .bg(theme::h(t.text, 0.05)),
                ),
        );
    }
    col
}
