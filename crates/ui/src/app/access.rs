//! The Access view: every permission the run needed, compared with the
//! baseline (the lock by default, or another run), and a detail pane to
//! accept permissions into the lock or mark them `# test`.

use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Div, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, MouseButton, MouseDownEvent, Pixels, Point, Render, ScrollStrategy, SharedString,
    UniformListScrollHandle, Window, div, prelude::*, px, rgb, uniform_list,
};

use super::{
    session::{Baseline, Session, SessionEvent},
    widgets,
};
use crate::{
    fonts, glyph,
    model::{Access, Trace, operation_label, service_label, status_label},
    store::{AccessDelta, AccessFilter, AccessRow, policy_json, statement_json},
    theme::{self, Ui, text},
};

const PAGE_ROWS: isize = 20;

pub enum AccessEvent {
    Selected(Option<AccessRow>),
    ContextMenu(Point<Pixels>, AccessRow),
    /// Show the calls that need this permission.
    FilterCalls(Access),
    SelectCall(String),
    Toast(String),
}

pub struct AccessView {
    session: Entity<Session>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    filter: AccessFilter,
    tokens: Vec<String>,
    rows: Arc<Vec<AccessRow>>,
    view: Arc<Vec<usize>>,
    selected: Option<Access>,
    _subscription: gpui::Subscription,
}

impl EventEmitter<AccessEvent> for AccessView {}

impl Focusable for AccessView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl AccessView {
    pub fn new(session: Entity<Session>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&session, |this, _, event, cx| match event {
            SessionEvent::Derived | SessionEvent::RunChanged => this.refresh(cx),
            _ => {}
        });
        let mut view = AccessView {
            session,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            filter: AccessFilter::All,
            tokens: Vec::new(),
            rows: Arc::new(Vec::new()),
            view: Arc::new(Vec::new()),
            selected: None,
            _subscription: subscription,
        };
        view.refresh(cx);
        view
    }

    pub fn selected(&self) -> Option<AccessRow> {
        let selected = self.selected.as_ref()?;
        self.rows
            .iter()
            .find(|row| &row.access == selected)
            .cloned()
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let rows = self.session.read(cx).access_rows.clone();
        if Arc::ptr_eq(&rows, &self.rows) {
            return;
        }
        self.rows = rows;
        self.recompute(cx);
    }

    fn recompute(&mut self, cx: &mut Context<Self>) {
        let view: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| self.filter.matches(row) && row.matches_search(&self.tokens))
            .map(|(index, _)| index)
            .collect();
        self.view = Arc::new(view);
        let still_there = self
            .selected
            .as_ref()
            .is_some_and(|sel| self.view.iter().any(|&ix| &self.rows[ix].access == sel));
        if !still_there {
            let first = self.view.first().map(|&ix| self.rows[ix].access.clone());
            self.selected = first;
        }
        cx.emit(AccessEvent::Selected(self.selected()));
        cx.notify();
    }

    pub fn set_tokens(&mut self, tokens: Vec<String>, cx: &mut Context<Self>) {
        if self.tokens != tokens {
            self.tokens = tokens;
            self.recompute(cx);
        }
    }

    fn set_filter(&mut self, filter: AccessFilter, cx: &mut Context<Self>) {
        self.filter = if self.filter == filter {
            AccessFilter::All
        } else {
            filter
        };
        self.recompute(cx);
    }

    pub fn clear_filter(&mut self, cx: &mut Context<Self>) -> bool {
        if self.filter == AccessFilter::All {
            return false;
        }
        self.filter = AccessFilter::All;
        self.recompute(cx);
        true
    }

    pub fn select(&mut self, access: Access, cx: &mut Context<Self>) {
        if !self.rows.iter().any(|row| row.access == access) {
            return;
        }
        if !self.view.iter().any(|&ix| self.rows[ix].access == access) {
            self.filter = AccessFilter::All;
            self.tokens.clear();
            self.recompute(cx);
        }
        self.selected = Some(access);
        if let Some(position) = self.position() {
            self.scroll.scroll_to_item(position, ScrollStrategy::Center);
        }
        cx.emit(AccessEvent::Selected(self.selected()));
        cx.notify();
    }

    fn position(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.view
            .iter()
            .position(|&ix| &self.rows[ix].access == selected)
    }

    pub fn move_by(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.view.is_empty() {
            return;
        }
        let last = self.view.len() as isize - 1;
        let next = match self.position() {
            Some(position) => (position as isize + delta).clamp(0, last),
            None => 0,
        } as usize;
        self.selected = Some(self.rows[self.view[next]].access.clone());
        self.scroll.scroll_to_item(next, ScrollStrategy::Top);
        cx.emit(AccessEvent::Selected(self.selected()));
        cx.notify();
    }

    pub fn page(&mut self, down: bool, cx: &mut Context<Self>) {
        self.move_by(if down { PAGE_ROWS } else { -PAGE_ROWS }, cx);
    }

    pub fn select_edge(&mut self, last: bool, cx: &mut Context<Self>) {
        let delta = if last { isize::MAX / 2 } else { isize::MIN / 2 };
        self.move_by(delta, cx);
    }

    fn render_subbar(&self, ui: &Ui, cx: &mut Context<Self>) -> Div {
        let t = ui.p;
        let count = |f: AccessFilter| self.rows.iter().filter(|row| f.matches(row)).count();
        let added = count(AccessFilter::Added);
        let removed = count(AccessFilter::Removed);
        let session = self.session.read(cx);
        let vs_lock = session.baseline == Baseline::Lock || session.baseline_run.is_none();
        let editable = session.can_edit_lock();
        let entity = cx.entity();
        let chip = |id: &'static str, label: String, n: usize, f: AccessFilter, color: u32| {
            let entity = entity.clone();
            widgets::chip(ui, id, label, Some(n), self.filter == f, color).on_click(
                move |_, _, cx| {
                    entity.update(cx, |view, cx| view.set_filter(f, cx));
                },
            )
        };
        let mut bar = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .h(px(theme::SUBBAR_H))
            .px(px(theme::ROW_PAD_X - 4.0))
            .border_b_1()
            .border_color(rgb(t.border_subtle))
            .bg(rgb(t.toolbar))
            .child(chip(
                "a-all",
                ui.t("All", "全部").into(),
                self.rows.len(),
                AccessFilter::All,
                t.accent,
            ))
            .child(chip(
                "a-added",
                if vs_lock {
                    ui.t("+ Not in lock", "+ 未入锁").into()
                } else {
                    ui.t("+ Added", "+ 新增").into()
                },
                added,
                AccessFilter::Added,
                t.amber,
            ))
            .child(chip(
                "a-removed",
                if vs_lock {
                    ui.t("− Unused", "− 未使用").into()
                } else {
                    ui.t("− Removed", "− 移除").into()
                },
                removed,
                AccessFilter::Removed,
                t.text_muted,
            ))
            .child(chip(
                "a-implied",
                ui.t("Implied", "隐含").into(),
                count(AccessFilter::Implied),
                AccessFilter::Implied,
                t.accent,
            ))
            .child(chip(
                "a-test",
                "# test".into(),
                count(AccessFilter::Test),
                AccessFilter::Test,
                t.text_muted,
            ))
            .child(div().flex_1());
        let not_in_lock: Vec<(Access, bool)> = self
            .rows
            .iter()
            .filter(|row| row.used && !row.in_lock)
            .map(|row| (row.access.clone(), false))
            .collect();
        if editable && !not_in_lock.is_empty() {
            let session = self.session.clone();
            let n = not_in_lock.len();
            bar = bar.child(
                widgets::primary_button(
                    ui,
                    "accept-all",
                    if ui.zh() {
                        format!("全部写入 lock（{n}）")
                    } else {
                        format!("Accept {n} into lock")
                    },
                )
                .tooltip(widgets::tip(
                    ui.t(
                        "Add every permission this run used that the lock lacks",
                        "将本次运行使用但 lock 中缺少的权限全部写入",
                    ),
                    None,
                ))
                .on_click(move |_, _, cx| {
                    let entries = not_in_lock.clone();
                    session.update(cx, |session, cx| session.accept(entries, cx));
                }),
            );
        }
        let rows = self.rows.clone();
        bar.child(
            widgets::button(ui, "copy-policy", ui.t("Copy policy", "复制策略"))
                .tooltip(widgets::tip(
                    ui.t(
                        "IAM policy JSON for what this run used (without # test)",
                        "本次运行所用权限的 IAM 策略 JSON（不含 # test）",
                    ),
                    Some(crate::i18n::shortcut("⇧E")),
                ))
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(policy_json(&rows)));
                    cx.emit(AccessEvent::Toast("Copied policy JSON".into()));
                })),
        )
    }

    fn render_head(&self, ui: &Ui) -> Div {
        let t = ui.p;
        let head = |label: &'static str| {
            div()
                .text_size(px(text::XS))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(t.text_muted))
                .child(label)
        };
        div()
            .flex()
            .items_center()
            .gap(px(theme::COL_GAP))
            .h(px(theme::TABLE_HEAD_H))
            .px(px(theme::ROW_PAD_X))
            .bg(rgb(t.table_head))
            .border_b_1()
            .border_color(rgb(t.border_subtle))
            .child(div().w(px(14.0)))
            .child(head("ACTION").w(px(220.0)).flex_shrink_0())
            .child(head("RESOURCE").flex_1().min_w(px(0.0)))
            .child(head("CALLS").w(px(56.0)).flex_shrink_0().text_right())
    }
}

impl Render for AccessView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scope = crate::fsguard::RenderScope::enter();
        let ui = Ui::of(cx);
        let t = ui.p;
        let body: AnyElement = if self.view.is_empty() {
            let health_ready = self.session.read(cx).health == super::session::Health::Ready;
            widgets::empty_state(
                &ui,
                if self.rows.is_empty() {
                    ui.t("No permissions yet", "还没有权限记录")
                } else {
                    ui.t("Nothing matches", "没有匹配项")
                },
                if self.rows.is_empty() && health_ready {
                    ui.t(
                        "Permissions appear as calls that need them arrive.",
                        "当需要权限的调用到达时，这里会列出所需权限。",
                    )
                } else {
                    ui.t("Press Esc to clear the filter.", "按 Esc 清除筛选。")
                },
                None,
            )
            .into_any_element()
        } else {
            let rows = self.rows.clone();
            let view = self.view.clone();
            let selected = self.selected.clone();
            let entity = cx.entity().downgrade();
            uniform_list("access", view.len(), move |range, _window, cx| {
                let _scope = crate::fsguard::RenderScope::enter();
                let ui = Ui::of(cx);
                range
                    .map(|position| {
                        let row = &rows[view[position]];
                        access_row(
                            &ui,
                            row,
                            position,
                            selected.as_ref() == Some(&row.access),
                            entity.clone(),
                        )
                    })
                    .collect()
            })
            .flex_1()
            .min_h(px(0.0))
            .track_scroll(self.scroll.clone())
            .into_any_element()
        };
        div()
            .id("access-list")
            .size_full()
            .key_context("Access")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .bg(rgb(t.panel))
            .child(self.render_subbar(&ui, cx))
            .child(self.render_head(&ui))
            .child(body)
    }
}

fn access_row(
    ui: &Ui,
    row: &AccessRow,
    position: usize,
    selected: bool,
    view: gpui::WeakEntity<AccessView>,
) -> gpui::Stateful<Div> {
    let t = ui.p;
    let (glyph, color) = match row.delta {
        AccessDelta::Added => ("+", t.amber),
        AccessDelta::Removed => ("−", t.text_muted),
        AccessDelta::Same => ("", t.text_muted),
    };
    let service = row.access.action.split(':').next().unwrap_or("");
    let service_color = ui.service_color(&service_label(service));
    let access = row.access.clone();
    let menu_row = row.clone();
    let click_view = view.clone();
    let mut flags = div().flex().items_center().gap(px(4.0)).flex_shrink_0();
    if row.implied {
        flags = flags.child(widgets::badge(ui, "IMPLIED", t.text_muted));
    }
    if row.test {
        flags = flags.child(widgets::badge(ui, "# test", t.text_muted));
    }
    if row.errors > 0 {
        flags = flags.child(widgets::badge(ui, format!("{} ERR", row.errors), t.red));
    }
    div()
        .id(ElementId::named_usize("access-row", position))
        .w_full()
        .flex()
        .items_center()
        .gap(px(theme::COL_GAP))
        .h(px(32.0))
        .px(px(theme::ROW_PAD_X))
        .border_b_1()
        .border_color(rgb(t.border_subtle))
        .bg(if selected {
            rgb(t.row_selected)
        } else {
            rgb(t.row)
        })
        .hover(move |s| {
            s.bg(if selected {
                rgb(t.row_selected_hover)
            } else {
                rgb(t.row_hover)
            })
        })
        .when(!row.used, |el| el.opacity(0.65))
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            let access = access.clone();
            click_view
                .update(cx, |view, cx| {
                    window.focus(&view.focus);
                    view.selected = Some(access);
                    cx.emit(AccessEvent::Selected(view.selected()));
                    cx.notify();
                })
                .ok();
        })
        .on_mouse_down(
            MouseButton::Right,
            move |event: &MouseDownEvent, window, cx| {
                let row = menu_row.clone();
                view.update(cx, |view, cx| {
                    window.focus(&view.focus);
                    view.selected = Some(row.access.clone());
                    cx.emit(AccessEvent::Selected(Some(row.clone())));
                    cx.emit(AccessEvent::ContextMenu(event.position, row));
                    cx.notify();
                })
                .ok();
            },
        )
        .child(
            div()
                .w(px(14.0))
                .flex_shrink_0()
                .font_family(fonts::mono())
                .text_size(px(text::MD))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(color))
                .child(glyph),
        )
        .child(
            div()
                .w(px(220.0))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(6.0))
                .overflow_hidden()
                .child(glyph::dot(service_color, 6.0))
                .child(
                    div()
                        .min_w(px(0.0))
                        .font_family(fonts::mono())
                        .text_size(px(text::SM))
                        .text_color(rgb(t.text))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(row.access.action.clone()),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .font_family(fonts::mono())
                        .text_size(px(text::SM))
                        .text_color(rgb(t.text_dim))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(row.access.resource.clone()),
                )
                .child(flags),
        )
        .child(
            div()
                .w(px(56.0))
                .flex_shrink_0()
                .text_right()
                .font_family(fonts::mono())
                .text_size(px(text::SM))
                .text_color(rgb(t.text_muted))
                .child(if row.used {
                    row.calls.to_string()
                } else {
                    "—".into()
                }),
        )
}

/// The right-hand pane of the Access view.
pub struct AccessDetail {
    session: Entity<Session>,
    row: Option<AccessRow>,
    calls: Vec<Arc<Trace>>,
}

impl EventEmitter<AccessEvent> for AccessDetail {}

impl AccessDetail {
    pub fn new(session: Entity<Session>) -> Self {
        AccessDetail {
            session,
            row: None,
            calls: Vec::new(),
        }
    }

    pub fn set_row(&mut self, row: Option<AccessRow>, cx: &mut Context<Self>) {
        if self.row == row {
            return;
        }
        self.calls = match row.as_ref() {
            Some(row) => self
                .session
                .read(cx)
                .store
                .rows()
                .iter()
                .filter(|call| !call.pending && call.uses(&row.access))
                .take(40)
                .cloned()
                .collect(),
            None => Vec::new(),
        };
        self.row = row;
        cx.notify();
    }
}

impl Render for AccessDetail {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scope = crate::fsguard::RenderScope::enter();
        let ui = Ui::of(cx);
        let t = ui.p;
        let container = div()
            .id("access-detail")
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(t.panel_raised));
        let Some(row) = self.row.clone() else {
            return container.child(widgets::empty_state(
                &ui,
                ui.t("Select a permission", "选择一项权限"),
                "",
                None,
            ));
        };
        let session = self.session.read(cx);
        let editable = session.can_edit_lock();
        let (service, action) = row
            .access
            .action
            .split_once(':')
            .unwrap_or(("", row.access.action.as_str()));
        let service_color = ui.service_color(&service_label(service));

        let mut badges = div().flex().items_center().gap(px(6.0));
        badges = badges.child(widgets::badge(
            ui_ref(&ui),
            service_label(service),
            color_of(service_color),
        ));
        badges = badges.child(match (row.delta, row.in_lock) {
            (AccessDelta::Added, false) => widgets::badge(&ui, "NOT IN LOCK", t.amber),
            (AccessDelta::Added, true) => widgets::badge(&ui, "NEW VS BASELINE", t.amber),
            (AccessDelta::Removed, _) => widgets::badge(&ui, "UNUSED", t.text_muted),
            (AccessDelta::Same, true) => widgets::badge(&ui, "IN LOCK", t.green),
            (AccessDelta::Same, false) => widgets::badge(&ui, "ALLOWED", t.green),
        });
        if row.implied {
            badges = badges.child(widgets::badge(&ui, "IMPLIED", t.accent));
        }
        if row.test {
            badges = badges.child(widgets::badge(&ui, "# test", t.text_muted));
        }

        let mut actions = div().flex().flex_wrap().items_center().gap(px(8.0));
        if editable && row.used && !row.in_lock {
            let session = self.session.clone();
            let access = row.access.clone();
            actions = actions.child(
                widgets::primary_button(&ui, "accept", ui.t("Accept into lock", "写入 lock"))
                    .on_click(move |_, _, cx| {
                        let entries = vec![(access.clone(), false)];
                        session.update(cx, |session, cx| session.accept(entries, cx));
                    }),
            );
        }
        if editable && (row.in_lock || row.used) {
            let session = self.session.clone();
            let access = row.access.clone();
            let test = !row.test;
            actions = actions.child(
                widgets::button(
                    &ui,
                    "mark-test",
                    if row.test {
                        ui.t("Unmark # test", "取消 # test")
                    } else {
                        ui.t("Mark # test", "标记 # test")
                    },
                )
                .tooltip(widgets::tip(
                    ui.t(
                        "Test-only permissions stay in the lock but out of `lenslet policy`",
                        "仅测试需要的权限保留在 lock 中，但不进入 `lenslet policy`",
                    ),
                    None,
                ))
                .on_click(move |_, _, cx| {
                    let access = access.clone();
                    session.update(cx, |session, cx| session.set_test_mark(access, test, cx));
                }),
            );
        }
        let statement = statement_json(&row.access);
        actions = actions.child(
            widgets::button(
                &ui,
                "copy-statement",
                ui.t("Copy statement", "复制 Statement"),
            )
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(statement.clone()));
                cx.emit(AccessEvent::Toast("Copied statement".into()));
            })),
        );
        if row.used {
            let access = row.access.clone();
            actions = actions.child(
                widgets::button(&ui, "show-calls", ui.t("Show calls", "查看调用")).on_click(
                    cx.listener(move |_, _, _, cx| {
                        cx.emit(AccessEvent::FilterCalls(access.clone()));
                    }),
                ),
            );
        }

        let mut calls = div().flex().flex_col();
        for (index, call) in self.calls.iter().enumerate() {
            let id = call.id.clone();
            calls = calls.child(
                div()
                    .id(ElementId::named_usize("access-call", index))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .h(px(28.0))
                    .px(px(8.0))
                    .rounded(px(5.0))
                    .cursor_pointer()
                    .hover(move |s| s.bg(rgb(t.row_hover)))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(AccessEvent::SelectCall(id.clone()));
                    }))
                    .child(
                        div()
                            .w(px(40.0))
                            .font_family(fonts::mono())
                            .text_size(px(text::SM))
                            .text_color(ui.status_color(call.status))
                            .child(status_label(call.status)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_size(px(text::MD))
                            .text_color(rgb(t.text_soft))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(format!("{} {}", call.service_name(), operation_label(call))),
                    ),
            );
        }
        if self.calls.is_empty() {
            calls = calls.child(
                div()
                    .text_size(px(text::MD))
                    .text_color(rgb(t.text_muted))
                    .child(if row.used {
                        ui.t(
                            "The calls that needed it are no longer in view.",
                            "相关调用已不在当前列表中。",
                        )
                    } else {
                        ui.t("No call in this run needed it.", "本次运行没有调用需要它。")
                    }),
            );
        }

        let via: SharedString = row.via.join(", ").into();
        container.child(
            div()
                .id("access-detail-scroll")
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .overflow_y_scroll()
                .gap(px(16.0))
                .p(px(16.0))
                .child(badges)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(
                            div()
                                .font_family(fonts::mono())
                                .text_size(px(text::XL))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(t.text))
                                .child(action.to_string()),
                        )
                        .child(
                            div()
                                .font_family(fonts::mono())
                                .text_size(px(text::MD))
                                .line_height(px(18.0))
                                .text_color(rgb(t.text_dim))
                                .child(row.access.resource.clone()),
                        ),
                )
                .child(actions)
                .when(!via.is_empty(), |el| {
                    el.child(widgets::meta_row(
                        &ui,
                        ui.t("Needed by", "调用方"),
                        widgets::mono(&ui, via.clone()),
                    ))
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(widgets::label(&ui, ui.t("CALLS", "调用")))
                        .child(calls),
                ),
        )
    }
}

fn ui_ref(ui: &Ui) -> &Ui {
    ui
}

fn color_of(color: gpui::Rgba) -> u32 {
    ((color.r * 255.0).round() as u32) << 16
        | ((color.g * 255.0).round() as u32) << 8
        | (color.b * 255.0).round() as u32
}
