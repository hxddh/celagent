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

use crate::{fonts, glyph, theme};

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
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
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
        self.content =
            (self.content[0..range.start].to_owned() + &new_text + &self.content[range.end..])
                .into();
        if new_text.is_empty() {
            self.marked_range = None;
        } else {
            self.marked_range = Some(range.start..range.start + new_text.len());
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .map(|range| range.start + range.start..range.start + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
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

        let (display_text, text_color) = if content.is_empty() {
            (self.placeholder.clone(), theme::h(theme::TEXT_FAINT, 0.95))
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
                    rgb(theme::ACCENT),
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
                    theme::line(theme::ACCENT, 0.22),
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
        let focused = self.focus.is_focused(window);
        let compact = window.viewport_size().width < px(1_180.0);
        let has_query = !self.content.is_empty();
        let placeholder: SharedString = if compact {
            "Search calls, paths…".into()
        } else {
            "Search calls, operations, paths…".into()
        };
        let clear = div()
            .id("search-clear")
            .w(px(15.0))
            .h(px(15.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .text_size(px(12.0))
            .line_height(px(12.0))
            .text_color(rgb(theme::TEXT_MUTED))
            .cursor_pointer()
            .hover(|style| {
                style
                    .bg(theme::h(theme::TEXT, 0.06))
                    .text_color(rgb(theme::TEXT_SOFT))
            })
            .child("×")
            .on_click(cx.listener(|this, _, window, cx| this.clear(window, cx)));

        let trailing: gpui::AnyElement = if has_query {
            clear.into_any_element()
        } else {
            div()
                .flex_shrink_0()
                .font_family(fonts::mono())
                .text_size(px(8.5))
                .line_height(px(12.0))
                .text_color(rgb(theme::TEXT_FAINT))
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
                theme::line(theme::ACCENT, 0.52)
            } else {
                theme::h(theme::BORDER, 1.0)
            })
            .bg(if focused {
                rgb(0x0A0E14)
            } else {
                rgb(0x0B0F15)
            })
            .text_size(px(12.0))
            .line_height(px(16.0))
            .font_family(fonts::sans())
            .text_color(rgb(theme::TEXT_SOFT))
            .track_focus(&self.focus)
            .cursor_text()
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(glyph::search_icon(if focused {
                theme::h(theme::ACCENT, 0.75)
            } else {
                theme::h(theme::TEXT_MUTED, 0.9)
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
