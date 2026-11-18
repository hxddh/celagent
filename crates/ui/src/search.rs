//! A compact, real, focusable single-line search field for GPUI.
//!
//! Adapted from GPUI's `input` example: text is shaped directly, the platform
//! input handler is installed during paint, and the usual editing keys work.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, GlobalElementId, InspectorElementId, IntoElement,
    KeyDownEvent, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    Pixels, Point, Render, ShapedLine, SharedString, Style, TextRun, UTF16Selection,
    UnderlineStyle, Window, div, fill, point, prelude::*, px, relative, rgb, size,
};

use crate::{fonts, glyph, i18n, theme};

pub struct SearchInput {
    focus: FocusHandle,
    root_focus: FocusHandle,
    content: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
}

impl SearchInput {
    pub fn new(root_focus: FocusHandle, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            root_focus,
            content: "".into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
        }
    }

    pub fn query(&self) -> &str {
        &self.content
    }

    pub fn handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.content = "".into();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        window.focus(&self.focus);
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify();
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .char_indices()
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .char_indices()
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.content.len())
    }

    fn backspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = text.replace(['\n', '\r'], " ");
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let key = keystroke.key.as_str();
        let secondary = keystroke.modifiers.secondary();

        if secondary {
            match key {
                "a" => {
                    self.select_all(cx);
                    cx.stop_propagation();
                }
                "c" => {
                    self.copy(cx);
                    cx.stop_propagation();
                }
                "x" => {
                    self.cut(window, cx);
                    cx.stop_propagation();
                }
                "v" => {
                    self.paste(window, cx);
                    cx.stop_propagation();
                }
                _ => {}
            }
            return;
        }

        let shift = keystroke.modifiers.shift;
        match key {
            "escape" => {
                if self.content.is_empty() {
                    window.focus(&self.root_focus);
                    cx.notify();
                } else {
                    self.content = "".into();
                    self.selected_range = 0..0;
                    self.selection_reversed = false;
                    self.marked_range = None;
                    cx.notify();
                }
                cx.stop_propagation();
            }
            "enter" => {
                window.focus(&self.root_focus);
                cx.notify();
                cx.stop_propagation();
            }
            "backspace" => {
                self.backspace(window, cx);
                cx.stop_propagation();
            }
            "delete" => {
                self.delete(window, cx);
                cx.stop_propagation();
            }
            "left" => {
                if shift {
                    let offset = if self.selected_range.is_empty() {
                        self.previous_boundary(self.cursor_offset())
                    } else {
                        self.selected_range.start
                    };
                    self.select_to(offset, cx);
                } else if self.selected_range.is_empty() {
                    let offset = self.previous_boundary(self.cursor_offset());
                    self.move_to(offset, cx);
                } else {
                    self.move_to(self.selected_range.start, cx);
                }
                cx.stop_propagation();
            }
            "right" => {
                if shift {
                    let offset = if self.selected_range.is_empty() {
                        self.next_boundary(self.cursor_offset())
                    } else {
                        self.selected_range.end
                    };
                    self.select_to(offset, cx);
                } else if self.selected_range.is_empty() {
                    let offset = self.next_boundary(self.cursor_offset());
                    self.move_to(offset, cx);
                } else {
                    self.move_to(self.selected_range.end, cx);
                }
                cx.stop_propagation();
            }
            "home" => {
                if shift {
                    self.select_to(0, cx);
                } else {
                    self.move_to(0, cx);
                }
                cx.stop_propagation();
            }
            "end" => {
                if shift {
                    self.select_to(self.content.len(), cx);
                } else {
                    self.move_to(self.content.len(), cx);
                }
                cx.stop_propagation();
            }
            // Up/down intentionally bubble so the stream can move its selection.
            _ => {}
        }
    }

    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        line.closest_index_for_x(position.x - bounds.left())
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        self.is_selecting = true;
        let offset = self.index_for_position(event.position);
        if event.modifiers.shift {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
        cx.stop_propagation();
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_selecting {
            let offset = self.index_for_position(event.position);
            self.select_to(offset, cx);
        }
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        utf16_to_utf8(&self.content, offset)
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    fn clamp_range(&self, range: Range<usize>) -> Range<usize> {
        let mut start = range.start.min(self.content.len());
        while start > 0 && !self.content.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = range.end.min(self.content.len()).max(start);
        while end > start && !self.content.is_char_boundary(end) {
            end -= 1;
        }
        start..end
    }
}

impl EntityInputHandler for SearchInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.clamp_range(self.range_from_utf16(&range_utf16));
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.clamp_range(
            range_utf16
                .as_ref()
                .map(|range| self.range_from_utf16(range))
                .or(self.marked_range.clone())
                .unwrap_or(self.selected_range.clone()),
        );

        let new_text = new_text.replace(['\n', '\r'], " ");
        self.content =
            (self.content[0..range.start].to_owned() + &new_text + &self.content[range.end..])
                .into();
        self.selected_range =
            self.clamp_range(range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;
        self.marked_range.take();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.clamp_range(
            range_utf16
                .as_ref()
                .map(|range| self.range_from_utf16(range))
                .or(self.marked_range.clone())
                .unwrap_or(self.selected_range.clone()),
        );

        let new_text = new_text.replace(['\n', '\r'], " ");
        let (next_content, marked, selected) =
            compose_marked_edit(&self.content, range, &new_text, new_selected_range_utf16);
        self.content = next_content.into();
        self.marked_range = marked;
        self.selected_range = self.clamp_range(selected);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let last_layout = self.last_layout.as_ref()?;
        let range = self.clamp_range(self.range_from_utf16(&range_utf16));
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(range.start),
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(range.end),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let last_layout = self.last_layout.as_ref()?;
        let index = last_layout.closest_index_for_x(point.x - bounds.left());
        Some(self.offset_to_utf16(index))
    }
}

impl Focusable for SearchInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

struct SearchTextElement {
    input: Entity<SearchInput>,
    placeholder: SharedString,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for SearchTextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SearchTextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let display_len = if content.is_empty() {
            self.placeholder.len()
        } else {
            content.len()
        };
        let selected_range =
            input.selected_range.start.min(display_len)..input.selected_range.end.min(display_len);
        let cursor = input.cursor_offset().min(display_len);
        let marked_range = input
            .marked_range
            .as_ref()
            .map(|range| range.start.min(display_len)..range.end.min(display_len))
            .filter(|range| range.start < range.end);
        let style = window.text_style();

        let t = theme::p();
        let (display_text, text_color) = if content.is_empty() {
            (self.placeholder.clone(), theme::h(t.text_faint, 0.95))
        } else {
            (content, style.color)
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked_range) = marked_range.as_ref() {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked_range.end - marked_range.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked_range.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let cursor_x = line.x_for_index(cursor);
        let (selection, cursor) = if selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_x, bounds.top() + px(2.0)),
                        size(px(1.5), bounds.bottom() - bounds.top() - px(4.0)),
                    ),
                    rgb(t.accent),
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(selected_range.start),
                            bounds.top() + px(1.0),
                        ),
                        point(
                            bounds.left() + line.x_for_index(selected_range.end),
                            bounds.bottom() - px(1.0),
                        ),
                    ),
                    theme::line(t.accent, 0.22),
                )),
                None,
            )
        };

        PrepaintState {
            line: Some(line),
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );

        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection);
        }
        let line = prepaint.line.take().expect("shaped search text");
        line.paint(bounds.origin, window.line_height(), window, cx)
            .ok();

        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }

        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for SearchInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::p();
        let focused = self.focus.is_focused(window);
        let compact = window.viewport_size().width < px(1_180.0);
        let has_query = !self.content.is_empty();
        let placeholder: SharedString = if compact {
            i18n::pick("Search calls, paths…", "搜索请求、Path…").into()
        } else {
            i18n::pick(
                "Search calls, operations, paths…",
                "搜索请求、Operation、Path…",
            )
            .into()
        };
        let clear = div()
            .id("search-clear")
            .w(px(15.0))
            .h(px(15.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .text_size(px(13.0))
            .line_height(px(14.0))
            .text_color(rgb(t.text_muted))
            .cursor_pointer()
            .hover(move |style| {
                style
                    .bg(theme::h(t.text, 0.06))
                    .text_color(rgb(t.text_soft))
            })
            .child("×")
            .on_click(cx.listener(|this, _, window, cx| this.clear(window, cx)));

        let trailing: gpui::AnyElement = if has_query {
            clear.into_any_element()
        } else {
            div()
                .flex_shrink_0()
                .font_family(fonts::mono())
                .text_size(px(10.5))
                .line_height(px(14.0))
                .text_color(rgb(t.text_faint))
                .child(if cfg!(target_os = "macos") {
                    "⌘K"
                } else {
                    "Ctrl K"
                })
                .into_any_element()
        };

        div()
            .w_full()
            .h(px(30.0))
            .flex()
            .items_center()
            .gap(px(7.0))
            .px(px(9.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(if focused {
                theme::line(t.accent, 0.52)
            } else {
                theme::h(t.border, 1.0)
            })
            .bg(if focused {
                rgb(t.input_focused)
            } else {
                rgb(t.input)
            })
            .text_size(px(13.0))
            .line_height(px(16.0))
            .font_family(fonts::sans())
            .text_color(rgb(t.text_soft))
            .track_focus(&self.focus)
            .cursor_text()
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(glyph::search_icon(if focused {
                theme::h(t.accent, 0.75)
            } else {
                theme::h(t.text_muted, 0.9)
            }))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .child(SearchTextElement {
                        input: cx.entity(),
                        placeholder: placeholder.clone(),
                    }),
            )
            .child(trailing)
    }
}

fn utf16_to_utf8(text: &str, offset_utf16: usize) -> usize {
    let mut utf8_offset = 0;
    let mut utf16_count = 0;
    for ch in text.chars() {
        if utf16_count >= offset_utf16 {
            break;
        }
        utf16_count += ch.len_utf16();
        utf8_offset += ch.len_utf8();
    }
    utf8_offset
}

/// Apply an IME preedit (`new_text`) at `replace_range` inside `content`,
/// computing `new_selected_range_utf16` relative to `new_text` and offsetting
/// by `replace_range.start`.
fn compose_marked_edit(
    content: &str,
    replace_range: Range<usize>,
    new_text: &str,
    new_selected_range_utf16: Option<Range<usize>>,
) -> (String, Option<Range<usize>>, Range<usize>) {
    let base = replace_range.start;
    let next = format!(
        "{}{}{}",
        &content[..replace_range.start],
        new_text,
        &content[replace_range.end..]
    );
    let marked = (!new_text.is_empty()).then_some(base..base + new_text.len());
    let selected = match new_selected_range_utf16 {
        Some(rel_utf16) => {
            let rel_start = utf16_to_utf8(new_text, rel_utf16.start);
            let rel_end = utf16_to_utf8(new_text, rel_utf16.end);
            base + rel_start..base + rel_end
        }
        None => base + new_text.len()..base + new_text.len(),
    };
    (next, marked, selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ime_preedit_computes_selection_relative_to_marked_text() {
        // Existing Chinese prefix ("订单 ": 7 UTF-8 bytes) followed by Pinyin
        // preedit ("zhong'wen", cursor at UTF-16 offset 5 inside preedit).
        let prefix = "订单 ";
        assert_eq!(prefix.len(), 7);
        let (content, marked, selected) =
            compose_marked_edit(prefix, 7..7, "zhong'wen", Some(5..5));
        assert_eq!(content, "订单 zhong'wen");
        assert_eq!(marked, Some(7..16));
        assert_eq!(selected, 12..12, "cursor stays at 'zhong|' inside preedit");

        // Replacing preedit with CJK candidate ("中文": 2 UTF-16 code units, 6 UTF-8 bytes)
        // with candidate selection 0..2 in UTF-16.
        let (content2, marked2, selected2) =
            compose_marked_edit(&content, 7..16, "中文", Some(0..2));
        assert_eq!(content2, "订单 中文");
        assert_eq!(marked2, Some(7..13));
        assert_eq!(selected2, 7..13);
        assert!(content2.is_char_boundary(selected2.start));
        assert!(content2.is_char_boundary(selected2.end));
    }
}
