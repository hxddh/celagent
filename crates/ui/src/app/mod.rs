//! Lenslet's single-window view: stream, inspector, live tail and keyboard flow.

use std::{
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui::{
    AnyElement, ClipboardItem, Context, Div, ElementId, FocusHandle, FontWeight, KeyDownEvent,
    MouseButton, Render, ScrollStrategy, SharedString, Stateful, UniformListScrollHandle,
    WeakEntity, Window, div, linear_color_stop, linear_gradient, prelude::*, px, rgb, rgba,
    uniform_list,
};

mod access;
mod chrome;
mod inspector;
mod stream;
mod widgets;

use self::{stream::new_pill, widgets::*};
use crate::{
    demo, fonts, glyph,
    search::SearchInput,
    theme::{self, latency_color, status_color},
    trace::{
        self, Body, Exchange, Filter, MAX_ROWS, RunInfo, ServiceFacet, Source, TailEvent, Tailer,
        Trace, format_bytes, format_latency, full_time, headers_to_text, matches_search,
        operation_label, search_tokens, service_facets, short_clock, status_is_success,
        status_label, status_text,
    },
};

/// What the empty window tells people to type.
const START_COMMAND: &str = "lenslet run -- npm test";
const POLL_INTERVAL: Duration = Duration::from_millis(420);
/// Probe the proxy and emulator every few polls (~2 s).
const PROBE_EVERY: u64 = 5;
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);
const TOAST_MILLIS: u64 = 1_600;
const MAX_BODY_DISPLAY: usize = 24_000;

#[derive(Clone, Debug, PartialEq, Eq)]
enum TraceHealth {
    Ok,
    Missing,
    Error(String),
}

/// The left pane: every call, or the permissions those calls needed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Calls,
    Access,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InspectorTab {
    Overview,
    Request,
    Response,
}

impl InspectorTab {
    fn label(self) -> &'static str {
        match self {
            InspectorTab::Overview => "Overview",
            InspectorTab::Request => "Request",
            InspectorTab::Response => "Response",
        }
    }

    fn index(self) -> &'static str {
        match self {
            InspectorTab::Overview => "1",
            InspectorTab::Request => "2",
            InspectorTab::Response => "3",
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Counts {
    total: usize,
    reported_total: Option<usize>,
    retries: usize,
    errors: usize,
    visible: usize,
    visible_errors: usize,
    services: usize,
}

pub struct LensletApp {
    rows: Rc<Vec<Arc<Trace>>>,
    view: Rc<Vec<usize>>,
    facets: Vec<ServiceFacet>,
    counts: Counts,
    selected: Option<usize>,
    /// Set once the user picks a call; live appends then stop re-selecting.
    selection_locked: bool,
    filter: Filter,
    query: String,
    tab: InspectorTab,
    pane: Pane,
    show_all_services: bool,

    search: gpui::Entity<SearchInput>,
    search_focus: FocusHandle,
    root_focus: FocusHandle,
    scroll: UniformListScrollHandle,

    paused: bool,
    buffered: Vec<Trace>,
    pending_replace: bool,

    source: Source,
    /// The trace file being followed right now.
    trace_path: Option<PathBuf>,
    /// The run being shown: its header, and its report once it ended.
    run: Option<RunInfo>,
    probe: Probe,
    tailer: Option<Tailer>,
    tailer_generation: u64,
    health: TraceHealth,
    skipped: usize,
    demo: bool,

    toast: Option<(String, u64)>,
    toast_seq: u64,
}

impl LensletApp {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let source = trace_source();
        let demo = std::env::var_os("LENSLET_DEMO")
            .map(|value| value.to_string_lossy() == "1")
            .unwrap_or(false);
        let root_focus = cx.focus_handle();
        let search = cx.new(|cx| SearchInput::new(root_focus.clone(), cx));
        let search_focus = search.read(cx).handle();

        let mut rows: Vec<Arc<Trace>> = Vec::new();
        let mut tailer = None;
        let mut health = TraceHealth::Ok;
        let mut skipped = 0;
        let mut trace_path = None;
        let run: Option<trace::RunInfo>;

        if demo {
            rows = trace::merge(&[], demo::demo_rows());
            run = Some(demo::demo_run());
        } else {
            let mut tail = Tailer::follow(source.clone());
            match tail.poll() {
                TailEvent::Replaced(found) => {
                    rows = trace::merge(&[], found);
                }
                TailEvent::Missing => health = TraceHealth::Missing,
                TailEvent::Error(error) => health = TraceHealth::Error(error),
                _ => {}
            }
            skipped = tail.skipped();
            trace_path = tail.path().map(Path::to_path_buf);
            run = tail.run();
            tailer = Some(tail);
        }

        let mut app = Self {
            rows: Rc::new(rows),
            view: Rc::new(Vec::new()),
            facets: Vec::new(),
            counts: Counts::default(),
            selected: if demo { Some(0) } else { None },
            selection_locked: demo,
            filter: Filter::All,
            query: String::new(),
            tab: InspectorTab::Overview,
            pane: Pane::Calls,
            show_all_services: false,
            search,
            search_focus,
            root_focus,
            scroll: UniformListScrollHandle::new(),
            paused: false,
            buffered: Vec::new(),
            pending_replace: false,
            source,
            trace_path,
            run,
            probe: Probe::default(),
            tailer,
            tailer_generation: 0,
            health,
            skipped,
            demo,
            toast: None,
            toast_seq: 0,
        };
        app.refresh_view();
        if !app.demo && !app.view.is_empty() {
            app.scroll
                .scroll_to_item(app.view.len() - 1, ScrollStrategy::Bottom);
        }

        cx.observe(&app.search, |this, search, cx| {
            let query = search.read(cx).query().to_string();
            if query != this.query {
                this.query = query;
                this.refresh_view();
                this.reveal_selected();
                cx.notify();
            }
        })
        .detach();

        if !app.demo {
            app.start_polling(cx);
        }

        app
    }

    pub fn root_focus(&self) -> FocusHandle {
        self.root_focus.clone()
    }

    // -- data ---------------------------------------------------------------

    fn refresh_view(&mut self) {
        self.facets = service_facets(self.rows.iter().map(|row| row.as_ref()));

        if let Filter::Service(id) = &self.filter
            && !self.facets.iter().any(|facet| &facet.id == id)
        {
            self.filter = Filter::All;
        }

        self.counts.total = self.rows.len();
        let report = self.run.as_ref().and_then(|run| run.report.as_ref());
        self.counts.reported_total = report
            .map(|r| r.calls)
            .filter(|calls| *calls > self.rows.len());
        self.counts.retries = report
            .map(|r| r.retries)
            .filter(|retries| *retries > 0)
            .unwrap_or_else(|| self.rows.iter().filter(|row| row.is_retry()).count());
        self.counts.errors = self.rows.iter().filter(|row| row.is_error()).count();
        self.counts.services = self.facets.len();

        let tokens = search_tokens(&self.query);
        let mut view = Vec::new();
        let mut visible_errors = 0;
        for (index, row) in self.rows.iter().enumerate() {
            if self.filter.matches(row) && matches_search(row, &tokens) {
                view.push(index);
                if row.is_error() {
                    visible_errors += 1;
                }
            }
        }
        self.counts.visible = view.len();
        self.counts.visible_errors = visible_errors;
        self.view = Rc::new(view);

        // Live mode follows the newest call until the user deliberately picks
        // one. After that, incoming calls must not move the inspector away.
        if !self.selection_locked {
            self.selected = self.view.last().copied();
        } else if let Some(selected) = self.selected
            && !self.view.contains(&selected)
        {
            self.selected = self.view.last().copied();
        }
        if self.rows.is_empty() {
            self.selected = None;
        }
    }

    fn reveal_selected(&self) {
        if let Some(selected) = self.selected
            && let Some(position) = self.view.iter().position(|index| *index == selected)
        {
            self.scroll.scroll_to_item(position, ScrollStrategy::Center);
        }
    }

    fn near_bottom(&self) -> bool {
        let state = self.scroll.0.borrow();
        let max = state.base_handle.max_offset().height;
        if max <= px(1.0) {
            return true;
        }
        let offset = state.base_handle.offset().y;
        (-offset + px(36.0)) >= max
    }

    fn should_follow(&self) -> bool {
        if self.paused {
            return false;
        }
        self.near_bottom()
            && self
                .selected
                .is_none_or(|selected| selected + 1 >= self.rows.len())
    }

    fn apply_rows(&mut self, incoming: Vec<Trace>, replace: bool, follow: bool) {
        let selected_key = self
            .selected
            .and_then(|index| self.rows.get(index))
            .map(|row| row.key());
        let base: &[Arc<Trace>] = if replace { &[] } else { &self.rows };
        self.rows = Rc::new(trace::merge(base, incoming));
        // Rows can be replaced (pending → done), reordered or dropped, so the
        // selection follows its call, not its index.
        self.selected =
            selected_key.and_then(|key| self.rows.iter().position(|row| row.key() == key));

        self.refresh_view();
        if follow && !self.view.is_empty() {
            self.scroll
                .scroll_to_item(self.view.len() - 1, ScrollStrategy::Bottom);
        }
    }

    fn ingest(&mut self, incoming: Vec<Trace>, replace: bool, cx: &mut Context<Self>) {
        if self.paused {
            if replace {
                self.buffered.clear();
                self.pending_replace = true;
            }
            self.buffered.extend(incoming);
            if self.buffered.len() > MAX_ROWS {
                let dropped = self.buffered.len() - MAX_ROWS;
                self.buffered.drain(..dropped);
            }
            cx.notify();
            return;
        }

        let follow = self.should_follow();
        self.apply_rows(incoming, replace, follow);
        cx.notify();
    }

    fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        self.paused = !self.paused;
        if !self.paused {
            let follow = self.near_bottom()
                && self
                    .selected
                    .is_none_or(|selected| selected + 1 >= self.rows.len());
            let buffered = std::mem::take(&mut self.buffered);
            let replace = std::mem::take(&mut self.pending_replace);
            if !buffered.is_empty() || replace {
                self.apply_rows(buffered, replace, follow);
            }
        }
        cx.notify();
    }

    fn apply_tail(&mut self, event: TailEvent, cx: &mut Context<Self>) {
        if let Some(tailer) = self.tailer.as_ref() {
            self.skipped = tailer.skipped();
            let path = tailer.path().map(Path::to_path_buf);
            let run = tailer.run();
            if path != self.trace_path || run != self.run {
                if path != self.trace_path {
                    self.probe = Probe::default();
                }
                self.trace_path = path;
                self.run = run;
                self.refresh_view();
                cx.notify();
            }
        }
        match event {
            TailEvent::Missing => {
                if self.health != TraceHealth::Missing {
                    self.health = TraceHealth::Missing;
                    cx.notify();
                }
            }
            TailEvent::Error(error) => {
                if self.health != TraceHealth::Error(error.clone()) {
                    self.health = TraceHealth::Error(error);
                    cx.notify();
                }
            }
            TailEvent::Idle => {
                if self.health != TraceHealth::Ok {
                    self.health = TraceHealth::Ok;
                    cx.notify();
                }
            }
            TailEvent::Appended(incoming) => {
                self.health = TraceHealth::Ok;
                if incoming.is_empty() {
                    cx.notify();
                } else {
                    self.ingest(incoming, false, cx);
                }
            }
            TailEvent::Replaced(found) => {
                self.health = TraceHealth::Ok;
                self.ingest(found, true, cx);
            }
        }
    }

    fn start_polling(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut tick: u64 = 0;
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                tick += 1;
                if tick % PROBE_EVERY == 1 {
                    let targets = this.update(cx, |app, _| app.probe_targets());
                    let Ok(targets) = targets else {
                        break;
                    };
                    let probe = cx
                        .background_executor()
                        .spawn(async move { Probe::run(targets) })
                        .await;
                    let changed = this.update(cx, |app, cx| {
                        if app.probe != probe {
                            app.probe = probe;
                            cx.notify();
                        }
                    });
                    if changed.is_err() {
                        break;
                    }
                }

                let payload = this.update(cx, |app, _| {
                    app.tailer
                        .as_ref()
                        .map(|tailer| (app.tailer_generation, tailer.clone()))
                });
                let Ok(payload) = payload else {
                    break;
                };
                let Some((generation, tailer)) = payload else {
                    continue;
                };

                let (tailer, event) = cx
                    .background_executor()
                    .spawn(async move {
                        let mut tailer = tailer;
                        let event = tailer.poll();
                        (tailer, event)
                    })
                    .await;

                let result = this.update(cx, |app, cx| {
                    if app.tailer_generation == generation {
                        app.tailer = Some(tailer);
                        app.apply_tail(event, cx);
                    }
                });
                if result.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn reload_now(&mut self, cx: &mut Context<Self>) {
        if self.demo {
            self.show_toast("demo data is static".to_string(), cx);
            return;
        }
        self.tailer_generation += 1;
        let mut tailer = Tailer::follow(self.source.clone());
        let event = tailer.poll();
        self.skipped = tailer.skipped();
        self.tailer = Some(tailer);
        self.apply_tail(event, cx);
        self.show_toast("reloaded trace".to_string(), cx);
    }

    fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.filter = if self.filter == filter {
            Filter::All
        } else {
            filter
        };
        self.refresh_view();
        self.reveal_selected();
        cx.notify();
    }

    fn set_tab(&mut self, tab: InspectorTab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            cx.notify();
        }
    }

    fn select_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(index);
        self.selection_locked = true;
        window.focus(&self.root_focus);
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.selection_locked = true;
        if self.view.is_empty() {
            self.selected = None;
            cx.notify();
            return;
        }
        let current = self
            .selected
            .and_then(|raw| self.view.iter().position(|index| *index == raw));
        let next = match current {
            Some(position) => {
                (position as isize + delta).clamp(0, self.view.len() as isize - 1) as usize
            }
            None => {
                if delta > 0 {
                    0
                } else {
                    self.view.len() - 1
                }
            }
        };
        self.selected = Some(self.view[next]);
        self.scroll.scroll_to_item(next, ScrollStrategy::Center);
        cx.notify();
    }

    fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.search_focus);
        cx.notify();
    }

    fn clear_search_and_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter = Filter::All;
        self.search
            .update(cx, |search, cx| search.clear(window, cx));
        self.query.clear();
        self.refresh_view();
        self.reveal_selected();
        cx.notify();
    }

    fn copy_text(&mut self, label: &'static str, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.show_toast(format!("copied {label}"), cx);
    }

    fn show_toast(&mut self, message: String, cx: &mut Context<Self>) {
        self.toast_seq += 1;
        let sequence = self.toast_seq;
        self.toast = Some((message, sequence));
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(TOAST_MILLIS))
                .await;
            this.update(cx, |app, cx| {
                if app.toast.as_ref().map(|(_, seq)| *seq) == Some(sequence) {
                    app.toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let search_focused = self.search_focus.is_focused(window);

        if keystroke.modifiers.secondary() && keystroke.key == "k" {
            self.focus_search(window, cx);
            return;
        }

        match keystroke.key.as_str() {
            "up" => self.move_selection(-1, cx),
            "down" => self.move_selection(1, cx),
            "enter" if search_focused => {
                window.focus(&self.root_focus);
                cx.notify();
            }
            "space" if !search_focused => self.toggle_pause(cx),
            "r" if !search_focused
                && !keystroke.modifiers.secondary()
                && !keystroke.modifiers.alt =>
            {
                self.reload_now(cx)
            }
            "a" if !search_focused && !keystroke.modifiers.modified() => {
                let next = if self.pane == Pane::Calls {
                    Pane::Access
                } else {
                    Pane::Calls
                };
                self.set_pane(next, cx)
            }
            "[" if !search_focused => self.step_run(-1, cx),
            "]" if !search_focused => self.step_run(1, cx),
            "1" if !search_focused && !keystroke.modifiers.modified() => {
                self.set_tab(InspectorTab::Overview, cx)
            }
            "2" if !search_focused && !keystroke.modifiers.modified() => {
                self.set_tab(InspectorTab::Request, cx)
            }
            "3" if !search_focused && !keystroke.modifiers.modified() => {
                self.set_tab(InspectorTab::Response, cx)
            }
            _ => {}
        }
    }

    fn set_pane(&mut self, pane: Pane, cx: &mut Context<Self>) {
        if self.pane != pane {
            self.pane = pane;
            cx.notify();
        }
    }

    /// From the access table: show the calls behind one permission.
    fn show_access(&mut self, access: trace::Access, cx: &mut Context<Self>) {
        self.filter = Filter::Access(access);
        self.pane = Pane::Calls;
        self.selection_locked = false;
        self.refresh_view();
        self.reveal_selected();
        cx.notify();
    }

    /// The runs this window can step through, and where the current one is.
    fn run_position(&self) -> Option<(usize, usize)> {
        let Source::Runs { dir, .. } = &self.source else {
            return None;
        };
        let runs = trace::list_runs(dir);
        let current = self.trace_path.as_ref()?;
        let index = runs.iter().position(|run| run == current)?;
        Some((index + 1, runs.len()))
    }

    /// `[` / `]`: the previous or next run. Stepping onto the newest run
    /// follows new runs again.
    fn step_run(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.demo {
            return;
        }
        let Source::Runs { dir, .. } = &self.source else {
            self.show_toast("pinned to LENSLET_TRACE".to_string(), cx);
            return;
        };
        let dir = dir.clone();
        let runs = trace::list_runs(&dir);
        let Some(current) = self
            .trace_path
            .as_ref()
            .and_then(|path| runs.iter().position(|run| run == path))
        else {
            return;
        };
        let target = (current as isize + delta).clamp(0, runs.len() as isize - 1) as usize;
        if target == current {
            return;
        }
        let pinned = (target + 1 < runs.len()).then(|| runs[target].clone());
        self.source = Source::Runs { dir, pinned };
        self.selected = None;
        self.selection_locked = false;
        self.filter = Filter::All;
        self.tailer_generation += 1;
        let mut tailer = Tailer::follow(self.source.clone());
        let event = tailer.poll();
        self.tailer = Some(tailer);
        self.apply_tail(event, cx);
        cx.notify();
    }

    /// The access table: the finished run's report (including unused lock
    /// entries and `# test` marks), or live from the calls.
    fn access_rows(&self) -> Vec<trace::AccessEntry> {
        let test_marks = self
            .run
            .as_ref()
            .and_then(|run| {
                let lock = run.header.lock.as_deref()?;
                Some(trace::read_test_marks(&run.header.cwd, lock))
            })
            .unwrap_or_default();

        if let Some(report) = self.run.as_ref().and_then(|run| run.report.as_ref()) {
            let mut rows = report.access.clone();
            for entry in &mut rows {
                let key = trace::Access {
                    action: entry.action.clone(),
                    resource: entry.resource.clone(),
                };
                entry.test = test_marks.contains(&key);
            }
            for unused in &report.access_unused {
                rows.push(trace::AccessEntry {
                    action: unused.action.clone(),
                    resource: unused.resource.clone(),
                    unused: true,
                    test: test_marks.contains(unused),
                    ..trace::AccessEntry::default()
                });
            }
            rows.sort_by(|a, b| {
                a.action
                    .cmp(&b.action)
                    .then_with(|| a.resource.cmp(&b.resource))
            });
            return rows;
        }
        let mut table: std::collections::BTreeMap<trace::Access, trace::AccessEntry> =
            std::collections::BTreeMap::new();
        for row in self.rows.iter().filter(|row| !row.pending) {
            for (call_access, implied) in row.call_access() {
                let key = call_access.key();
                let is_new = row.is_access_new(call_access);
                let entry = table
                    .entry(key.clone())
                    .or_insert_with(|| trace::AccessEntry {
                        action: key.action.clone(),
                        resource: key.resource.clone(),
                        implied,
                        test: test_marks.contains(&key),
                        ..trace::AccessEntry::default()
                    });
                entry.calls += 1;
                entry.errors += usize::from(row.is_error());
                entry.implied &= implied;
                entry.new |= is_new;
                let via = format!("{} {}", row.service_name(), operation_label(row));
                if entry.via.len() < 3 && !entry.via.contains(&via) {
                    entry.via.push(via);
                }
            }
        }
        table.into_values().collect()
    }

    pub(super) fn run_start_ms(&self) -> Option<i64> {
        self.run
            .as_ref()
            .and_then(|run| trace::parse_rfc3339_ms(&run.header.started))
            .or_else(|| self.rows.first().and_then(|row| row.at_ms()))
    }

    /// Count and combined wall time of all attempts sharing `trace`'s invocation id.
    pub(super) fn invocation_stats(&self, trace: &Trace) -> Option<(usize, u64)> {
        let id = trace.invocation_id.as_deref()?;
        let mut count = 0usize;
        let mut total_ms = 0u64;
        for row in self.rows.iter().filter(|row| !row.pending) {
            if row.invocation_id.as_deref() == Some(id) {
                count += 1;
                total_ms += row.ms;
            }
        }
        (count > 1).then_some((count, total_ms))
    }

    // -- render -------------------------------------------------------------

    fn copy_button(
        &self,
        id: &'static str,
        label: &'static str,
        text: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(text) = text else {
            return div().into_any_element();
        };
        action_button(id, "Copy")
            .on_click(cx.listener(move |this, _, _, cx| {
                this.copy_text(label, text.clone(), cx);
            }))
            .into_any_element()
    }
}

impl Render for LensletApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport_width = window.viewport_size().width.to_f64() as f32;
        let compact = viewport_width < 1_180.0;
        let fixed_columns = theme::COL_TIME
            + theme::COL_SERVICE
            + theme::COL_STATUS
            + theme::COL_LATENCY
            + theme::COL_AUTH;
        let op_width = (viewport_width
            - theme::INSPECTOR_W
            - theme::ROW_PAD_X * 2.0
            - fixed_columns
            - theme::COL_GAP * 5.0
            - 10.0)
            .max(60.0);
        let header = self.render_header(cx);
        let toolbar = self.render_toolbar(compact, cx);
        let strip = self
            .show_all_services
            .then(|| self.render_service_strip(cx));
        let stream = self.render_stream(op_width, cx);
        let inspector = self.render_inspector(cx);
        let footer = self.render_footer(compact);

        div()
            .id("lenslet")
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(theme::CANVAS))
            .text_color(rgb(theme::TEXT))
            .font_family(fonts::sans())
            .track_focus(&self.root_focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, _cx| {
                    window.focus(&this.root_focus);
                }),
            )
            .child(header)
            .child(toolbar)
            .children(strip)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(stream)
                    .child(inspector),
            )
            .child(footer)
    }
}

#[derive(Clone, Copy)]
enum Side {
    Request,
    Response,
}

impl Side {
    fn noun(self) -> &'static str {
        match self {
            Side::Request => "request",
            Side::Response => "response",
        }
    }

    fn target_label(self) -> &'static str {
        match self {
            Side::Request => "TARGET",
            Side::Response => "STATUS LINE",
        }
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// `LENSLET_TRACE` pins one file; otherwise follow the newest run.
fn trace_source() -> Source {
    match std::env::var_os("LENSLET_TRACE") {
        Some(path) => Source::File(PathBuf::from(path)),
        None => {
            let home = home();
            Source::Runs {
                dir: home.join(".lenslet/runs"),
                pinned: None,
            }
        }
    }
}

/// Whether the proxy and the emulator of the followed run answer on TCP.
/// `None` means unknown (no run, or not probed yet).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Probe {
    proxy: Option<bool>,
    emulator: Option<bool>,
}

#[derive(Default)]
struct ProbeTargets {
    proxy: Option<String>,
    emulator: Option<String>,
}

impl Probe {
    fn run(targets: ProbeTargets) -> Probe {
        Probe {
            proxy: targets.proxy.as_deref().map(tcp_answers),
            emulator: targets.emulator.as_deref().map(tcp_answers),
        }
    }
}

fn tcp_answers(address: &str) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    let Ok(mut addresses) = address.to_socket_addrs() else {
        return false;
    };
    addresses.any(|address| TcpStream::connect_timeout(&address, PROBE_TIMEOUT).is_ok())
}

/// One status light: dot color, text, text color.
pub(super) struct Light {
    pub dot: gpui::Hsla,
    pub text: String,
    pub color: u32,
}

impl Light {
    fn new(dot: u32, alpha: f32, text: String, color: u32) -> Self {
        Light {
            dot: theme::h(dot, alpha),
            text,
            color,
        }
    }
}

impl LensletApp {
    /// The followed run's endpoint without the scheme, e.g. `127.0.0.1:43761`.
    pub(super) fn endpoint_label(&self) -> String {
        if self.demo {
            return "127.0.0.1:9297".to_string();
        }
        self.run
            .as_ref()
            .and_then(|run| RunInfo::address(&run.header.endpoint))
            .map(str::to_string)
            .unwrap_or_else(|| "no run yet".to_string())
    }

    /// The proxy light: live, gone, or the finished run's exit code.
    pub(super) fn proxy_light(&self) -> Light {
        if self.demo {
            return Light::new(theme::AMBER, 0.9, "demo data".into(), theme::AMBER);
        }
        let Some(run) = self.run.as_ref() else {
            return Light::new(
                theme::TEXT_FAINT,
                0.8,
                "no run · lenslet run -- <command>".into(),
                theme::TEXT_MUTED,
            );
        };
        if run.ended() {
            return match run.exit_code() {
                Some(0) => Light::new(
                    theme::GREEN,
                    0.6,
                    "run ended · exit 0".into(),
                    theme::TEXT_MUTED,
                ),
                Some(code) => Light::new(
                    theme::RED,
                    0.9,
                    format!("run ended · exit {code}"),
                    theme::RED,
                ),
                None => Light::new(
                    theme::TEXT_FAINT,
                    0.8,
                    "proxy stopped".into(),
                    theme::TEXT_MUTED,
                ),
            };
        }
        let port = RunInfo::address(&run.header.endpoint)
            .and_then(|address| address.rsplit(':').next())
            .unwrap_or("?");
        match self.probe.proxy {
            Some(true) => Light::new(
                theme::GREEN,
                0.95,
                format!("proxy :{port} live"),
                theme::TEXT_MUTED,
            ),
            Some(false) => Light::new(
                theme::RED,
                0.95,
                format!("proxy :{port} not answering"),
                theme::RED,
            ),
            None => Light::new(
                theme::AMBER,
                0.9,
                format!("proxy :{port}"),
                theme::TEXT_MUTED,
            ),
        }
    }

    /// The emulator light, from a TCP probe of the run's upstream.
    pub(super) fn emulator_light(&self) -> Option<Light> {
        if self.demo {
            return Some(Light::new(
                theme::AMBER,
                0.6,
                "emulator :9298".into(),
                theme::TEXT_MUTED,
            ));
        }
        let run = self.run.as_ref()?;
        let address = RunInfo::address(&run.header.upstream).unwrap_or("?");
        let port = address.rsplit(':').next().unwrap_or(address);
        Some(match self.probe.emulator {
            Some(true) => Light::new(
                theme::GREEN,
                0.95,
                format!("emulator :{port} up"),
                theme::TEXT_MUTED,
            ),
            Some(false) => Light::new(
                theme::RED,
                0.95,
                format!("emulator :{port} down"),
                theme::RED,
            ),
            None => Light::new(
                theme::TEXT_FAINT,
                0.8,
                format!("emulator :{port}"),
                theme::TEXT_MUTED,
            ),
        })
    }

    fn probe_targets(&self) -> ProbeTargets {
        let Some(run) = self.run.as_ref() else {
            return ProbeTargets::default();
        };
        ProbeTargets {
            // An ended run has no proxy to ask.
            proxy: (!run.ended())
                .then(|| RunInfo::address(&run.header.endpoint).map(str::to_string))
                .flatten(),
            emulator: RunInfo::address(&run.header.upstream).map(str::to_string),
        }
    }
}

fn display_trace_path(path: Option<&Path>) -> String {
    let Some(path) = path else {
        return "~/.lenslet/runs".to_string();
    };
    let text = path.display().to_string();
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if let Some(rest) = text.strip_prefix(home.as_ref()) {
            return format!("~{rest}");
        }
    }
    text
}

fn call_count(count: usize) -> String {
    if count == 1 {
        "1 call".to_string()
    } else {
        format!("{count} calls")
    }
}

fn error_count(count: usize) -> String {
    if count == 1 {
        "1 error".to_string()
    } else {
        format!("{count} errors")
    }
}
