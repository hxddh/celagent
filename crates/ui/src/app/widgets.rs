//! Small shared building blocks.

use super::*;

pub(super) fn micro_label(text: &'static str) -> Div {
    div()
        .text_size(px(10.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text)
}

pub(super) fn table_label(text: &'static str, width: f32) -> Div {
    div().w(px(width)).child(micro_label(text))
}

pub(super) fn action_button(id: &'static str, label: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(20.0))
        .px(px(6.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(rgb(theme::BORDER_SUBTLE))
        .bg(theme::h(theme::TEXT, 0.015))
        .text_size(px(10.0))
        .text_color(rgb(theme::TEXT_MUTED))
        .cursor_pointer()
        .hover(|style| {
            style
                .bg(theme::h(theme::TEXT, 0.06))
                .border_color(rgb(theme::BORDER))
                .text_color(rgb(theme::TEXT_SOFT))
        })
        .child(label)
}

pub(super) fn section(title: &'static str, content: Div) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(micro_label(title))
        .child(content)
}

pub(super) fn section_with_action(
    title: &'static str,
    meta: Option<String>,
    action: Option<AnyElement>,
    content: impl IntoElement,
) -> Div {
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
                .text_size(px(9.5))
                .text_color(rgb(theme::TEXT_FAINT))
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
    div()
        .flex()
        .items_start()
        .gap(px(10.0))
        .py(px(5.0))
        .border_b_1()
        .border_color(rgb(theme::BORDER_SUBTLE))
        .child(
            div()
                .w(px(92.0))
                .flex_shrink_0()
                .text_size(px(10.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(theme::TEXT_MUTED))
                .child(label.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_size(px(11.5))
                .line_height(px(16.0))
                .text_color(rgb(theme::TEXT_SOFT))
                .overflow_hidden()
                .child(value),
        )
}

pub(super) fn mono_value(text: String) -> Div {
    div()
        .font_family(fonts::mono())
        .text_size(px(11.0))
        .text_color(rgb(theme::TEXT_BODY))
        .whitespace_nowrap()
        .text_ellipsis()
        .overflow_hidden()
        .child(text)
}

pub(super) fn text_value(text: String, color: u32) -> Div {
    div().text_color(rgb(color)).child(text)
}

pub(super) fn empty_note(text: &str) -> Div {
    div()
        .p(px(10.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(theme::BORDER_SUBTLE))
        .bg(rgb(theme::CODE))
        .text_size(px(11.0))
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_string())
}

pub(super) fn note_text(text: String) -> Div {
    div()
        .font_family(fonts::mono())
        .text_size(px(10.0))
        .line_height(px(14.0))
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text)
}

pub(super) fn code_block(id: &'static str, text: String, max_height: f32) -> Stateful<Div> {
    div()
        .id(id)
        .w_full()
        .p(px(9.0))
        .max_h(px(max_height))
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(theme::BORDER))
        .bg(rgb(theme::CODE))
        .font_family(fonts::mono())
        .text_size(px(12.0))
        .line_height(px(17.0))
        .text_color(rgb(theme::TEXT_BODY))
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
                .child(text),
        )
}
