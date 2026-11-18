//! Lenslet 6.0 Workbench: 3-zone layout (global header, mode-linked sub-toolbar,
//! draggable split workspace with Calls / Access / Diff views), dedicated
//! Settings modal (`⌘,`), Run Switcher popover, and on-demand body hydration.

use std::{
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui::{
    AnyElement, ClipboardItem, Context, CursorStyle, Div, ElementId, FocusHandle, FontWeight,
    KeyDownEvent, MouseButton, MouseMoveEvent, MouseUpEvent, Render, ScrollStrategy, SharedString,
    Stateful, UniformListScrollHandle, WeakEntity, Window, actions, div, linear_color_stop,
    linear_gradient, prelude::*, px, rgb, rgba, uniform_list,
};

mod access;
mod chrome;
mod diff;
mod inspector;
mod settings;
mod stream;
mod widgets;

use self::{
    stream::{blocked_pill, new_pill},
    widgets::*,
};
use crate::{
    demo, fonts, glyph, i18n,
    prefs::{self, AccentColor, Density, Lang, ThemeMode, TimeFormat, UiPrefs},
    search::SearchInput,
    theme::{self, latency_color, status_color},
    trace::{
        self, AccessFilter, Body, CallFilter, Exchange, MAX_ROWS, RunInfo, ServiceFacet, Source,
        StatusFilter, TailEvent, Tailer, Trace, format_bytes, format_latency, full_time,
        headers_to_text, matches_search, operation_label, search_tokens, service_facets,
        short_clock, status_is_success, status_label, status_text,
    },
};

actions!(
    lenslet,
    [OpenSettings, ToggleInspector, ReloadTrace, QuitApp]
);

const START_COMMAND: &str = "lenslet run -- npm test";
const POLL_INTERVAL: Duration = Duration::from_millis(420);
const PROBE_EVERY: u64 = 5;
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);
const TOAST_MILLIS: u64 = 1_600;
const MAX_BODY_DISPLAY: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
enum TraceHealth {
    Ok,
    Missing,
    Error(String),
}

/// Primary workspace mode in the center segmented switch.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Pane {
    Calls,
    Access,
    Diff,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum InspectorTab {
    Exchange,
    Headers,
    IamMeta,
}

impl InspectorTab {
    fn label(self) -> &'static str {
        match self {
            InspectorTab::Exchange => i18n::pick("Exchange", "报文详情"),
            InspectorTab::Headers => "Headers",
            InspectorTab::IamMeta => i18n::pick("IAM & Meta", "IAM 与元数据"),
        }
    }

    fn index(self) -> &'static str {
        match self {
            InspectorTab::Exchange => "1",
            InspectorTab::Headers => "2",
            InspectorTab::IamMeta => "3",
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Counts {
    total: usize,
    retries: usize,
    errors: usize,
    blocked: usize,
    slow: usize,
    visible: usize,
}

pub struct LensletApp {
    rows: Rc<Vec<Arc<Trace>>>,
    view: Rc<Vec<usize>>,
    facets: Vec<ServiceFacet>,
    counts: Counts,
    selected: Option<usize>,
    /// Cached hydrated copy of the selected row when its body was offloaded to disk.
    hydrated_cache: Option<(String, Arc<Trace>)>,
    selection_locked: bool,

    call_filter: CallFilter,
    access_filter: AccessFilter,
    selected_access: Option<trace::Access>,
    diff_base: Option<PathBuf>,
    query: String,

    tab: InspectorTab,
    pane: Pane,
    show_all_services: bool,
    raw_body: bool,
    expand_body: bool,
    prefs: UiPrefs,

    settings_open: bool,
    runs_popover_open: bool,
    dragging_splitter: bool,
    last_saved_viewport: (u32, u32),

    search: gpui::Entity<SearchInput>,
    search_focus: FocusHandle,
    root_focus: FocusHandle,
    scroll: UniformListScrollHandle,

    paused: bool,
    buffered: Vec<Trace>,
    pending_replace: bool,

    source: Source,
    trace_path: Option<PathBuf>,
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
        let prefs = prefs::load(&home());
        i18n::set_lang(prefs.lang);
        theme::set_accent(prefs.accent);

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

        let raw_body = prefs.raw_body_default;
        let mut app = Self {
            rows: Rc::new(rows),
            view: Rc::new(Vec::new()),
            facets: Vec::new(),
            counts: Counts::default(),
            selected: if demo { Some(0) } else { None },
            hydrated_cache: None,
            selection_locked: demo,
            call_filter: CallFilter::default(),
            access_filter: AccessFilter::default(),
            selected_access: None,
            diff_base: None,
            query: String::new(),
            tab: InspectorTab::Exchange,
            pane: Pane::Calls,
            show_all_services: false,
            raw_body,
            expand_body: false,
            prefs,
            settings_open: false,
            runs_popover_open: false,
            dragging_splitter: false,
            last_saved_viewport: (0, 0),
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

    pub fn observe_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.observe_window_appearance(window, |_, _, cx| {
            cx.notify();
        })
        .detach();
        cx.observe_window_bounds(window, |this, window, _cx| {
            let vp = window.viewport_size();
            let w = vp.width.to_f64().round() as u32;
            let h = vp.height.to_f64().round() as u32;
            if (w, h) != this.last_saved_viewport && w >= 960 && h >= 640 {
                this.last_saved_viewport = (w, h);
                this.prefs.window_width = Some(w as f32);
                this.prefs.window_height = Some(h as f32);
                prefs::save(&home(), &this.prefs);
            }
        })
        .detach();
    }

    pub fn root_focus(&self) -> FocusHandle {
        self.root_focus.clone()
    }

    // -- Settings & Preferences mutators -----------------------------------

    pub(super) fn set_theme_mode(&mut self, mode: ThemeMode, cx: &mut Context<Self>) {
        self.prefs.theme = mode;
        prefs::save(&home(), &self.prefs);
        cx.notify();
    }

    pub(super) fn set_accent_color(&mut self, accent: AccentColor, cx: &mut Context<Self>) {
        self.prefs.accent = accent;
        theme::set_accent(accent);
        prefs::save(&home(), &self.prefs);
        cx.notify();
    }

    pub(super) fn set_density(&mut self, density: Density, cx: &mut Context<Self>) {
        self.prefs.density = density;
        prefs::save(&home(), &self.prefs);
        cx.notify();
    }

    pub(super) fn set_language(&mut self, lang: Lang, cx: &mut Context<Self>) {
        self.prefs.lang = lang;
        i18n::set_lang(lang);
        prefs::save(&home(), &self.prefs);
        cx.notify();
    }

    pub(super) fn set_time_format(&mut self, fmt: TimeFormat, cx: &mut Context<Self>) {
        self.prefs.time_format = fmt;
        prefs::save(&home(), &self.prefs);
        cx.notify();
    }

    pub(super) fn set_raw_body_default(&mut self, raw: bool, cx: &mut Context<Self>) {
        self.prefs.raw_body_default = raw;
        self.raw_body = raw;
        prefs::save(&home(), &self.prefs);
        cx.notify();
    }

    pub(super) fn set_word_wrap(&mut self, wrap: bool, cx: &mut Context<Self>) {
        self.prefs.word_wrap = wrap;
        prefs::save(&home(), &self.prefs);
        cx.notify();
    }

    pub(super) fn toggle_inspector(&mut self, cx: &mut Context<Self>) {
        self.prefs.inspector_open = !self.prefs.inspector_open;
        prefs::save(&home(), &self.prefs);
        cx.notify();
    }

    pub(super) fn runs_dir(&self) -> PathBuf {
        match &self.source {
            Source::Runs { dir, .. } => dir.clone(),
            Source::File(p) => p
                .parent()
                .map_or_else(|| home().join(".lenslet/runs"), Path::to_path_buf),
        }
    }

    pub(super) fn clear_historical_runs(&mut self, cx: &mut Context<Self>) {
        let dir = self.runs_dir();
        let removed = trace::clear_finished_runs(&dir, self.trace_path.as_deref());
        let msg = if i18n::is_zh() {
            format!("已清理 {removed} 个历史运行文件")
        } else {
            format!(
                "Cleared {removed} finished {}",
                if removed == 1 { "run" } else { "runs" }
            )
        };
        self.show_toast(msg, cx);
    }

    // -- Hydrated Trace Selection ------------------------------------------

    pub(super) fn selected_hydrated_trace(&mut self) -> Option<Arc<Trace>> {
        let index = self.selected?;
        let raw = self.rows.get(index)?;
        let cache_key = format!("{}:{}:{}", raw.id, raw.pending, raw.status);
        if let Some((cached_key, cached_trace)) = self.hydrated_cache.as_ref()
            && cached_key == &cache_key
        {
            return Some(cached_trace.clone());
        }
        let hydrated = match raw.hydrated() {
            std::borrow::Cow::Borrowed(_) => raw.clone(),
            std::borrow::Cow::Owned(full) => Arc::new(full),
        };
        self.hydrated_cache = Some((cache_key, hydrated.clone()));
        Some(hydrated)
    }

    pub(super) fn copy_selected_curl(&mut self, cx: &mut Context<Self>) {
        let Some(trace) = self.selected_hydrated_trace() else {
            return;
        };
        let endpoint = self
            .run
            .as_ref()
            .map(|r| r.header.endpoint.as_str())
            .filter(|e| !e.is_empty());
        let Some(curl) = trace::to_curl(&trace, endpoint) else {
            return;
        };
        self.copy_text("curl", curl, cx);
    }

    // -- Data & Filtering --------------------------------------------------

    fn refresh_view(&mut self) {
        self.facets = service_facets(self.rows.iter().map(|row| row.as_ref()));

        if let Some(svc) = self.call_filter.service.as_deref()
            && !self.facets.iter().any(|facet| facet.id == svc)
        {
            self.call_filter.service = None;
        }

        self.counts.total = self.rows.len();
        let report = self.run.as_ref().and_then(|run| run.report.as_ref());
        self.counts.retries = report
            .map(|r| r.retries)
            .filter(|retries| *retries > 0)
            .unwrap_or_else(|| self.rows.iter().filter(|row| row.is_retry()).count());
        self.counts.blocked = report
            .map(|r| r.blocked)
            .filter(|blocked| *blocked > 0)
            .unwrap_or_else(|| self.rows.iter().filter(|row| row.blocked).count());
        if self.call_filter.status == StatusFilter::Blocked && self.counts.blocked == 0 {
            self.call_filter.status = StatusFilter::All;
        }
        self.counts.errors = self.rows.iter().filter(|row| row.is_error()).count();
        self.counts.slow = self
            .rows
            .iter()
            .filter(|row| row.pending || row.ms >= 500)
            .count();

        let tokens = search_tokens(&self.query);
        let mut view = Vec::new();
        for (index, row) in self.rows.iter().enumerate() {
            if self.call_filter.matches(row) && matches_search(row, &tokens) {
                view.push(index);
            }
        }
        self.counts.visible = view.len();
        self.view = Rc::new(view);

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
        self.hydrated_cache = None;
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
                    self.diff_base = None;
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

    pub(super) fn reload_now(&mut self, cx: &mut Context<Self>) {
        if self.demo {
            self.show_toast(
                i18n::pick("Demo data is static", "演示数据为静态内容").to_string(),
                cx,
            );
            return;
        }
        self.tailer_generation += 1;
        let mut tailer = Tailer::follow(self.source.clone());
        let event = tailer.poll();
        self.skipped = tailer.skipped();
        self.tailer = Some(tailer);
        self.apply_tail(event, cx);
        self.show_toast(
            i18n::pick("Reloaded trace", "已重新加载 Trace").to_string(),
            cx,
        );
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
        self.expand_body = false;
        self.raw_body = self.prefs.raw_body_default;
        if !self.prefs.inspector_open {
            self.prefs.inspector_open = true;
        }
        window.focus(&self.root_focus);
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.pane == Pane::Access {
            let rows = self.filtered_access_rows();
            if rows.is_empty() {
                return;
            }
            let current = self.selected_access.as_ref().and_then(|sel| {
                rows.iter()
                    .position(|r| r.action == sel.action && r.resource == sel.resource)
            });
            let next = match current {
                Some(pos) => (pos as isize + delta).clamp(0, rows.len() as isize - 1) as usize,
                None => 0,
            };
            self.selected_access = Some(trace::Access {
                action: rows[next].action.clone(),
                resource: rows[next].resource.clone(),
            });
            cx.notify();
            return;
        }

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
        self.expand_body = false;
        self.raw_body = self.prefs.raw_body_default;
        self.scroll.scroll_to_item(next, ScrollStrategy::Center);
        cx.notify();
    }

    fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.search_focus);
        cx.notify();
    }

    fn clear_search_and_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.call_filter = CallFilter::default();
        self.access_filter = AccessFilter::default();
        self.search
            .update(cx, |search, cx| search.clear(window, cx));
        self.query.clear();
        self.refresh_view();
        self.reveal_selected();
        cx.notify();
    }

    fn copy_text(&mut self, label: &'static str, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.show_toast(i18n::copied_toast(label), cx);
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

        if keystroke.key == "escape" {
            if self.settings_open {
                self.settings_open = false;
                cx.notify();
                return;
            }
            if self.runs_popover_open {
                self.runs_popover_open = false;
                cx.notify();
                return;
            }
        }

        if keystroke.modifiers.secondary() {
            match keystroke.key.as_str() {
                "k" => {
                    self.focus_search(window, cx);
                    return;
                }
                "," => {
                    self.settings_open = !self.settings_open;
                    self.runs_popover_open = false;
                    cx.notify();
                    return;
                }
                "b" => {
                    self.toggle_inspector(cx);
                    return;
                }
                "r" => {
                    self.reload_now(cx);
                    return;
                }
                _ => {}
            }
        }

        if self.settings_open {
            return;
        }

        match keystroke.key.as_str() {
            "up" if !search_focused => self.move_selection(-1, cx),
            "down" if !search_focused => self.move_selection(1, cx),
            "enter" if search_focused => {
                window.focus(&self.root_focus);
                cx.notify();
            }
            "space" if !search_focused => self.toggle_pause(cx),
            "c" if !search_focused && !keystroke.modifiers.modified() => {
                self.copy_selected_curl(cx)
            }
            "[" if !search_focused => self.step_run(-1, cx),
            "]" if !search_focused => self.step_run(1, cx),
            "1" if !search_focused && !keystroke.modifiers.modified() => {
                self.set_tab(InspectorTab::Exchange, cx)
            }
            "2" if !search_focused && !keystroke.modifiers.modified() => {
                self.set_tab(InspectorTab::Headers, cx)
            }
            "3" if !search_focused && !keystroke.modifiers.modified() => {
                self.set_tab(InspectorTab::IamMeta, cx)
            }
            _ => {}
        }
    }

    pub(super) fn set_pane(&mut self, pane: Pane, cx: &mut Context<Self>) {
        if self.pane != pane {
            self.pane = pane;
            let access_mode = pane == Pane::Access;
            self.search
                .update(cx, |s, cx| s.set_access_mode(access_mode, cx));
            if access_mode && self.selected_access.is_none() {
                self.selected_access = self.access_rows().first().map(|e| trace::Access {
                    action: e.action.clone(),
                    resource: e.resource.clone(),
                });
            }
            cx.notify();
        }
    }

    pub(super) fn filter_calls_by_access(&mut self, access: trace::Access, cx: &mut Context<Self>) {
        self.call_filter.access = Some(access);
        self.set_pane(Pane::Calls, cx);
        self.selection_locked = false;
        self.refresh_view();
        self.reveal_selected();
        cx.notify();
    }

    fn run_position(&self) -> Option<(usize, usize)> {
        let Source::Runs { dir, .. } = &self.source else {
            return None;
        };
        let runs = trace::list_runs(dir);
        let current = self.trace_path.as_ref()?;
        let index = runs.iter().position(|run| run == current)?;
        Some((index + 1, runs.len()))
    }

    pub(super) fn follow_latest_run(&mut self, cx: &mut Context<Self>) {
        if self.demo {
            return;
        }
        let Source::Runs { dir, .. } = &self.source else {
            return;
        };
        self.switch_source(
            Source::Runs {
                dir: dir.clone(),
                pinned: None,
            },
            cx,
        );
    }

    pub(super) fn pin_run(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.demo {
            return;
        }
        let Source::Runs { dir, .. } = &self.source else {
            return;
        };
        self.switch_source(
            Source::Runs {
                dir: dir.clone(),
                pinned: Some(path),
            },
            cx,
        );
    }

    fn switch_source(&mut self, next_source: Source, cx: &mut Context<Self>) {
        self.source = next_source;
        self.selected = None;
        self.hydrated_cache = None;
        self.selection_locked = false;
        self.expand_body = false;
        self.call_filter = CallFilter::default();
        self.diff_base = None;
        self.tailer_generation += 1;
        let mut tailer = Tailer::follow(self.source.clone());
        let event = tailer.poll();
        self.tailer = Some(tailer);
        self.apply_tail(event, cx);
        cx.notify();
    }

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
        self.switch_source(Source::Runs { dir, pinned }, cx);
    }

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
        action_button(id, i18n::pick("Copy", "复制"))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.copy_text(label, text.clone(), cx);
            }))
            .into_any_element()
    }

    fn render_splitter(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = theme::p();
        let dragging = self.dragging_splitter;
        div()
            .id("inspector-splitter")
            .w(px(theme::SPLITTER_W))
            .flex_shrink_0()
            .cursor(CursorStyle::ResizeLeftRight)
            .bg(if dragging {
                theme::h(t.accent, 0.45)
            } else {
                theme::h(t.border, 1.0)
            })
            .hover(move |s| s.bg(theme::h(t.accent, 0.35)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.dragging_splitter = true;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
    }
}

impl Render for LensletApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        i18n::set_lang(self.prefs.lang);
        theme::set_appearance(theme::resolve(self.prefs.theme, window.appearance()));
        theme::set_accent(self.prefs.accent);
        let t = theme::p();

        let viewport_width = window.viewport_size().width.to_f64() as f32;
        let compact = viewport_width < 1_180.0;
        let show_inspector = self.prefs.inspector_open && self.pane != Pane::Diff;
        let max_insp = (viewport_width - 420.0).clamp(320.0, 780.0);
        let inspector_w = if show_inspector {
            self.prefs.clamped_inspector_width().min(max_insp)
        } else {
            0.0
        };
        let splitter_w = if show_inspector {
            theme::SPLITTER_W
        } else {
            0.0
        };

        let fixed_columns = theme::COL_TIME
            + theme::COL_SERVICE
            + theme::COL_STATUS
            + theme::COL_LATENCY
            + theme::COL_AUTH;
        let op_width = (viewport_width
            - inspector_w
            - splitter_w
            - theme::ROW_PAD_X * 2.0
            - fixed_columns
            - theme::COL_GAP * 5.0
            - 8.0)
            .max(60.0);

        let header = self.render_header(cx);
        let subbar = self.render_subbar(compact, cx);
        let strip = (self.pane == Pane::Calls && self.show_all_services)
            .then(|| self.render_service_strip(cx));

        let workspace: AnyElement = if self.pane == Pane::Diff {
            self.render_diff(cx).into_any_element()
        } else {
            let stream = self.render_stream(op_width, cx);
            let mut split = div()
                .flex()
                .flex_row()
                .flex_1()
                .min_h(px(0.0))
                .child(stream);
            if show_inspector {
                let splitter = self.render_splitter(cx);
                let inspector = self.render_inspector(inspector_w, cx);
                split = split.child(splitter).child(inspector);
            }
            split.into_any_element()
        };

        let runs_popover = self.runs_popover_open.then(|| self.render_runs_popover(cx));
        let settings_modal = self.settings_open.then(|| self.render_settings_modal(cx));
        let toast = self.render_floating_toast();

        div()
            .id("lenslet")
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(t.canvas))
            .text_color(rgb(t.text))
            .font(fonts::sans_font())
            .track_focus(&self.root_focus)
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| {
                this.settings_open = !this.settings_open;
                this.runs_popover_open = false;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleInspector, _, cx| {
                this.toggle_inspector(cx);
            }))
            .on_action(cx.listener(|this, _: &ReloadTrace, _, cx| {
                this.reload_now(cx);
            }))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, _cx| {
                    window.focus(&this.root_focus);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                if this.dragging_splitter {
                    let vw = window.viewport_size().width.to_f64() as f32;
                    let x = event.position.x.to_f64() as f32;
                    let max_w = (vw - 420.0).clamp(320.0, 780.0);
                    this.prefs.inspector_width = (vw - x).clamp(320.0, max_w);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    if this.dragging_splitter {
                        this.dragging_splitter = false;
                        prefs::save(&home(), &this.prefs);
                        cx.notify();
                    }
                }),
            )
            .child(header)
            .child(subbar)
            .children(strip)
            .child(workspace)
            .children(runs_popover)
            .children(settings_modal)
            .children(toast)
    }
}

#[derive(Clone, Copy)]
enum Side {
    Request,
    Response,
}

pub(crate) fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

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

impl LensletApp {
    fn probe_targets(&self) -> ProbeTargets {
        let Some(run) = self.run.as_ref() else {
            return ProbeTargets::default();
        };
        ProbeTargets {
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
