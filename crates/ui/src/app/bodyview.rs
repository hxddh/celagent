//! A line-virtualized, selectable text viewer for payloads and headers: one
//! `uniform_list` row per display line with its gutter, so a body of any size
//! renders only what is on screen. Drag (or Shift-click) selects lines;
//! ⌘C copies them (or the whole document), ⌘A selects everything.

use std::sync::Arc;

use gpui::{
    App, ClipboardItem, Context, ElementId, EventEmitter, FocusHandle, Focusable, MouseButton,
    MouseDownEvent, MouseMoveEvent, Render, SharedString, StyledText, UniformListScrollHandle,
    Window, div, prelude::*, px, rgb, uniform_list,
};

use crate::{
    body::BodyDoc,
    fonts,
    theme::{self, BODY_LINE_H, Ui, text},
};

pub enum BodyViewEvent {
    Copied(usize),
}

pub struct BodyView {
    id: SharedString,
    doc: Arc<BodyDoc>,
    /// `(anchor, head)` line indices.
    selection: Option<(usize, usize)>,
    dragging: bool,
    loading: bool,
    placeholder: SharedString,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
}

impl EventEmitter<BodyViewEvent> for BodyView {}

impl Focusable for BodyView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BodyView {
    pub fn new(id: &'static str, cx: &mut Context<Self>) -> Self {
        BodyView {
            id: id.into(),
            doc: Arc::new(BodyDoc::default()),
            selection: None,
            dragging: false,
            loading: false,
            placeholder: SharedString::default(),
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
        }
    }

    pub fn set_doc(&mut self, doc: Arc<BodyDoc>, cx: &mut Context<Self>) {
        if !Arc::ptr_eq(&self.doc, &doc) {
            let same_text = self.doc.text == doc.text;
            self.doc = doc;
            if !same_text {
                self.selection = None;
                self.scroll
                    .scroll_to_item_strict(0, gpui::ScrollStrategy::Top);
            }
        }
        self.loading = false;
        cx.notify();
    }

    pub fn set_loading(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        cx.notify();
    }

    pub fn set_placeholder(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.placeholder = text.into();
        cx.notify();
    }

    pub fn doc(&self) -> &Arc<BodyDoc> {
        &self.doc
    }

    /// Copy the selected lines, or everything when nothing is selected.
    pub fn copy(&mut self, cx: &mut Context<Self>) {
        if self.doc.is_empty() {
            return;
        }
        let last = self.doc.lines.len().saturating_sub(1);
        let (first, end) = self
            .selection
            .map(|(a, b)| (a.min(b), a.max(b)))
            .unwrap_or((0, last));
        let text = self.doc.copy_lines(first, end);
        let bytes = text.len();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        cx.emit(BodyViewEvent::Copied(bytes));
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        if !self.doc.lines.is_empty() {
            self.selection = Some((0, self.doc.lines.len() - 1));
            cx.notify();
        }
    }

    fn press(
        &mut self,
        line: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        if event.click_count >= 3 {
            self.selection = Some((0, self.doc.lines.len().saturating_sub(1)));
        } else {
            self.selection = Some(match (event.modifiers.shift, self.selection) {
                (true, Some((anchor, _))) => (anchor, line),
                _ => (line, line),
            });
            self.dragging = true;
        }
        cx.notify();
    }

    fn drag_to(&mut self, line: usize, cx: &mut Context<Self>) {
        if self.dragging
            && let Some((anchor, head)) = self.selection
            && head != line
        {
            self.selection = Some((anchor, line));
            cx.notify();
        }
    }
}

impl Render for BodyView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scope = crate::fsguard::RenderScope::enter();
        let ui = Ui::of(cx);
        let t = ui.p;
        let doc = self.doc.clone();
        let container = div()
            .id(ElementId::Name(self.id.clone()))
            .size_full()
            .key_context("Body")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(if self.focus.is_focused(_window) {
                theme::h(t.accent, 0.45)
            } else {
                theme::h(t.border, 1.0)
            })
            .bg(rgb(t.code))
            .overflow_hidden()
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _, _, _| view.dragging = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|view, _, _, _| view.dragging = false),
            );

        if doc.lines.is_empty() || (doc.lines.len() == 1 && doc.lines[0].is_empty()) {
            let note = if self.loading {
                SharedString::from(ui.t("Loading…", "加载中…"))
            } else {
                self.placeholder.clone()
            };
            return container.child(
                div()
                    .p(px(12.0))
                    .text_size(px(text::SM))
                    .text_color(rgb(t.text_muted))
                    .child(note),
            );
        }

        let digits = doc.lines.len().to_string().len().max(2);
        let gutter_w = 16.0 + digits as f32 * 7.0;
        let selection = self.selection.map(|(a, b)| (a.min(b), a.max(b)));
        let entity = cx.entity().downgrade();
        let list = uniform_list(
            ElementId::Name(format!("{}-lines", self.id).into()),
            doc.lines.len(),
            move |range, _window, cx| {
                let _scope = crate::fsguard::RenderScope::enter();
                let ui = Ui::of(cx);
                let t = ui.p;
                range
                    .map(|index| {
                        let number = doc.numbers[index];
                        let selected = selection.is_some_and(|(a, b)| index >= a && index <= b);
                        let line = doc.line_text(index);
                        let styled = StyledText::new(SharedString::from(line.to_string()))
                            .with_highlights(doc.spans[index].iter().cloned());
                        let down = entity.clone();
                        let moved = entity.clone();
                        div()
                            .id(index)
                            .flex()
                            .flex_row()
                            .h(px(BODY_LINE_H))
                            .w_full()
                            .when(selected, |el| el.bg(theme::h(t.accent, 0.18)))
                            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                                down.update(cx, |view, cx| view.press(index, event, window, cx))
                                    .ok();
                            })
                            .on_mouse_move(move |event: &MouseMoveEvent, _, cx| {
                                if event.pressed_button == Some(MouseButton::Left) {
                                    moved.update(cx, |view, cx| view.drag_to(index, cx)).ok();
                                }
                            })
                            .child(
                                div()
                                    .w(px(gutter_w))
                                    .flex_shrink_0()
                                    .pr(px(8.0))
                                    .text_right()
                                    .text_color(rgb(t.text_faint))
                                    .border_r_1()
                                    .border_color(rgb(t.border_subtle))
                                    .child(if doc.continued[index] {
                                        "↪".to_string()
                                    } else {
                                        number.to_string()
                                    }),
                            )
                            .child(
                                div()
                                    .pl(px(8.0))
                                    .pr(px(16.0))
                                    .whitespace_nowrap()
                                    .text_color(rgb(t.text_body))
                                    .child(styled),
                            )
                    })
                    .collect()
            },
        )
        .with_horizontal_sizing_behavior(gpui::ListHorizontalSizingBehavior::Unconstrained)
        .with_width_from_item(Some(self.doc.widest))
        .flex_1()
        .min_h(px(0.0))
        .py(px(4.0))
        .font_family(fonts::mono())
        .text_size(px(text::SM))
        .line_height(px(BODY_LINE_H))
        .track_scroll(self.scroll.clone());
        container.child(list)
    }
}
