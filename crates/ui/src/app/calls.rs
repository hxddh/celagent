//! The Calls list: filter chips, findings, the run mini-map and a virtualized
//! table. Its view (filtered row indices) is recomputed when inputs change —
//! incrementally for appends, on the background executor for large searches —
//! never in render.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use gpui::{
    AnyElement, App, Context, Div, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point, Render, ScrollStrategy,
    SharedString, Stateful, Task, UniformListScrollHandle, Window, div, prelude::*, px, rgb,
    uniform_list,
};
use lenslet_core::findings::{Finding, Severity};

use super::{
    session::{Health, Session, SessionEvent},
    widgets,
};
use crate::{
    fonts, glyph, i18n,
    model::{
        CallFilter, ServiceFacet, StatusFilter, Trace, display_path, ellipsize, format_latency,
        format_offset, operation_label, search_tokens, short_clock, status_label,
    },
    store::{BACKGROUND_SEARCH_ROWS, Stats, ViewQuery, filter_rows},
    theme::{self, Ui, text},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortCol {
    #[default]
    Time,
    Service,
    Operation,
    Status,
    Duration,
}

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(60);
const PAGE_ROWS: isize = 20;
const START_COMMAND: &str = "lenslet run -- npm test";

pub enum CallsEvent {
    /// The selected call changed (or was replaced by its finished line).
    Selected(Option<Arc<Trace>>),
    /// Right-click on a row.
    ContextMenu(Point<Pixels>, Arc<Trace>),
    Toast(String),
}

pub struct CallsList {
    session: Entity<Session>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    pub filter: CallFilter,
    query: String,
    tokens: Vec<String>,
    rows: Arc<Vec<Arc<Trace>>>,
    view: Arc<Vec<u32>>,
    changed: usize,
    stats: Stats,
    facets: Vec<ServiceFacet>,
    selected: Option<String>,
    /// The user picked a row; stop following the newest call.
    pinned: bool,
    findings_open: bool,
    sort_col: SortCol,
    sort_asc: bool,
    group_retries: bool,
    attempt_counts: Arc<HashMap<String, usize>>,
    timeline_drag_anchor: Option<usize>,
    view_generation: u64,
    _search_task: Option<Task<()>>,
    _subscription: gpui::Subscription,
}

impl EventEmitter<CallsEvent> for CallsList {}

impl Focusable for CallsList {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl CallsList {
    pub fn new(session: Entity<Session>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&session, |this, _, event, cx| match event {
            SessionEvent::RunChanged => this.on_run_changed(cx),
            SessionEvent::Rows(delta) => this.on_rows(delta.clone(), cx),
            SessionEvent::Derived => {
                let changes_matter = this.filter.status == StatusFilter::Changed;
                this.recompute(cx);
                if !changes_matter {
                    cx.notify();
                }
            }
            SessionEvent::Runs | SessionEvent::Toast(_) => {}
        });
        let mut list = CallsList {
            session,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            filter: CallFilter::default(),
            query: String::new(),
            tokens: Vec::new(),
            rows: Arc::new(Vec::new()),
            view: Arc::new(Vec::new()),
            changed: 0,
            stats: Stats::default(),
            facets: Vec::new(),
            selected: None,
            pinned: false,
            findings_open: true,
            sort_col: SortCol::Time,
            sort_asc: true,
            group_retries: false,
            attempt_counts: Arc::new(HashMap::new()),
            timeline_drag_anchor: None,
            view_generation: 0,
            _search_task: None,
            _subscription: subscription,
        };
        list.on_run_changed(cx);
        list
    }

    pub fn selected(&self) -> Option<Arc<Trace>> {
        let id = self.selected.as_deref()?;
        self.rows.iter().rev().find(|row| row.id == id).cloned()
    }

    fn on_run_changed(&mut self, cx: &mut Context<Self>) {
        self.filter = CallFilter::default();
        if self.selected.take().is_some() {
            cx.emit(CallsEvent::Selected(None));
        }
        self.pinned = self.session.read(cx).demo;
        self.recompute(cx);
        if self.pinned {
            // Demo: open on the first failure, which tells the story.
            let first_error = self
                .view
                .iter()
                .map(|&ix| &self.rows[ix as usize])
                .find(|row| row.is_error())
                .map(|row| row.id.clone());
            if let Some(id) = first_error {
                self.select_id(Some(id), true, cx);
            }
        }
    }

    fn snapshot(&mut self, cx: &App) {
        let session = self.session.read(cx);
        self.rows = session.store.rows().clone();
        self.stats = session.store.stats();
        self.facets = session.store.facets();
        if let Some(service) = self.filter.service.as_deref()
            && !self.facets.iter().any(|facet| facet.id == service)
        {
            self.filter.service = None;
        }
    }

    fn query(&self, cx: &App) -> ViewQuery {
        let session = self.session.read(cx);
        ViewQuery {
            filter: self.filter.clone(),
            tokens: self.tokens.clone(),
            run_start_ms: session.run_start_ms(),
            changes: session.changes.clone(),
        }
    }

    fn on_rows(&mut self, delta: crate::store::Delta, cx: &mut Context<Self>) {
        let follow = !self.pinned && self.near_bottom();
        let custom_view = self.group_retries || self.sort_col != SortCol::Time || !self.sort_asc;
        match delta.appended_from {
            Some(from) if !delta.reindexed && !custom_view => {
                self.snapshot(cx);
                let query = self.query(cx);
                let mut view = (*self.view).clone();
                let mut changed = self.changed;
                for (index, row) in self.rows.iter().enumerate().skip(from) {
                    if query.changes.is_changed(row) {
                        changed += 1;
                    }
                    if query.matches(row) {
                        view.push(index as u32);
                    }
                }
                // A replaced row (pending → finished) may now match differently.
                let replaced_mismatch = delta.replaced.iter().any(|&ix| {
                    query.matches(&self.rows[ix]) != view.binary_search(&(ix as u32)).is_ok()
                });
                if replaced_mismatch || !delta.replaced.is_empty() && self.filter.is_active() {
                    self.recompute(cx);
                } else {
                    self.changed = changed;
                    self.view = Arc::new(view);
                    self.after_view_change(follow, cx);
                }
            }
            _ => {
                self.recompute(cx);
            }
        }
        // The selected call finished: show its final line.
        if let Some(id) = self.selected.clone()
            && let Some(position) = self.rows.iter().position(|row| row.id == id)
            && delta.replaced.contains(&position)
        {
            cx.emit(CallsEvent::Selected(Some(self.rows[position].clone())));
        }
    }

    /// Recompute the view from scratch: synchronously when cheap, on the
    /// background executor for a search over many rows.
    pub fn recompute(&mut self, cx: &mut Context<Self>) {
        self.snapshot(cx);
        self.view_generation += 1;
        let generation = self.view_generation;
        let query = self.query(cx);
        let rows = self.rows.clone();
        let sort_col = self.sort_col;
        let sort_asc = self.sort_asc;
        let group_retries = self.group_retries;
        let follow = !self.pinned && self.near_bottom();
        if query.tokens.is_empty() && rows.len() <= BACKGROUND_SEARCH_ROWS {
            let (view, changed, counts) =
                compute_view(&rows, &query, sort_col, sort_asc, group_retries);
            self.view = Arc::new(view);
            self.changed = changed;
            self.attempt_counts = Arc::new(counts);
            self.after_view_change(follow, cx);
            return;
        }
        if rows.len() <= BACKGROUND_SEARCH_ROWS {
            let (view, changed, counts) =
                compute_view(&rows, &query, sort_col, sort_asc, group_retries);
            self.view = Arc::new(view);
            self.changed = changed;
            self.attempt_counts = Arc::new(counts);
            self.after_view_change(follow, cx);
            return;
        }
        self._search_task = Some(cx.spawn(async move |this, cx| {
            let (view, changed, counts) = cx
                .background_executor()
                .spawn(
                    async move { compute_view(&rows, &query, sort_col, sort_asc, group_retries) },
                )
                .await;
            this.update(cx, |list, cx| {
                if list.view_generation == generation {
                    list.view = Arc::new(view);
                    list.changed = changed;
                    list.attempt_counts = Arc::new(counts);
                    list.after_view_change(follow, cx);
                }
            })
            .ok();
        }));
    }

    fn after_view_change(&mut self, follow: bool, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            if self.selected.take().is_some() {
                cx.emit(CallsEvent::Selected(None));
            }
        } else if !self.pinned {
            let newest = self
                .view
                .last()
                .map(|&ix| self.rows[ix as usize].id.clone());
            self.select_id(newest, false, cx);
            if follow && !self.view.is_empty() {
                self.scroll
                    .scroll_to_item(self.view.len() - 1, ScrollStrategy::Bottom);
            }
        } else if self
            .selected
            .as_deref()
            .is_none_or(|id| !self.view_contains(id))
            && let Some(&first) = self.view.first()
        {
            // The selection was filtered out: move to the first match.
            let id = self.rows[first as usize].id.clone();
            self.select_id(Some(id), false, cx);
            self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
        cx.notify();
    }

    fn view_contains(&self, id: &str) -> bool {
        self.view.iter().any(|&ix| self.rows[ix as usize].id == id)
    }

    fn near_bottom(&self) -> bool {
        let state = self.scroll.0.borrow();
        let max = state.base_handle.max_offset().height;
        if max <= px(1.0) {
            return true;
        }
        let offset = state.base_handle.offset().y;
        (-offset + px(48.0)) >= max
    }

    // -- Query and filters ---------------------------------------------------

    /// Debounced: typing does not filter on every keystroke.
    pub fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        if query == self.query {
            return;
        }
        self.query = query;
        self.view_generation += 1;
        let generation = self.view_generation;
        self._search_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            this.update(cx, |list, cx| {
                if list.view_generation == generation {
                    list.tokens = search_tokens(&list.query);
                    list.recompute(cx);
                }
            })
            .ok();
        }));
    }

    pub fn set_filter(&mut self, filter: CallFilter, cx: &mut Context<Self>) {
        if self.filter != filter {
            self.filter = filter;
            self.recompute(cx);
            self.reveal();
        }
    }

    pub fn set_status(&mut self, status: StatusFilter, cx: &mut Context<Self>) {
        let mut filter = self.filter.clone();
        filter.status = if filter.status == status {
            StatusFilter::All
        } else {
            status
        };
        self.set_filter(filter, cx);
    }

    pub fn toggle_sort(&mut self, col: SortCol, cx: &mut Context<Self>) {
        if self.sort_col == col {
            self.sort_asc = !self.sort_asc;
        } else {
            self.sort_col = col;
            self.sort_asc = !matches!(col, SortCol::Duration | SortCol::Status);
        }
        self.recompute(cx);
    }

    pub fn toggle_group_retries(&mut self, cx: &mut Context<Self>) {
        self.group_retries = !self.group_retries;
        self.recompute(cx);
    }

    pub fn set_service(&mut self, service: Option<String>, cx: &mut Context<Self>) {
        let mut filter = self.filter.clone();
        filter.service = if filter.service == service {
            None
        } else {
            service
        };
        self.set_filter(filter, cx);
    }

    /// Escape: drop the most specific filter first. Returns whether one was cleared.
    pub fn clear_one_filter(&mut self, cx: &mut Context<Self>) -> bool {
        let mut filter = self.filter.clone();
        let cleared = filter.ids.take().is_some()
            || filter.time_window.take().is_some()
            || filter.access.take().is_some()
            || filter.service.take().is_some()
            || std::mem::take(&mut filter.status) != StatusFilter::All;
        if cleared {
            self.set_filter(filter, cx);
        }
        cleared
    }

    pub fn show_finding(&mut self, finding: &Finding, cx: &mut Context<Self>) {
        let ids: HashSet<String> = finding.calls.iter().cloned().collect();
        let mut filter = self.filter.clone();
        filter.ids = Some((finding.title.clone(), Arc::new(ids)));
        filter.status = StatusFilter::All;
        self.set_filter(filter, cx);
        if let Some(first) = finding.calls.first() {
            self.select_id(Some(first.clone()), true, cx);
            self.reveal();
        }
    }

    // -- Selection -----------------------------------------------------------

    pub fn select_id(&mut self, id: Option<String>, by_user: bool, cx: &mut Context<Self>) {
        if by_user {
            self.pinned = true;
        }
        if self.selected != id {
            self.selected = id;
            let row = self.selected();
            cx.emit(CallsEvent::Selected(row));
            cx.notify();
        }
    }

    fn position(&self) -> Option<usize> {
        let id = self.selected.as_deref()?;
        self.view
            .iter()
            .position(|&ix| self.rows[ix as usize].id == id)
    }

    pub fn move_by(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.view.is_empty() {
            return;
        }
        let last = self.view.len() as isize - 1;
        let next = match self.position() {
            Some(position) => (position as isize + delta).clamp(0, last),
            None if delta > 0 => 0,
            None => last,
        } as usize;
        let id = self.rows[self.view[next] as usize].id.clone();
        self.select_id(Some(id), true, cx);
        // Non-strict: scrolls only when the row is out of view.
        self.scroll.scroll_to_item(next, ScrollStrategy::Top);
    }

    pub fn page(&mut self, down: bool, cx: &mut Context<Self>) {
        self.move_by(if down { PAGE_ROWS } else { -PAGE_ROWS }, cx);
    }

    pub fn select_edge(&mut self, last: bool, cx: &mut Context<Self>) {
        if self.view.is_empty() {
            return;
        }
        let index = if last { self.view.len() - 1 } else { 0 };
        let id = self.rows[self.view[index] as usize].id.clone();
        // Jumping to the end resumes following new calls.
        self.select_id(Some(id), !last, cx);
        if last {
            self.pinned = false;
        }
        self.scroll.scroll_to_item(index, ScrollStrategy::Bottom);
    }

    fn reveal(&self) {
        if let Some(position) = self.position() {
            self.scroll.scroll_to_item(position, ScrollStrategy::Center);
        }
    }

    // -- Render --------------------------------------------------------------

    fn render_subbar(&self, ui: &Ui, cx: &mut Context<Self>) -> Div {
        let t = ui.p;
        let stats = self.stats;
        let status = self.filter.status;
        let entity = cx.entity();
        let status_chip =
            |id: &'static str, label: String, count: usize, value: StatusFilter, color: u32| {
                let entity = entity.clone();
                widgets::chip(ui, id, label, Some(count), status == value, color).on_click(
                    move |_, _, cx| {
                        entity.update(cx, |list, cx| list.set_status(value, cx));
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
            .overflow_hidden()
            .child(status_chip(
                "f-all",
                ui.t("All", "全部").into(),
                stats.total,
                StatusFilter::All,
                t.accent,
            ));
        if stats.errors > 0 {
            bar = bar.child(status_chip(
                "f-errors",
                ui.t("Errors", "错误").into(),
                stats.errors,
                StatusFilter::Errors,
                t.red,
            ));
        }
        if self.changed > 0 || status == StatusFilter::Changed {
            bar = bar.child(
                status_chip(
                    "f-changed",
                    ui.t("Changed", "有变化").into(),
                    self.changed,
                    StatusFilter::Changed,
                    t.amber,
                )
                .tooltip(widgets::tip(
                    ui.t(
                        "Calls that differ from the baseline: new failures or new access",
                        "与对比基线不同的请求：新失败或新增权限",
                    ),
                    None,
                )),
            );
        }
        if stats.retries > 0 {
            bar = bar.child(status_chip(
                "f-retries",
                ui.t("Retries", "重试").into(),
                stats.retries,
                StatusFilter::Retries,
                t.accent,
            ));
            let entity = entity.clone();
            bar = bar.child(
                widgets::chip(
                    ui,
                    "f-group-retries",
                    ui.t("Group retries", "合并重试"),
                    None,
                    self.group_retries,
                    t.accent,
                )
                .tooltip(widgets::tip(
                    ui.t(
                        "Collapse multi-attempt SDK invocations into their final attempt",
                        "将同一次 SDK 调用的多次重试折叠为最终尝试",
                    ),
                    None,
                ))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |list, cx| list.toggle_group_retries(cx));
                }),
            );
        }
        if stats.slow > 0 {
            bar = bar.child(status_chip(
                "f-slow",
                ui.t("Slow", "慢").into(),
                stats.slow,
                StatusFilter::Slow,
                t.amber,
            ));
        }
        if stats.blocked > 0 {
            bar = bar.child(status_chip(
                "f-blocked",
                ui.t("Blocked", "已拦截").into(),
                stats.blocked,
                StatusFilter::Blocked,
                t.purple,
            ));
        }
        if self.facets.len() > 1 {
            bar = bar.child(div().w(px(1.0)).h(px(16.0)).bg(rgb(t.border)).mx(px(2.0)));
            for (index, facet) in self.facets.iter().enumerate() {
                let active = self.filter.service.as_deref() == Some(facet.id.as_str());
                let color = ui.service_color(&facet.label);
                let id = facet.id.clone();
                let entity = entity.clone();
                bar = bar.child(
                    widgets::chip(
                        ui,
                        ElementId::named_usize("svc", index),
                        ellipsize(&facet.label, 14),
                        Some(facet.count),
                        active,
                        color_hex(color),
                    )
                    .on_click(move |_, _, cx| {
                        let id = id.clone();
                        entity.update(cx, |list, cx| list.set_service(Some(id), cx));
                    }),
                );
            }
        }
        // Active one-off filters as removable chips.
        let mut extra: Vec<(SharedString, CallFilter)> = Vec::new();
        if let Some((title, _)) = self.filter.ids.as_ref() {
            let mut next = self.filter.clone();
            next.ids = None;
            extra.push((format!("{} ×", ellipsize(title, 28)).into(), next));
        }
        if let Some(access) = self.filter.access.as_ref() {
            let mut next = self.filter.clone();
            next.access = None;
            extra.push((format!("{} ×", access.action).into(), next));
        }
        if let Some((start, end)) = self.filter.time_window {
            let mut next = self.filter.clone();
            next.time_window = None;
            extra.push((
                format!("{}–{} ×", format_offset(start), format_offset(end)).into(),
                next,
            ));
        }
        for (index, (title, next)) in extra.into_iter().enumerate() {
            let entity = entity.clone();
            bar = bar.child(
                widgets::chip(
                    ui,
                    ElementId::named_usize("f-extra", index),
                    title,
                    None,
                    true,
                    t.accent,
                )
                .on_click(move |_, _, cx| {
                    let next = next.clone();
                    entity.update(cx, |list, cx| list.set_filter(next, cx));
                }),
            );
        }
        bar.child(div().flex_1())
            .when(!self.tokens.is_empty() || self.filter.is_active(), |bar| {
                bar.child(
                    div()
                        .flex_shrink_0()
                        .font_family(fonts::mono())
                        .text_size(px(text::SM))
                        .text_color(rgb(t.text_muted))
                        .child(format!("{} / {}", self.view.len(), self.rows.len())),
                )
            })
    }

    fn render_findings(
        &self,
        ui: &Ui,
        findings: &[Finding],
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if findings.is_empty() {
            return None;
        }
        let t = ui.p;
        let open = self.findings_open;
        let errors = findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .count();
        let header = div()
            .id("findings-toggle")
            .flex()
            .items_center()
            .gap(px(8.0))
            .h(px(28.0))
            .px(px(theme::ROW_PAD_X))
            .cursor_pointer()
            .hover(move |s| s.bg(theme::h(t.text, 0.03)))
            .on_click(cx.listener(|list, _, _, cx| {
                list.findings_open = !list.findings_open;
                cx.notify();
            }))
            .child(
                div()
                    .text_size(px(text::XS))
                    .text_color(rgb(t.text_muted))
                    .child(if open { "▾" } else { "▸" }),
            )
            .child(widgets::label(ui, ui.t("FINDINGS", "发现")))
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(text::XS))
                    .text_color(rgb(if errors > 0 { t.red } else { t.text_muted }))
                    .child(findings.len().to_string()),
            );
        let mut strip = div()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(rgb(t.border_subtle))
            .bg(rgb(t.panel))
            .child(header);
        if open {
            let mut list = div().flex().flex_col().pb(px(4.0));
            for (index, finding) in findings.iter().take(6).enumerate() {
                let color = match finding.severity {
                    Severity::Error => t.red,
                    Severity::Warning => t.amber,
                    Severity::Info => t.accent,
                };
                let glyph = match finding.severity {
                    Severity::Error => "●",
                    Severity::Warning => "▲",
                    Severity::Info => "◆",
                };
                let finding_clone = finding.clone();
                let mut row = div()
                    .id(ElementId::named_usize("finding", index))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .h(px(24.0))
                    .px(px(theme::ROW_PAD_X))
                    .cursor_pointer()
                    .hover(move |s| s.bg(rgb(t.row_hover)))
                    .on_click(cx.listener(move |list, _, _, cx| {
                        list.show_finding(&finding_clone, cx);
                    }))
                    .child(
                        div()
                            .w(px(12.0))
                            .text_size(px(9.0))
                            .text_color(rgb(color))
                            .child(glyph),
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
                            .child(finding.title.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .font(fonts::mono_font())
                            .text_size(px(text::XS))
                            .text_color(rgb(t.text_muted))
                            .child(i18n::calls_count(finding.calls.len(), ui.lang)),
                    );
                if let Some(detail) = finding.detail.clone() {
                    row = row.tooltip(widgets::tip(detail, None));
                }
                list = list.child(row);
            }
            if findings.len() > 6 {
                list = list.child(
                    div()
                        .px(px(theme::ROW_PAD_X + 20.0))
                        .text_size(px(text::SM))
                        .text_color(rgb(t.text_muted))
                        .child(i18n::more_count(findings.len() - 6, ui.lang)),
                );
            }
            strip = strip.child(list);
        }
        Some(strip)
    }

    fn render_timeline(
        &self,
        ui: &Ui,
        start_ms: Option<i64>,
        span_ms: i64,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        const BUCKETS: usize = 48;
        let start_ms = start_ms?;
        if self.rows.is_empty() {
            return None;
        }
        let t = ui.p;
        let mut counts = [0u16; BUCKETS];
        let mut severity = [0u8; BUCKETS];
        for row in self.rows.iter() {
            let Some(at) = row.at_ms() else { continue };
            let rel_s = (at - start_ms).max(0);
            let rel_e = (rel_s + row.ms as i64).max(rel_s + 1);
            let b0 = ((rel_s as f64 / span_ms as f64) * BUCKETS as f64) as usize;
            let b1 = ((rel_e as f64 / span_ms as f64) * BUCKETS as f64) as usize;
            let sev = if row.blocked {
                3
            } else if row.is_error() {
                2
            } else if row.is_retry() || row.is_slow() {
                1
            } else {
                0
            };
            for idx in b0.min(BUCKETS - 1)..=b1.min(BUCKETS - 1) {
                counts[idx] = counts[idx].saturating_add(1);
                severity[idx] = severity[idx].max(sev);
            }
        }
        let max = counts.iter().copied().max().unwrap_or(1).max(1) as f32;
        let active = self.filter.time_window;
        let mut bars = div()
            .flex()
            .items_end()
            .flex_1()
            .min_w(px(0.0))
            .h(px(14.0))
            .gap(px(2.0));
        for idx in 0..BUCKETS {
            let width = (span_ms as f64 / BUCKETS as f64).max(1.0);
            let win_s = (idx as f64 * width).floor() as i64;
            let win_e = (((idx + 1) as f64 * width).ceil() as i64).max(win_s + 1);
            let selected = active.is_some_and(|(s, e)| win_s >= s && win_e <= e + 1);
            let count = counts[idx];
            let height = if count == 0 {
                2.0
            } else {
                ((count as f32 / max).sqrt().clamp(0.25, 1.0) * 14.0).max(3.0)
            };
            let hex = match severity[idx] {
                3 => t.purple,
                2 => t.red,
                1 => t.amber,
                _ => t.accent,
            };
            let alpha = if count == 0 {
                0.10
            } else if selected {
                1.0
            } else if active.is_some() {
                0.25
            } else {
                0.70
            };
            bars = bars.child(
                div()
                    .id(ElementId::named_usize("tl", idx))
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_end()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |list, _, _, cx| {
                            list.timeline_drag_anchor = Some(idx);
                            let mut filter = list.filter.clone();
                            let next = Some((win_s, win_e));
                            filter.time_window = if filter.time_window == next {
                                None
                            } else {
                                next
                            };
                            list.set_filter(filter, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(move |list, event: &MouseMoveEvent, _, cx| {
                        if event.pressed_button == Some(MouseButton::Left)
                            && let Some(anchor) = list.timeline_drag_anchor
                            && anchor != idx
                        {
                            let lo = anchor.min(idx);
                            let hi = anchor.max(idx);
                            let bs = (lo as f64 * width).floor() as i64;
                            let be = (((hi + 1) as f64 * width).ceil() as i64).max(bs + 1);
                            let mut filter = list.filter.clone();
                            filter.time_window = Some((bs, be));
                            list.set_filter(filter, cx);
                        }
                    }))
                    .child(
                        div()
                            .w_full()
                            .h(px(height))
                            .rounded(px(1.5))
                            .bg(theme::h(hex, alpha)),
                    ),
            );
        }
        Some(
            div()
                .id("timeline")
                .flex()
                .items_center()
                .gap(px(8.0))
                .h(px(theme::TIMELINE_H))
                .px(px(theme::ROW_PAD_X))
                .bg(rgb(t.strip))
                .border_b_1()
                .border_color(rgb(t.border_subtle))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|list, _, _, _| list.timeline_drag_anchor = None),
                )
                .on_mouse_up_out(
                    MouseButton::Left,
                    cx.listener(|list, _, _, _| list.timeline_drag_anchor = None),
                )
                .child(bars)
                .child(
                    div()
                        .flex_shrink_0()
                        .font_family(fonts::mono())
                        .text_size(px(text::XS))
                        .text_color(rgb(t.text_faint))
                        .child(format_latency(span_ms as u64)),
                ),
        )
    }

    fn render_head(&self, ui: &Ui, cx: &mut Context<Self>) -> Div {
        let t = ui.p;
        let sort_col = self.sort_col;
        let sort_asc = self.sort_asc;
        let entity = cx.entity();
        let head = |id: &'static str,
                    col: SortCol,
                    label: &'static str,
                    width: Option<f32>,
                    right: bool| {
            let entity = entity.clone();
            let active = sort_col == col;
            let arrow = if !active || (col == SortCol::Time && sort_asc) {
                ""
            } else if sort_asc {
                " ▲"
            } else {
                " ▼"
            };
            let cell = div()
                .id(id)
                .flex()
                .items_center()
                .when(right, |el| el.justify_end())
                .text_size(px(text::XS))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(if active && !arrow.is_empty() {
                    t.accent
                } else {
                    t.text_muted
                }))
                .cursor_pointer()
                .hover(move |s| s.text_color(rgb(t.text)))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |list, cx| list.toggle_sort(col, cx));
                })
                .child(format!("{label}{arrow}"));
            match width {
                Some(w) => cell.w(px(w)).flex_shrink_0(),
                None => cell.flex_1().min_w(px(0.0)),
            }
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
            .child(head(
                "sort-time",
                SortCol::Time,
                ui.t("TIME", "时间"),
                Some(theme::COL_TIME),
                false,
            ))
            .child(head(
                "sort-svc",
                SortCol::Service,
                "SERVICE",
                Some(theme::COL_SERVICE),
                false,
            ))
            .child(head(
                "sort-op",
                SortCol::Operation,
                "OPERATION",
                None,
                false,
            ))
            .child(head(
                "sort-status",
                SortCol::Status,
                "STATUS",
                Some(theme::COL_STATUS),
                false,
            ))
            .child(head(
                "sort-dur",
                SortCol::Duration,
                ui.t("DURATION", "耗时"),
                Some(theme::COL_LATENCY),
                true,
            ))
    }

    fn render_empty(&self, ui: &Ui, health: &Health, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.entity().downgrade();
        let lang = ui.lang;
        let command = widgets::command_line(ui, START_COMMAND, move |_, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(START_COMMAND.to_string()));
            weak.update(cx, |_, cx| {
                cx.emit(CallsEvent::Toast(i18n::copied("command", lang)))
            })
            .ok();
        });
        match health {
            Health::Loading => widgets::skeleton_rows(ui, 10).into_any_element(),
            Health::Missing => widgets::empty_state(
                ui,
                ui.t("No runs yet", "还没有运行记录"),
                ui.t(
                    "Start your local AWS emulator, then run your tests through Lenslet. Every AWS call shows up here as it happens.",
                    "先启动本地 AWS 模拟器，再通过 Lenslet 运行测试。每个 AWS 调用都会实时出现在这里。",
                ),
                Some(command),
            )
            .into_any_element(),
            Health::Error(error) => widgets::empty_state(
                ui,
                ui.t("Can't read the run file", "无法读取运行文件"),
                format!("{error}. {}", ui.t("Press ⌘R to try again.", "按 ⌘R 重试。")),
                None,
            )
            .into_any_element(),
            Health::Ready if self.rows.is_empty() => widgets::empty_state(
                ui,
                ui.t("Waiting for the first call", "等待第一个请求"),
                ui.t(
                    "The run has started. Point your AWS SDK at the endpoint above; calls appear here live.",
                    "运行已开始。将 AWS SDK 指向上方的 endpoint，请求会实时出现在这里。",
                ),
                None,
            )
            .into_any_element(),
            Health::Ready => {
                let entity = cx.entity();
                widgets::empty_state(
                    ui,
                    ui.t("No calls match", "没有匹配的请求"),
                    ui.t(
                        "Nothing matches the search and filters. Press Esc to clear them one at a time.",
                        "没有符合搜索与筛选条件的请求。按 Esc 逐个清除筛选。",
                    ),
                    Some(
                        widgets::button(ui, "clear-filters", ui.t("Clear filters", "清除筛选"))
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |list, cx| {
                                    list.set_filter(CallFilter::default(), cx);
                                })
                            })
                            .into_any_element(),
                    ),
                )
                .into_any_element()
            }
        }
    }
}

fn color_hex(color: gpui::Rgba) -> u32 {
    let r = (color.r * 255.0).round() as u32;
    let g = (color.g * 255.0).round() as u32;
    let b = (color.b * 255.0).round() as u32;
    (r << 16) | (g << 8) | b
}

/// The view, changed-call count, and per-call collapsed attempt counts.
fn compute_view(
    rows: &[Arc<Trace>],
    query: &ViewQuery,
    sort_col: SortCol,
    sort_asc: bool,
    group_retries: bool,
) -> (Vec<u32>, usize, HashMap<String, usize>) {
    let changed = if query.changes.new_access || !query.changes.new_failures.is_empty() {
        rows.iter()
            .filter(|row| query.changes.is_changed(row))
            .count()
    } else {
        0
    };
    let mut indices = filter_rows(rows, query);
    let mut collapsed_counts: HashMap<String, usize> = HashMap::new();
    if group_retries {
        // Keep only the latest row for each non-replay `invocation_id` in `indices`.
        let mut last_for_inv: HashMap<&str, (u32, usize)> = HashMap::new();
        for &ix in &indices {
            let r = &rows[ix as usize];
            if r.replay_of.is_none()
                && let Some(inv) = r.invocation_id.as_deref()
            {
                let entry = last_for_inv.entry(inv).or_insert((ix, 0));
                entry.0 = ix;
                entry.1 += 1;
            }
        }
        indices.retain(|&ix| {
            let r = &rows[ix as usize];
            if r.replay_of.is_none()
                && let Some(inv) = r.invocation_id.as_deref()
                && let Some(&(keep_ix, count)) = last_for_inv.get(inv)
            {
                if count > 1 && keep_ix == ix {
                    collapsed_counts.insert(r.id.clone(), count);
                }
                return keep_ix == ix;
            }
            true
        });
    }
    if !(sort_col == SortCol::Time && sort_asc) {
        indices.sort_by(|&a, &b| {
            let ra = &rows[a as usize];
            let rb = &rows[b as usize];
            let ord = match sort_col {
                SortCol::Time => a.cmp(&b),
                SortCol::Service => ra
                    .service_name()
                    .cmp(rb.service_name())
                    .then_with(|| a.cmp(&b)),
                SortCol::Operation => operation_label(ra)
                    .cmp(operation_label(rb))
                    .then_with(|| a.cmp(&b)),
                SortCol::Status => ra.status.cmp(&rb.status).then_with(|| a.cmp(&b)),
                SortCol::Duration => ra.ms.cmp(&rb.ms).then_with(|| a.cmp(&b)),
            };
            if sort_asc { ord } else { ord.reverse() }
        });
    }
    (indices, changed, collapsed_counts)
}

impl Render for CallsList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scope = crate::fsguard::RenderScope::enter();
        let ui = Ui::of(cx);
        let t = ui.p;
        let session = self.session.read(cx);
        let health = session.health.clone();
        let findings = session.findings.clone();
        let start_ms = session.run_start_ms();
        let span_ms = session.run_span_ms().max(1);
        let changes = session.changes.clone();

        let body: AnyElement = if self.view.is_empty() {
            self.render_empty(&ui, &health, cx)
        } else {
            crate::fsguard::milestone_once("first frame with rows");
            let rows = self.rows.clone();
            let view = self.view.clone();
            let selected = self.selected.clone();
            let attempt_counts = self.attempt_counts.clone();
            let entity = cx.entity().downgrade();
            uniform_list("calls", view.len(), move |range, _window, cx| {
                let _scope = crate::fsguard::RenderScope::enter();
                let ui = Ui::of(cx);
                range
                    .map(|position| {
                        let index = view[position] as usize;
                        let row = &rows[index];
                        let prev_end = index.checked_sub(1).and_then(|prev| {
                            rows[prev].at_ms().map(|at| at + rows[prev].ms as i64)
                        });
                        let grouped_attempts = attempt_counts.get(&row.id).copied();
                        call_row(
                            &ui,
                            row,
                            position,
                            selected.as_deref() == Some(row.id.as_str()),
                            changes.is_changed(row),
                            grouped_attempts,
                            start_ms,
                            span_ms,
                            prev_end,
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
            .id("calls-list")
            .size_full()
            .key_context("Calls")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .bg(rgb(t.panel))
            .child(self.render_subbar(&ui, cx))
            .children(self.render_findings(&ui, &findings, cx))
            .children(self.render_timeline(&ui, start_ms, span_ms, cx))
            .child(self.render_head(&ui, cx))
            .child(body)
    }
}

#[allow(clippy::too_many_arguments)]
fn call_row(
    ui: &Ui,
    trace: &Arc<Trace>,
    position: usize,
    selected: bool,
    changed: bool,
    grouped_attempts: Option<usize>,
    run_start_ms: Option<i64>,
    run_span_ms: i64,
    prev_end_ms: Option<i64>,
    list: gpui::WeakEntity<CallsList>,
) -> Stateful<Div> {
    let t = ui.p;
    let comfortable = ui.density == crate::prefs::Density::Comfortable;
    let service_color = ui.service_color(trace.service_name());
    let rel = match (trace.at_ms(), run_start_ms) {
        (Some(at), Some(start)) => format_offset(at - start),
        _ => short_clock(&trace.at),
    };
    let gap = match (trace.at_ms(), prev_end_ms) {
        (Some(at), Some(prev)) if at - prev >= 500 => Some(at - prev),
        _ => None,
    };
    let id = trace.id.clone();
    let menu_trace = trace.clone();
    let click_list = list.clone();
    let status_color = if trace.pending {
        rgb(t.amber)
    } else {
        ui.status_color(trace.status)
    };
    let semantic = trace.semantic_target();

    let mut title = div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .min_w(px(0.0))
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(text::LG))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(t.text))
                .whitespace_nowrap()
                .child(operation_label(trace).to_string()),
        );
    if !comfortable {
        let compact_hint = if let Some(problem) = trace.problem.as_ref() {
            Some((problem.code.clone(), status_color))
        } else {
            (!semantic.is_empty()).then(|| (semantic.clone(), rgb(t.text_muted)))
        };
        if let Some((hint_text, hint_color)) = compact_hint {
            title = title.child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .font(fonts::mono_font())
                    .text_size(px(text::XS))
                    .text_color(hint_color)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(hint_text),
            );
        }
    }
    if trace.blocked {
        title = title.child(widgets::badge(ui, "BLOCKED", t.purple));
    }
    if changed && trace.is_error() {
        title = title.child(widgets::badge(ui, "NEW FAILURE", t.red));
    } else if trace.new && !trace.blocked {
        title = title.child(widgets::badge(ui, "NEW ACCESS", t.amber));
    }
    if let Some(n) = grouped_attempts {
        title = title.child(widgets::badge(ui, format!("↻ ×{n}"), t.accent));
    } else if let Some(retry) = trace.retry_badge() {
        title = title.child(widgets::badge(ui, retry, t.accent));
    }
    if trace.replay_of.is_some() {
        title = title.child(widgets::badge(ui, "REPLAY", t.accent));
    }
    if let Some(role) = trace.caller_role.as_deref() {
        let short_role = role.rsplit('/').next().unwrap_or(role);
        title = title.child(widgets::badge(
            ui,
            format!("role/{}", ellipsize(short_role, 18)),
            t.text_muted,
        ));
    }

    let detail = match trace.problem.as_ref() {
        Some(problem) => div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .min_w(px(0.0))
            .child(
                div()
                    .flex_shrink_0()
                    .font(fonts::mono_font())
                    .text_size(px(text::SM))
                    .text_color(status_color)
                    .child(ellipsize(&problem.code, 40)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_size(px(text::SM))
                    .text_color(rgb(t.text_dim))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(
                        problem
                            .message
                            .clone()
                            .filter(|m| !m.is_empty())
                            .unwrap_or_else(|| semantic.clone()),
                    ),
            ),
        None if !semantic.is_empty() => div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .min_w(px(0.0))
            .font(fonts::mono_font())
            .text_size(px(text::SM))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_color(rgb(t.text_soft))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(semantic),
            ),
        None => div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .min_w(px(0.0))
            .font(fonts::mono_font())
            .text_size(px(text::SM))
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(rgb(t.text_muted))
                    .child(trace.method.to_ascii_uppercase()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_color(rgb(t.text_muted))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(display_path(trace).to_string()),
            ),
    };

    const TRACK_W: f32 = 44.0;
    let (wf_left, wf_w) = match (trace.at_ms(), run_start_ms) {
        (Some(at), Some(start)) => {
            let s = ((at - start).max(0) as f32 / run_span_ms as f32).clamp(0.0, 0.92);
            // Log-scaled duration width so 2ms vs 50ms vs 800ms calls stay visually legible
            // even across multi-minute sessions.
            let log_ratio = ((1.0 + trace.ms.max(1) as f32).ln()
                / (1.0 + run_span_ms.max(20) as f32).ln())
            .clamp(0.08, 0.75);
            let left = (s * TRACK_W).min(TRACK_W - 4.0);
            (left, (log_ratio * TRACK_W).clamp(3.5, TRACK_W - left))
        }
        _ => (0.0, 6.0),
    };
    let wf_color = if trace.blocked {
        theme::h(t.purple, 0.9)
    } else if trace.is_error() {
        theme::h(t.red, 0.9)
    } else if trace.is_slow() {
        theme::h(t.amber, 0.9)
    } else {
        theme::tint(service_color, 0.75)
    };

    div()
        .id(ElementId::named_usize("call", position))
        .relative()
        .flex()
        .items_center()
        .gap(px(theme::COL_GAP))
        .w_full()
        .h(px(ui.row_height()))
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
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            let id = id.clone();
            click_list
                .update(cx, |list, cx| {
                    window.focus(&list.focus);
                    list.select_id(Some(id), true, cx);
                })
                .ok();
        })
        .on_mouse_down(
            MouseButton::Right,
            move |event: &MouseDownEvent, window, cx| {
                let trace = menu_trace.clone();
                list.update(cx, |list, cx| {
                    window.focus(&list.focus);
                    list.select_id(Some(trace.id.clone()), true, cx);
                    cx.emit(CallsEvent::ContextMenu(event.position, trace));
                })
                .ok();
            },
        )
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(px(2.0))
                .when(selected, |el| el.bg(rgb(t.accent)))
                .when(!selected && trace.is_error(), |el| {
                    el.bg(theme::h(t.red, 0.7))
                }),
        )
        .child(
            div()
                .w(px(theme::COL_TIME))
                .flex_shrink_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .font_family(fonts::mono())
                .child(
                    div()
                        .text_size(px(text::SM))
                        .text_color(rgb(t.text_dim))
                        .child(rel),
                )
                .when(comfortable, |col| {
                    col.child(match gap {
                        Some(gap) => div()
                            .text_size(px(text::XS))
                            .text_color(rgb(t.amber))
                            .child(format!("Δ {}", format_latency(gap as u64))),
                        None => div()
                            .text_size(px(text::XS))
                            .text_color(rgb(t.text_faint))
                            .child(short_clock(&trace.at)),
                    })
                }),
        )
        .child(
            div()
                .w(px(theme::COL_SERVICE))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(6.0))
                .overflow_hidden()
                .child(glyph::dot(service_color, 6.0))
                .child(
                    div()
                        .min_w(px(0.0))
                        .text_size(px(text::SM))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(service_color)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(trace.service_name().to_string()),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(title)
                .when(comfortable, |col| col.child(detail)),
        )
        .child(
            div()
                .w(px(theme::COL_STATUS))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(glyph::dot(status_color, 6.0))
                .child(
                    div()
                        .font_family(fonts::mono())
                        .text_size(px(text::SM))
                        .text_color(if trace.is_error() || trace.pending {
                            status_color
                        } else {
                            rgb(t.text_body)
                        })
                        .child(if trace.pending {
                            "…".to_string()
                        } else {
                            status_label(trace.status)
                        }),
                ),
        )
        .child(
            div()
                .w(px(theme::COL_LATENCY))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.0))
                .child(
                    div()
                        .relative()
                        .w(px(TRACK_W))
                        .h(px(4.0))
                        .rounded(px(2.0))
                        .bg(theme::h(t.text, 0.06))
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left(px(wf_left))
                                .w(px(wf_w))
                                .rounded(px(2.0))
                                .bg(wf_color),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .text_right()
                        .font_family(fonts::mono())
                        .text_size(px(text::SM))
                        .text_color(if trace.pending {
                            rgb(t.amber)
                        } else {
                            ui.latency_color(trace.ms)
                        })
                        .child(if trace.pending {
                            ui.t("running", "进行中").to_string()
                        } else {
                            format_latency(trace.ms)
                        }),
                ),
        )
}
