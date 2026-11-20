//! A line-virtualized, character-selectable, JSON-foldable text viewer for
//! payloads, headers, diffs, and IaC policies: one `uniform_list` row per
//! visible display line with its gutter, so a body of any size renders only
//! what is on screen.
//!
//! - Click & drag selects exact character spans; double-click selects a token
//!   (e.g. an ARN or JSON value); triple-click selects a full line; ⌘A selects
//!   the whole document; ⌘C copies the selection (or the whole document when no
//!   range is selected).
//! - Clicking `▾` / `▸` in the gutter folds or unfolds multi-line JSON objects
//!   and arrays.
//! - `step_match` (⌘G / ⇧⌘G) jumps through search matches, unfolding if needed.

use std::{cell::Cell, collections::HashSet, ops::Range, rc::Rc, sync::Arc};

use gpui::{
    App, ClipboardItem, Context, ElementId, EventEmitter, FocusHandle, Focusable, HighlightStyle,
    MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point, Render, ScrollStrategy,
    SharedString, StyledText, TextRun, UniformListScrollHandle, Window, canvas, div, prelude::*,
    px, rgb, uniform_list,
};

use crate::{
    body::BodyDoc,
    fonts,
    theme::{self, BODY_LINE_H, Ui, text},
};

pub enum BodyViewEvent {
    Copied(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPos {
    pub line: usize,
    pub col: usize,
}

pub struct BodyView {
    id: SharedString,
    doc: Arc<BodyDoc>,
    /// `(anchor, head)` character positions in `doc`.
    selection: Option<(TextPos, TextPos)>,
    folded: HashSet<usize>,
    visible_lines: Arc<Vec<usize>>,
    active_match: Option<usize>,
    origin_x: Rc<Cell<Pixels>>,
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
            folded: HashSet::new(),
            visible_lines: Arc::new(Vec::new()),
            active_match: None,
            origin_x: Rc::new(Cell::new(px(0.0))),
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
                self.folded.clear();
                self.scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
            }
            self.active_match = (!self.doc.match_lines.is_empty()).then_some(0);
            self.rebuild_visible();
        }
        self.loading = false;
        cx.notify();
    }

    fn rebuild_visible(&mut self) {
        let total = self.doc.lines.len();
        if self.folded.is_empty() {
            self.visible_lines = Arc::new((0..total).collect());
            return;
        }
        let mut vis = Vec::with_capacity(total);
        let mut line = 0usize;
        while line < total {
            vis.push(line);
            if self.folded.contains(&line)
                && let Some(&close) = self.doc.fold_ranges.get(&line)
                && close > line
            {
                line = close + 1;
                continue;
            }
            line += 1;
        }
        self.visible_lines = Arc::new(vis);
    }

    pub fn toggle_fold(&mut self, line: usize, cx: &mut Context<Self>) {
        if !self.doc.fold_ranges.contains_key(&line) {
            return;
        }
        if !self.folded.remove(&line) {
            self.folded.insert(line);
        }
        self.rebuild_visible();
        cx.notify();
    }

    fn ensure_line_visible(&mut self, target: usize) {
        if self.folded.is_empty() {
            return;
        }
        let mut changed = false;
        let to_open: Vec<usize> = self
            .folded
            .iter()
            .copied()
            .filter(|&open| {
                self.doc
                    .fold_ranges
                    .get(&open)
                    .is_some_and(|&close| target > open && target <= close)
            })
            .collect();
        for open in to_open {
            self.folded.remove(&open);
            changed = true;
        }
        if changed {
            self.rebuild_visible();
        }
    }

    /// `(1-based current, total)` match indicator for the toolbar.
    pub fn match_status(&self) -> Option<(usize, usize)> {
        let count = self.doc.match_lines.len();
        (count > 0).then(|| (self.active_match.unwrap_or(0).min(count - 1) + 1, count))
    }

    /// Jump forward (`+1`) or backward (`-1`) across search match lines.
    pub fn step_match(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.doc.match_lines.len();
        if count == 0 {
            return;
        }
        let cur = self.active_match.unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(count as isize) as usize;
        self.active_match = Some(next);
        let target_line = self.doc.match_lines[next];
        self.ensure_line_visible(target_line);
        if let Ok(vis_ix) = self.visible_lines.binary_search(&target_line) {
            self.scroll.scroll_to_item(vis_ix, ScrollStrategy::Center);
        }
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

    /// Copy the selected character range, or everything when nothing (or a single caret) is selected.
    pub fn copy(&mut self, cx: &mut Context<Self>) {
        if self.doc.is_empty() {
            return;
        }
        let text = match self.selection {
            Some((a, b)) if a != b => {
                let (s, e) = (a.min(b), a.max(b));
                self.doc.copy_selection(s.line, s.col, e.line, e.col)
            }
            _ => {
                let last = self.doc.lines.len().saturating_sub(1);
                self.doc.copy_lines(0, last)
            }
        };
        let bytes = text.len();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        cx.emit(BodyViewEvent::Copied(bytes));
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        if !self.doc.lines.is_empty() {
            let last = self.doc.lines.len() - 1;
            let end_col = self.doc.line_text(last).len();
            self.selection = Some((
                TextPos { line: 0, col: 0 },
                TextPos {
                    line: last,
                    col: end_col,
                },
            ));
            cx.notify();
        }
    }

    fn gutter_width(&self) -> f32 {
        let digits = self.doc.lines.len().to_string().len().max(2);
        let fold_col = if self.doc.fold_ranges.is_empty() {
            0.0
        } else {
            14.0
        };
        16.0 + digits as f32 * 7.0 + fold_col
    }

    fn column_at(&self, line_ix: usize, pos: Point<Pixels>, window: &Window) -> usize {
        let text_str = self.doc.line_text(line_ix);
        if text_str.is_empty() {
            return 0;
        }
        let h_scroll = -self.scroll.0.borrow().base_handle.offset().x;
        let rel_x =
            (pos.x - self.origin_x.get() + h_scroll - px(self.gutter_width() + 8.0)).max(px(0.0));
        let run = [TextRun {
            len: text_str.len(),
            font: fonts::mono_font(),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }];
        let shaped = window.text_system().shape_line(
            SharedString::from(text_str.to_string()),
            px(text::SM),
            &run,
            None,
        );
        shaped.closest_index_for_x(rel_x).min(text_str.len())
    }

    fn press(
        &mut self,
        line: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        let col = self.column_at(line, event.position, window);
        let line_len = self.doc.line_text(line).len();
        if event.click_count >= 3 {
            self.selection = Some((
                TextPos { line, col: 0 },
                TextPos {
                    line,
                    col: line_len,
                },
            ));
            self.dragging = false;
        } else if event.click_count == 2 {
            let (s, e) = token_bounds_at(self.doc.line_text(line), col);
            self.selection = Some((TextPos { line, col: s }, TextPos { line, col: e }));
            self.dragging = false;
        } else {
            let pt = TextPos { line, col };
            self.selection = Some(match (event.modifiers.shift, self.selection) {
                (true, Some((anchor, _))) => (anchor, pt),
                _ => (pt, pt),
            });
            self.dragging = true;
        }
        cx.notify();
    }

    fn drag_to(
        &mut self,
        line: usize,
        pos: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.dragging
            && let Some((anchor, head)) = self.selection
        {
            let col = self.column_at(line, pos, window);
            let next = TextPos { line, col };
            if head != next {
                self.selection = Some((anchor, next));
                cx.notify();
            }
        }
    }
}

fn token_bounds_at(line: &str, col: usize) -> (usize, usize) {
    if line.is_empty() {
        return (0, 0);
    }
    let mut pivot = col.min(line.len());
    while pivot > 0 && !line.is_char_boundary(pivot) {
        pivot -= 1;
    }
    let is_token_char = |ch: char| {
        ch.is_alphanumeric() || matches!(ch, '_' | '-' | ':' | '/' | '.' | '*' | '$' | '@')
    };
    let mut start = pivot;
    for (idx, ch) in line[..pivot].char_indices().rev() {
        if is_token_char(ch) {
            start = idx;
        } else {
            break;
        }
    }
    let mut end = pivot;
    for (rel, ch) in line[pivot..].char_indices() {
        if is_token_char(ch) {
            end = pivot + rel + ch.len_utf8();
        } else {
            break;
        }
    }
    if start == end {
        let next = line[pivot..]
            .chars()
            .next()
            .map_or(pivot, |c| pivot + c.len_utf8());
        (pivot, next)
    } else {
        (start, end)
    }
}

fn overlay_char_selection(
    base: &[(Range<usize>, HighlightStyle)],
    sel: Range<usize>,
    line_len: usize,
    bg_hex: u32,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let sel_start = sel.start.min(line_len);
    let sel_end = sel.end.min(line_len);
    if sel_start >= sel_end {
        return base.to_vec();
    }
    let sel_bg = Some(theme::h(bg_hex, 0.30));
    let mut points = vec![0, sel_start, sel_end, line_len];
    for (r, _) in base {
        points.push(r.start.min(line_len));
        points.push(r.end.min(line_len));
    }
    points.sort_unstable();
    points.dedup();

    let mut out = Vec::with_capacity(points.len());
    for pair in points.windows(2) {
        let (s, e) = (pair[0], pair[1]);
        if s >= e {
            continue;
        }
        let base_style = base
            .iter()
            .find(|(r, _)| r.start <= s && r.end >= e)
            .map(|(_, st)| *st);
        let in_sel = s >= sel_start && e <= sel_end;
        match (base_style, in_sel) {
            (Some(mut st), true) => {
                st.background_color = sel_bg;
                out.push((s..e, st));
            }
            (None, true) => {
                out.push((
                    s..e,
                    HighlightStyle {
                        background_color: sel_bg,
                        ..Default::default()
                    },
                ));
            }
            (Some(st), false) => out.push((s..e, st)),
            (None, false) => {}
        }
    }
    out
}

impl Render for BodyView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scope = crate::fsguard::RenderScope::enter();
        let ui = Ui::of(cx);
        let t = ui.p;
        let doc = self.doc.clone();
        let origin_cell = self.origin_x.clone();
        let probe = canvas(
            move |bounds, _, _| origin_cell.set(bounds.left()),
            |_, _, _, _| {},
        )
        .w_full()
        .h(px(0.0));

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
            )
            .child(probe);

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

        let gutter_w = self.gutter_width();
        let has_folds = !doc.fold_ranges.is_empty();
        let selection = self
            .selection
            .filter(|(a, b)| a != b)
            .map(|(a, b)| (a.min(b), a.max(b)));
        let active_match_line = self
            .active_match
            .and_then(|ix| doc.match_lines.get(ix).copied());
        let visible = self.visible_lines.clone();
        let widest_vis = visible.binary_search(&self.doc.widest).ok();
        let folded_snapshot = self.folded.clone();
        let entity = cx.entity().downgrade();
        let list = uniform_list(
            ElementId::Name(format!("{}-lines", self.id).into()),
            visible.len(),
            move |range, _window, cx| {
                let _scope = crate::fsguard::RenderScope::enter();
                let ui = Ui::of(cx);
                let t = ui.p;
                range
                    .map(|vis_pos| {
                        let index = visible[vis_pos];
                        let number = doc.numbers[index];
                        let line = doc.line_text(index);
                        let is_foldable = doc.fold_ranges.contains_key(&index);
                        let is_folded = folded_snapshot.contains(&index);
                        let hidden_count = is_folded
                            .then(|| doc.fold_ranges.get(&index).copied())
                            .flatten()
                            .map(|close| close.saturating_sub(index));

                        let line_sel: Option<Range<usize>> = selection.and_then(|(s, e)| {
                            if index < s.line || index > e.line {
                                None
                            } else {
                                let cs = if index == s.line { s.col } else { 0 };
                                let ce = if index == e.line { e.col } else { line.len() };
                                Some(cs.min(line.len())..ce.min(line.len()))
                            }
                        });
                        let full_row_selected = line_sel.as_ref().is_some_and(|r| {
                            r.start == 0 && r.end == line.len() && !line.is_empty()
                        });
                        let spans = match line_sel {
                            Some(sel_r) => overlay_char_selection(
                                &doc.spans[index],
                                sel_r,
                                line.len(),
                                t.accent,
                            ),
                            None => doc.spans[index].clone(),
                        };
                        let styled = StyledText::new(SharedString::from(line.to_string()))
                            .with_highlights(spans);

                        let down = entity.clone();
                        let moved = entity.clone();
                        let fold_click = entity.clone();
                        let is_active_match = active_match_line == Some(index);

                        let mut gutter = div()
                            .id(ElementId::named_usize("gutter", index))
                            .w(px(gutter_w))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap(px(4.0))
                            .pr(px(6.0))
                            .text_color(rgb(if is_active_match {
                                t.accent
                            } else {
                                t.text_faint
                            }))
                            .border_r_1()
                            .border_color(rgb(t.border_subtle))
                            .child(if doc.continued[index] {
                                "↪".to_string()
                            } else {
                                number.to_string()
                            });
                        if has_folds {
                            gutter = gutter.child(
                                div()
                                    .w(px(10.0))
                                    .text_center()
                                    .text_size(px(10.0))
                                    .text_color(rgb(t.text_muted))
                                    .child(if is_folded {
                                        "▸"
                                    } else if is_foldable {
                                        "▾"
                                    } else {
                                        ""
                                    }),
                            );
                        }
                        if is_foldable {
                            gutter = gutter.cursor_pointer().on_mouse_down(
                                MouseButton::Left,
                                move |_, _, cx| {
                                    cx.stop_propagation();
                                    fold_click
                                        .update(cx, |view, cx| view.toggle_fold(index, cx))
                                        .ok();
                                },
                            );
                        }

                        div()
                            .id(vis_pos)
                            .flex()
                            .flex_row()
                            .items_center()
                            .h(px(BODY_LINE_H))
                            .w_full()
                            .when(full_row_selected, |el| el.bg(theme::h(t.accent, 0.14)))
                            .when(!full_row_selected && is_active_match, |el| {
                                el.bg(theme::h(t.accent, 0.08))
                            })
                            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                                down.update(cx, |view, cx| view.press(index, event, window, cx))
                                    .ok();
                            })
                            .on_mouse_move(move |event: &MouseMoveEvent, window, cx| {
                                if event.pressed_button == Some(MouseButton::Left) {
                                    moved
                                        .update(cx, |view, cx| {
                                            view.drag_to(index, event.position, window, cx)
                                        })
                                        .ok();
                                }
                            })
                            .child(gutter)
                            .child(
                                div()
                                    .pl(px(8.0))
                                    .pr(px(16.0))
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .whitespace_nowrap()
                                    .text_color(rgb(t.text_body))
                                    .child(styled)
                                    .when_some(hidden_count, |el, count| {
                                        el.child(
                                            div()
                                                .px(px(5.0))
                                                .rounded(px(4.0))
                                                .bg(theme::h(t.accent, 0.14))
                                                .text_size(px(10.0))
                                                .text_color(rgb(t.accent))
                                                .cursor_pointer()
                                                .child(format!("… {count} lines")),
                                        )
                                    }),
                            )
                    })
                    .collect()
            },
        )
        .with_horizontal_sizing_behavior(gpui::ListHorizontalSizingBehavior::Unconstrained)
        .with_width_from_item(widest_vis)
        .flex_1()
        .min_h(px(0.0))
        .py(px(4.0))
        .font(fonts::mono_font())
        .text_size(px(text::SM))
        .line_height(px(BODY_LINE_H))
        .track_scroll(self.scroll.clone());
        container.child(list)
    }
}
