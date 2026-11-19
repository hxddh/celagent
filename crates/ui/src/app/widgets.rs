//! Small shared building blocks for the Lenslet workbench.

use super::*;

pub(super) fn micro_label(text: &'static str) -> Div {
    let t = theme::p();
    div()
        .text_size(px(10.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(t.text_muted))
        .child(text)
}

pub(super) fn table_label(text: &'static str, width: f32) -> Div {
    div().w(px(width)).child(micro_label(text))
}

pub(super) fn action_button(id: &'static str, label: &'static str) -> Stateful<Div> {
    let t = theme::p();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(21.0))
        .px(px(7.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(rgb(t.border_subtle))
        .bg(theme::h(t.text, 0.025))
        .text_size(px(10.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(t.text_dim))
        .cursor_pointer()
        .hover(move |style| {
            style
                .bg(theme::h(t.text, 0.07))
                .border_color(rgb(t.border))
                .text_color(rgb(t.text))
        })
        .child(label)
}

pub(super) fn section(title: &'static str, content: Div) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(micro_label(title))
        .child(content)
}

pub(super) fn section_with_action(
    title: &'static str,
    meta: Option<String>,
    action: Option<AnyElement>,
    content: impl IntoElement,
) -> Div {
    let t = theme::p();
    let mut header = div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(micro_label(title));
    if let Some(meta) = meta
        && !meta.is_empty()
    {
        header = header.child(
            div()
                .font_family(fonts::mono())
                .text_size(px(10.0))
                .text_color(rgb(t.text_muted))
                .child(meta),
        );
    }
    header = header.child(div().flex_1());
    if let Some(action) = action {
        header = header.child(action);
    }
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(header)
        .child(content)
}

pub(super) fn meta_row(label: &'static str, value: impl IntoElement) -> Div {
    let t = theme::p();
    div()
        .flex()
        .items_start()
        .gap(px(10.0))
        .py(px(5.0))
        .border_b_1()
        .border_color(rgb(t.border_subtle))
        .child(
            div()
                .w(px(96.0))
                .flex_shrink_0()
                .text_size(px(10.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(t.text_muted))
                .child(label.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_size(px(11.5))
                .line_height(px(16.0))
                .text_color(rgb(t.text_soft))
                .overflow_hidden()
                .child(value),
        )
}

pub(super) fn mono_value(text: String) -> Div {
    let t = theme::p();
    div()
        .font_family(fonts::mono())
        .text_size(px(11.0))
        .text_color(rgb(t.text_body))
        .whitespace_nowrap()
        .text_ellipsis()
        .overflow_hidden()
        .child(text)
}

pub(super) fn empty_note(text: &str) -> Div {
    let t = theme::p();
    div()
        .px(px(10.0))
        .py(px(8.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(t.border_subtle))
        .bg(rgb(t.code))
        .text_size(px(11.0))
        .text_color(rgb(t.text_muted))
        .child(text.to_string())
}

pub(super) fn note_text(text: String) -> Div {
    let t = theme::p();
    div()
        .font_family(fonts::mono())
        .text_size(px(10.0))
        .line_height(px(14.5))
        .text_color(rgb(t.text_muted))
        .child(text)
}

pub(super) fn code_block(
    id: &'static str,
    text: String,
    max_height: f32,
    wrap: bool,
) -> Stateful<Div> {
    syntax_code_block(
        id,
        text,
        crate::syntax::SyntaxKind::Plain,
        &[],
        max_height,
        wrap,
        false,
    )
    .0
}

/// GPU-accelerated syntax-highlighted & search-highlighted code viewer with an
/// optional line-number gutter. Returns `(element, search_match_count)`.
pub(super) fn syntax_code_block(
    id: &'static str,
    text: String,
    kind: crate::syntax::SyntaxKind,
    search_tokens: &[String],
    max_height: f32,
    wrap: bool,
    line_numbers: bool,
) -> (Stateful<Div>, usize) {
    let t = theme::p();
    let (spans, match_count) = crate::syntax::highlight_code(&text, kind, search_tokens);
    let line_count = text.lines().count().max(1);
    let show_gutter = line_numbers && !wrap && line_count > 1 && line_count <= 2_500;
    let styled = if spans.is_empty() {
        gpui::StyledText::new(text)
    } else {
        gpui::StyledText::new(text).with_highlights(spans)
    };

    let base = div()
        .id(id)
        .w_full()
        .max_h(px(max_height))
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(t.border))
        .bg(rgb(t.code))
        .font_family(fonts::mono())
        .text_size(px(11.5))
        .line_height(px(17.0))
        .text_color(rgb(t.text_body));

    let elem = if wrap {
        base.p(px(9.0))
            .overflow_y_scroll()
            .scrollbar_width(px(5.0))
            .child(div().w_full().min_w(px(0.0)).child(styled))
    } else if show_gutter {
        let mut nums = String::with_capacity(line_count * 4);
        for i in 1..=line_count {
            use std::fmt::Write as _;
            if i > 1 {
                nums.push('\n');
            }
            let _ = write!(nums, "{i:>3}");
        }
        base.overflow_scroll()
            .scrollbar_width(px(5.0))
            .flex()
            .items_start()
            .child(
                div()
                    .flex_shrink_0()
                    .py(px(9.0))
                    .px(px(8.0))
                    .border_r_1()
                    .border_color(rgb(t.border_subtle))
                    .bg(theme::h(t.text, 0.015))
                    .text_color(rgb(t.text_faint))
                    .whitespace_nowrap()
                    .child(nums),
            )
            .child(
                div()
                    .py(px(9.0))
                    .px(px(10.0))
                    .flex()
                    .flex_col()
                    .items_start()
                    .flex_shrink_0()
                    .whitespace_nowrap()
                    .child(styled),
            )
    } else {
        base.p(px(9.0))
            .overflow_scroll()
            .scrollbar_width(px(5.0))
            .flex()
            .items_start()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .flex_shrink_0()
                    .whitespace_nowrap()
                    .child(styled),
            )
    };
    (elem, match_count)
}

/// Outlined pill badge in a semantic color.
pub(super) fn badge(text: impl Into<SharedString>, color: u32) -> Div {
    div()
        .h(px(18.0))
        .px(px(6.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme::line(color, 0.35))
        .bg(theme::h(color, 0.09))
        .font_family(fonts::mono())
        .text_size(px(10.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(color))
        .whitespace_nowrap()
        .child(text.into())
}
