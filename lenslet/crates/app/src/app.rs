//! Lenslet's single-window view: stream, inspector, live tail and keyboard flow.

use std::{
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui::{
    AnyElement, ClipboardItem, Context, Div, ElementId, FocusHandle, FontWeight, KeyDownEvent,
    MouseButton, Render, ScrollStrategy, Stateful, UniformListScrollHandle, WeakEntity, Window,
    div, linear_color_stop, linear_gradient, prelude::*, px, rgb, rgba, uniform_list,
};

use crate::{
    demo, fonts, glyph,
    search::SearchInput,
    theme::{self, latency_color, status_color},
    trace::{
        self, Body, Exchange, Filter, MAX_ROWS, ServiceFacet, TailEvent, Tailer, Trace,
        format_bytes, format_latency, full_time, headers_to_text, matches_search, operation_label,
        search_tokens, service_facets, short_clock, status_is_error, status_is_success,
        status_label, status_text,
    },
};

const PROXY_HOST: &str = "127.0.0.1:9297";
const PROXY_ENDPOINT: &str = "https://127.0.0.1:9297";
const POLL_INTERVAL: Duration = Duration::from_millis(420);
const TOAST_MILLIS: u64 = 1_600;
const MAX_BODY_DISPLAY: usize = 24_000;

#[derive(Clone, Debug, PartialEq, Eq)]
enum TraceHealth {
    Ok,
    Missing,
    Error(String),
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
    show_all_services: bool,

    search: gpui::Entity<SearchInput>,
    search_focus: FocusHandle,
    root_focus: FocusHandle,
    scroll: UniformListScrollHandle,

    paused: bool,
    buffered: Vec<Trace>,
    pending_replace: bool,

    trace_path: PathBuf,
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
        let trace_path = trace_path();
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

        if demo {
            rows = demo::demo_rows().into_iter().map(Arc::new).collect();
        } else {
            let mut tail = Tailer::new(trace_path.clone());
            match tail.poll() {
                TailEvent::Replaced(found) => {
                    rows = found.into_iter().map(Arc::new).collect();
                }
                TailEvent::Missing => health = TraceHealth::Missing,
                TailEvent::Error(error) => health = TraceHealth::Error(error),
                _ => {}
            }
            skipped = tail.skipped();
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
            show_all_services: false,
            search,
            search_focus,
            root_focus,
            scroll: UniformListScrollHandle::new(),
            paused: false,
            buffered: Vec::new(),
            pending_replace: false,
            trace_path,
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
        self.counts.errors = self
            .rows
            .iter()
            .filter(|row| status_is_error(row.status))
            .count();
        self.counts.services = self.facets.len();

        let tokens = search_tokens(&self.query);
        let mut view = Vec::new();
        let mut visible_errors = 0;
        for (index, row) in self.rows.iter().enumerate() {
            if self.filter.matches(row) && matches_search(row, &tokens) {
                view.push(index);
                if status_is_error(row.status) {
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
                .map_or(true, |selected| selected + 1 >= self.rows.len())
    }

    fn apply_rows(&mut self, incoming: Vec<Trace>, replace: bool, follow: bool) {
        if replace {
            let selected_key = self
                .selected
                .and_then(|index| self.rows.get(index))
                .map(|row| row.key());
            self.rows = Rc::new(incoming.into_iter().map(Arc::new).collect());
            self.selected =
                selected_key.and_then(|key| self.rows.iter().position(|row| row.key() == key));
        } else if !incoming.is_empty() {
            let mut next: Vec<Arc<Trace>> = Vec::with_capacity(self.rows.len() + incoming.len());
            next.extend(self.rows.iter().cloned());
            next.extend(incoming.into_iter().map(Arc::new));
            if next.len() > MAX_ROWS {
                let dropped = next.len() - MAX_ROWS;
                next.drain(..dropped);
                if let Some(selected) = self.selected {
                    self.selected = if selected >= dropped {
                        Some(selected - dropped)
                    } else {
                        None
                    };
                }
            }
            self.rows = Rc::new(next);
        }

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
                    .map_or(true, |selected| selected + 1 >= self.rows.len());
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
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;

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
        let mut tailer = Tailer::new(self.trace_path.clone());
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

    // -- render -------------------------------------------------------------

    fn render_header(&self, cx: &mut Context<Self>) -> Div {
        let dot = if self.paused {
            theme::h(theme::AMBER, 0.95)
        } else {
            theme::h(theme::GREEN, 0.95)
        };
        let pause_icon = if self.paused {
            glyph::play_icon(theme::h(theme::AMBER, 0.95)).into_any_element()
        } else {
            glyph::pause_icon(theme::h(theme::TEXT_DIM, 0.95)).into_any_element()
        };

        let mut brand = div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(glyph::lens_mark(24.0))
            .child(
                div()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(theme::TEXT))
                    .child("Lenslet"),
            )
            .child(
                div()
                    .text_size(px(9.5))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child("local AWS lens"),
            );
        if self.demo {
            brand = brand.child(
                div()
                    .h(px(16.0))
                    .px(px(5.0))
                    .flex()
                    .items_center()
                    .rounded(px(3.0))
                    .border_1()
                    .border_color(theme::line(theme::AMBER, 0.35))
                    .bg(theme::h(theme::AMBER, 0.08))
                    .text_size(px(7.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(theme::AMBER))
                    .child("DEMO"),
            );
        }

        div()
            .flex()
            .items_center()
            .h(px(theme::HEADER_H))
            .px(px(14.0))
            .gap(px(14.0))
            .bg(rgb(theme::HEADER))
            .border_b_1()
            .border_color(rgb(theme::BORDER_SUBTLE))
            .child(brand)
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .h(px(26.0))
                    .px(px(9.0))
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(theme::BORDER))
                    .bg(theme::h(theme::TEXT, 0.02))
                    .child(glyph::dot(dot, 5.5))
                    .child(
                        div()
                            .font_family(fonts::mono())
                            .text_size(px(9.5))
                            .text_color(rgb(theme::TEXT_DIM))
                            .child(PROXY_HOST),
                    )
                    .child(
                        div()
                            .text_size(px(7.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(theme::TEXT_FAINT))
                            .child("TLS"),
                    ),
            )
            .child(
                div()
                    .id("pause-button")
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .h(px(26.0))
                    .px(px(9.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(if self.paused {
                        theme::line(theme::AMBER, 0.35)
                    } else {
                        theme::h(theme::BORDER, 1.0)
                    })
                    .bg(if self.paused {
                        theme::h(theme::AMBER, 0.07)
                    } else {
                        theme::h(theme::TEXT, 0.02)
                    })
                    .text_size(px(9.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if self.paused {
                        rgb(theme::AMBER)
                    } else {
                        rgb(theme::TEXT_DIM)
                    })
                    .cursor_pointer()
                    .hover(|style| {
                        style
                            .bg(theme::h(theme::TEXT, 0.06))
                            .text_color(rgb(theme::TEXT))
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_pause(cx)))
                    .child(pause_icon)
                    .child(if self.paused { "Resume" } else { "Pause" }),
            )
    }

    fn render_toolbar(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        let limit = if compact { 3 } else { 5 };
        let visible = self.visible_facets(limit);
        let hidden = self.facets.len().saturating_sub(visible.len());
        let visible_chips: Vec<Stateful<Div>> = visible
            .iter()
            .map(|facet| self.service_chip(facet, "service-chip", cx))
            .collect();
        let search_width = if compact { 248.0 } else { 320.0 };

        let mut chip_row = div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .flex_1()
            .min_w(px(0.0))
            .overflow_hidden()
            .child(self.chip(
                "filter-all",
                "All",
                self.counts.total,
                None,
                self.filter == Filter::All,
                cx,
            ))
            .child(self.chip(
                "filter-errors",
                "Errors",
                self.counts.errors,
                Some(theme::RED),
                self.filter == Filter::Errors,
                cx,
            ))
            .children(visible_chips);

        if hidden > 0 || self.show_all_services {
            chip_row = chip_row.child(
                div()
                    .id("more-services")
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .h(px(25.0))
                    .px(px(6.0))
                    .rounded(px(5.0))
                    .text_size(px(9.5))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .cursor_pointer()
                    .hover(|style| {
                        style
                            .bg(theme::h(theme::TEXT, 0.05))
                            .text_color(rgb(theme::TEXT_SOFT))
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_all_services = !this.show_all_services;
                        cx.notify();
                    }))
                    .child(if self.show_all_services {
                        "Less".to_string()
                    } else {
                        format!("More +{hidden}")
                    }),
            );
        }

        let (state_dot, state_label, state_color) = if self.demo {
            (
                theme::h(theme::AMBER, 0.95),
                if self.paused {
                    "PAUSED · DEMO".to_string()
                } else {
                    "DEMO DATA".to_string()
                },
                rgb(theme::AMBER),
            )
        } else if self.paused {
            (
                theme::h(theme::AMBER, 0.95),
                if self.buffered.is_empty() {
                    "PAUSED".to_string()
                } else {
                    format!("PAUSED · {} BUFFERED", self.buffered.len())
                },
                rgb(theme::AMBER),
            )
        } else {
            match &self.health {
                TraceHealth::Ok => (
                    theme::h(theme::GREEN, 0.95),
                    "LIVE".to_string(),
                    rgb(theme::TEXT_MUTED),
                ),
                TraceHealth::Missing => (
                    theme::h(theme::AMBER, 0.95),
                    "WAITING FOR TRACE".to_string(),
                    rgb(theme::AMBER),
                ),
                TraceHealth::Error(_) => (
                    theme::h(theme::RED, 0.95),
                    "TRACE ERROR".to_string(),
                    rgb(theme::RED),
                ),
            }
        };

        div()
            .flex()
            .items_center()
            .h(px(theme::TOOLBAR_H))
            .px(px(14.0))
            .gap(px(10.0))
            .bg(rgb(theme::TOOLBAR))
            .border_b_1()
            .border_color(rgb(theme::BORDER_SUBTLE))
            .child(div().w(px(search_width)).child(self.search.clone()))
            .child(chip_row)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .flex_shrink_0()
                    .child(glyph::dot(state_dot, 5.0))
                    .child(
                        div()
                            .font_family(fonts::mono())
                            .text_size(px(8.5))
                            .text_color(state_color)
                            .child(state_label),
                    ),
            )
    }

    fn render_service_strip(&self, cx: &mut Context<Self>) -> Div {
        let chips: Vec<Stateful<Div>> = self
            .facets
            .iter()
            .map(|facet| self.service_chip(facet, "service-strip", cx))
            .collect();

        div()
            .flex()
            .items_center()
            .h(px(32.0))
            .px(px(14.0))
            .gap(px(8.0))
            .bg(rgb(0x0B0E14))
            .border_b_1()
            .border_color(rgb(theme::BORDER_SUBTLE))
            .child(micro_label("SERVICES"))
            .child(
                div()
                    .id("services-scroll")
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_x_scroll()
                    .scrollbar_width(px(3.0))
                    .children(chips),
            )
            .child(
                action_button("close-services", "Close").on_click(cx.listener(|this, _, _, cx| {
                    this.show_all_services = false;
                    cx.notify();
                })),
            )
    }

    fn visible_facets(&self, limit: usize) -> Vec<&ServiceFacet> {
        let mut visible: Vec<&ServiceFacet> = self.facets.iter().take(limit).collect();
        if let Filter::Service(active) = &self.filter
            && !visible.iter().any(|facet| &facet.id == active)
            && let Some(facet) = self.facets.iter().find(|facet| &facet.id == active)
        {
            if !visible.is_empty() {
                visible.pop();
            }
            visible.push(facet);
        }
        visible
    }

    fn chip(
        &self,
        id: &'static str,
        label: &'static str,
        count: usize,
        dot: Option<u32>,
        active: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let error_color = dot == Some(theme::RED);
        let filter = if id == "filter-all" {
            Filter::All
        } else {
            Filter::Errors
        };
        let mut chip = div()
            .id(id)
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(5.0))
            .h(px(25.0))
            .px(px(7.0))
            .rounded(px(5.0))
            .border_1()
            .border_color(if active {
                rgb(theme::BORDER_STRONG)
            } else {
                rgba(0x00000000)
            })
            .bg(if active {
                theme::h(theme::TEXT, 0.06)
            } else {
                theme::h(0x000000, 0.0)
            })
            .text_size(px(9.5))
            .font_weight(if active {
                FontWeight::MEDIUM
            } else {
                FontWeight::NORMAL
            })
            .text_color(if active {
                rgb(theme::TEXT)
            } else {
                rgb(theme::TEXT_MUTED)
            })
            .cursor_pointer()
            .hover(|style| {
                style
                    .bg(theme::h(theme::TEXT, 0.05))
                    .text_color(rgb(theme::TEXT_SOFT))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_filter(filter.clone(), cx);
            }));

        if let Some(color) = dot {
            chip = chip.child(glyph::dot(
                theme::h(color, if error_color { 0.95 } else { 0.8 }),
                4.5,
            ));
        }
        chip = chip.child(div().child(label)).child(
            div()
                .font_family(fonts::mono())
                .text_size(px(8.0))
                .text_color(if error_color && count > 0 {
                    rgb(theme::RED)
                } else {
                    rgb(theme::TEXT_FAINT)
                })
                .child(count.to_string()),
        );
        chip
    }

    fn service_chip(
        &self,
        facet: &ServiceFacet,
        scope: &'static str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let active = self.filter == Filter::Service(facet.id.clone());
        let color = theme::service_color(&facet.label);
        div()
            .id(ElementId::Name(format!("{scope}-{}", facet.id).into()))
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(5.0))
            .h(px(25.0))
            .px(px(7.0))
            .rounded(px(5.0))
            .border_1()
            .border_color(if active {
                theme::tint(color, 0.55)
            } else {
                theme::h(0x000000, 0.0)
            })
            .bg(if active {
                theme::h(theme::TEXT, 0.06)
            } else {
                theme::h(0x000000, 0.0)
            })
            .text_size(px(9.5))
            .font_weight(if active {
                FontWeight::MEDIUM
            } else {
                FontWeight::NORMAL
            })
            .text_color(if active {
                rgb(theme::TEXT)
            } else {
                rgb(theme::TEXT_MUTED)
            })
            .cursor_pointer()
            .hover(|style| {
                style
                    .bg(theme::h(theme::TEXT, 0.05))
                    .text_color(rgb(theme::TEXT_SOFT))
            })
            .on_click(cx.listener({
                let id = facet.id.clone();
                move |this, _, _, cx| {
                    this.set_filter(Filter::Service(id.clone()), cx);
                }
            }))
            .child(glyph::dot(color, 4.5))
            .child(div().whitespace_nowrap().child(facet.label.clone()))
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(8.5))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(facet.count.to_string()),
            )
    }

    fn render_stream(&mut self, op_width: f32, cx: &mut Context<Self>) -> Div {
        let body: AnyElement = if self.view.is_empty() {
            if self.rows.is_empty() {
                self.render_first_run(cx).into_any_element()
            } else {
                self.render_no_matches(cx).into_any_element()
            }
        } else {
            self.render_rows(op_width, cx).into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .bg(rgb(theme::PANEL))
            .border_r_1()
            .border_color(rgb(theme::BORDER))
            .child(self.render_stream_head())
            .child(self.render_table_head(op_width))
            .child(body)
            .child(self.render_stream_footer())
    }

    fn render_stream_head(&self) -> Div {
        let filtered = !self.query.is_empty() || self.filter != Filter::All;
        let counts = if filtered {
            format!(
                "{} OF {} CALLS · {}",
                self.counts.visible,
                self.counts.total,
                error_count(self.counts.visible_errors)
            )
        } else {
            format!(
                "{} · {}",
                call_count(self.counts.total),
                error_count(self.counts.errors)
            )
        };

        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(theme::STREAM_HEAD_H))
            .px(px(theme::ROW_PAD_X))
            .border_b_1()
            .border_color(rgb(theme::BORDER_SUBTLE))
            .child(micro_label("REQUEST STREAM"))
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(8.0))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(counts),
            )
    }

    fn render_table_head(&self, op_width: f32) -> Div {
        div()
            .flex()
            .items_center()
            .h(px(theme::TABLE_HEAD_H))
            .px(px(theme::ROW_PAD_X))
            .gap(px(theme::COL_GAP))
            .bg(rgb(0x0C0F14))
            .border_b_1()
            .border_color(rgb(theme::BORDER_SUBTLE))
            .child(table_label("TIME", theme::COL_TIME))
            .child(table_label("SERVICE", theme::COL_SERVICE))
            .child(div().w(px(op_width)).child(micro_label("OPERATION / PATH")))
            .child(table_label("STATUS", theme::COL_STATUS))
            .child(
                div()
                    .w(px(theme::COL_LATENCY))
                    .text_right()
                    .child(micro_label("LATENCY")),
            )
            .child(
                div()
                    .w(px(theme::COL_AUTH))
                    .text_right()
                    .child(micro_label("AUTH")),
            )
    }

    fn render_rows(&mut self, op_width: f32, cx: &mut Context<Self>) -> Div {
        let rows = self.rows.clone();
        let view = self.view.clone();
        let selected = self.selected;
        let this = cx.entity().downgrade();
        let list = uniform_list("trace-rows", view.len(), move |range, _window, _cx| {
            range
                .map(|position| {
                    let raw = view[position];
                    let row = rows[raw].clone();
                    trace_row(&row, raw, selected == Some(raw), op_width, this.clone())
                })
                .collect()
        })
        .w_full()
        .flex_1()
        .min_h(px(0.0))
        .track_scroll(self.scroll.clone());

        // A short fade makes the partially visible last row read as a
        // scroll edge rather than a clipped row.
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .overflow_hidden()
            .child(list)
            .child(
                div()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .h(px(16.0))
                    .bg(linear_gradient(
                        0.0,
                        linear_color_stop(theme::h(theme::PANEL, 0.0), 0.0),
                        linear_color_stop(theme::h(theme::PANEL, 0.94), 1.0),
                    )),
            )
    }

    fn render_first_run(&mut self, cx: &mut Context<Self>) -> Div {
        let (title, description) = match &self.health {
            TraceHealth::Missing => (
                "Waiting for lenslet-proxy",
                "Point your AWS SDK at the endpoint below and start lenslet-proxy, then make a call. The trace stays on this machine.",
            ),
            TraceHealth::Error(error) => ("Can't read the trace file", error.as_str()),
            TraceHealth::Ok => (
                "No calls captured yet",
                "Point your AWS SDK at the endpoint below and start lenslet-proxy, then make a call. The trace stays on this machine; traffic from any AWS Emulator service appears here.",
            ),
        };

        let endpoint = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .h(px(30.0))
            .px(px(9.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme::BORDER))
            .bg(rgb(theme::CODE))
            .child(glyph::dot(theme::h(theme::GREEN, 0.9), 5.0))
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(10.0))
                    .text_color(rgb(theme::TEXT_SOFT))
                    .child(PROXY_ENDPOINT),
            )
            .child(self.copy_button(
                "copy-endpoint",
                "endpoint",
                Some(PROXY_ENDPOINT.to_string()),
                cx,
            ));

        div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .items_center()
            .justify_center()
            .p(px(24.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(10.0))
                    .w_full()
                    .max_w(px(430.0))
                    .child(glyph::lens_mark(38.0))
                    .child(
                        div()
                            .pt(px(2.0))
                            .text_size(px(13.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(theme::TEXT))
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .line_height(px(15.0))
                            .text_color(rgb(theme::TEXT_MUTED))
                            .text_center()
                            .child(description.to_string()),
                    )
                    .child(div().h(px(2.0)))
                    .child(endpoint)
                    .child(
                        div()
                            .font_family(fonts::mono())
                            .text_size(px(8.0))
                            .text_color(rgb(theme::TEXT_FAINT))
                            .child(format!("watching {}", display_trace_path(&self.trace_path))),
                    )
                    .when(self.demo, |this| {
                        this.child(
                            div()
                                .text_size(px(8.5))
                                .text_color(rgb(theme::AMBER))
                                .child("Demo data · LENSLET_DEMO=1"),
                        )
                    }),
            )
    }

    fn render_no_matches(&mut self, cx: &mut Context<Self>) -> Div {
        let query = if self.query.is_empty() {
            None
        } else {
            Some(format!("\"{}\"", trace::clip_utf8(&self.query, 40)))
        };
        let mut description = String::from("Try a different search");
        if let Filter::Service(id) = &self.filter {
            description = format!("No calls for the {id} filter");
        } else if self.filter == Filter::Errors {
            description = "No error calls in the current trace".to_string();
        }
        if let Some(query) = query {
            description = format!("Nothing matches {query}.");
        }

        div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .p(px(24.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(theme::TEXT_SOFT))
                    .child("No calls match"),
            )
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .text_center()
                    .child(description),
            )
            .child(
                action_button("clear-filters", "Clear search & filters").on_click(
                    cx.listener(|this, _, window, cx| this.clear_search_and_filter(window, cx)),
                ),
            )
    }

    fn render_stream_footer(&self) -> Div {
        let (dot, text) = if self.demo && !self.paused {
            (
                theme::h(theme::AMBER, 0.9),
                "demo data · static".to_string(),
            )
        } else if self.paused {
            (
                theme::h(theme::AMBER, 0.95),
                if self.buffered.is_empty() {
                    "paused".to_string()
                } else {
                    format!("paused · {} buffered", self.buffered.len())
                },
            )
        } else {
            match &self.health {
                TraceHealth::Missing => (
                    theme::h(theme::AMBER, 0.9),
                    "waiting for trace file".to_string(),
                ),
                TraceHealth::Error(error) => {
                    (theme::h(theme::RED, 0.9), format!("trace error · {error}"))
                }
                TraceHealth::Ok => (
                    theme::h(theme::GREEN, 0.9),
                    format!("watching {}", display_trace_path(&self.trace_path)),
                ),
            }
        };

        let mut left = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .flex_1()
            .min_w(px(0.0))
            .child(glyph::dot(dot, 4.5))
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(8.0))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(text),
            );
        if self.skipped > 0 {
            left = left.child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(8.0))
                    .text_color(rgb(theme::AMBER))
                    .child(format!(
                        "· {} {}",
                        self.skipped,
                        if self.skipped == 1 {
                            "bad line"
                        } else {
                            "bad lines"
                        }
                    )),
            );
        }

        div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .h(px(32.0))
            .px(px(theme::ROW_PAD_X))
            .border_t_1()
            .border_color(rgb(theme::BORDER_SUBTLE))
            .bg(rgb(0x0B0E14))
            .child(left)
    }

    fn render_inspector(&mut self, cx: &mut Context<Self>) -> Div {
        let trace = self
            .selected
            .and_then(|index| self.rows.get(index))
            .cloned();
        let body: AnyElement = if let Some(trace) = trace.as_ref() {
            self.render_inspector_body(trace, cx).into_any_element()
        } else {
            self.render_inspector_empty().into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .w(px(theme::INSPECTOR_W))
            .flex_shrink_0()
            .bg(rgb(theme::PANEL_RAISED))
            .child(if let Some(trace) = trace.as_ref() {
                self.render_inspector_header(trace, cx)
            } else {
                self.render_inspector_header_empty()
            })
            .child(body)
    }

    fn render_inspector_header_empty(&self) -> Div {
        div()
            .flex()
            .items_center()
            .h(px(theme::STREAM_HEAD_H))
            .px(px(14.0))
            .border_b_1()
            .border_color(rgb(theme::BORDER_SUBTLE))
            .child(micro_label("INSPECTOR"))
    }

    fn render_inspector_header(&self, trace: &Trace, cx: &mut Context<Self>) -> Div {
        let color = theme::service_color(trace.service_name());
        let service = div()
            .flex()
            .items_center()
            .gap(px(5.0))
            .h(px(18.0))
            .px(px(6.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(theme::tint(color, 0.30))
            .bg(theme::h(theme::TEXT, 0.03))
            .child(glyph::dot(color, 4.5))
            .child(
                div()
                    .text_size(px(8.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color)
                    .child(trace::ellipsize(trace.service_name(), 14)),
            );

        let title = div()
            .mt(px(7.0))
            .text_size(px(15.0))
            .line_height(px(19.0))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(theme::TEXT))
            .whitespace_nowrap()
            .text_ellipsis()
            .child(operation_label(trace).to_string());

        let path = div()
            .flex()
            .items_center()
            .gap(px(7.0))
            .mt(px(5.0))
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(9.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(theme::ACCENT))
                    .child(trace.method.to_ascii_uppercase()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .font_family(fonts::mono())
                    .text_size(px(10.0))
                    .text_color(rgb(theme::TEXT_DIM))
                    .whitespace_nowrap()
                    .child(trace::ellipsize(trace::display_path(trace), 48)),
            )
            .child(self.copy_button("copy-path", "path", Some(trace.path.clone()), cx));

        let status = div()
            .flex()
            .items_center()
            .gap(px(5.0))
            .child(glyph::dot(status_color(trace.status), 4.5))
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(9.0))
                    .text_color(status_color(trace.status))
                    .child(status_label(trace.status)),
            )
            .child(
                div()
                    .text_size(px(8.5))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(status_text(trace.status)),
            );

        let mut meta = div()
            .flex()
            .items_center()
            .gap(px(9.0))
            .mt(px(6.0))
            .child(status)
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(8.5))
                    .text_color(latency_color(trace.ms))
                    .child(format_latency(trace.ms)),
            )
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(8.5))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(full_time(&trace.at)),
            )
            .child(div().flex_1());
        if trace.authorization {
            meta = meta.child(glyph::auth_mark(true)).child(
                div()
                    .text_size(px(8.0))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child("sigv4"),
            );
        } else {
            meta = meta.child(glyph::auth_mark(false)).child(
                div()
                    .text_size(px(8.0))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child("no auth"),
            );
        }

        let mut top = div().flex().items_center().gap(px(8.0)).child(service);
        top = top.child(div().flex_1());
        if let Some(id) = trace.id.as_deref() {
            top = top.child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(8.0))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(format!("#{}", trace::clip_utf8(id, 18))),
            );
        }

        div()
            .flex()
            .flex_col()
            .px(px(14.0))
            .pt(px(12.0))
            .pb(px(8.0))
            .child(top)
            .child(title)
            .child(path)
            .child(meta)
            .child(self.render_tabs(cx))
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> Div {
        let mut tabs = Vec::new();
        for tab in [
            InspectorTab::Overview,
            InspectorTab::Request,
            InspectorTab::Response,
        ] {
            let active = self.tab == tab;
            tabs.push(
                div()
                    .id(ElementId::Name(format!("tab-{}", tab.index()).into()))
                    .h(px(36.0))
                    .flex()
                    .items_center()
                    .border_b_2()
                    .border_color(if active {
                        rgb(theme::ACCENT)
                    } else {
                        rgba(0x00000000)
                    })
                    .text_size(px(10.0))
                    .font_weight(if active {
                        FontWeight::MEDIUM
                    } else {
                        FontWeight::NORMAL
                    })
                    .text_color(if active {
                        rgb(theme::TEXT)
                    } else {
                        rgb(theme::TEXT_MUTED)
                    })
                    .cursor_pointer()
                    .hover(|style| style.text_color(rgb(theme::TEXT_SOFT)))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)))
                    .child(tab.label()),
            );
        }

        div()
            .flex()
            .items_end()
            .gap(px(18.0))
            .mt(px(10.0))
            .border_b_1()
            .border_color(rgb(theme::BORDER_SUBTLE))
            .children(tabs)
            .child(div().flex_1())
            .child(
                div()
                    .pb(px(5.0))
                    .font_family(fonts::mono())
                    .text_size(px(7.5))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child("1 2 3"),
            )
    }

    fn render_inspector_empty(&self) -> Div {
        div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .p(px(24.0))
            .child(glyph::lens_mark(30.0))
            .child(
                div()
                    .mt(px(2.0))
                    .text_size(px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(theme::TEXT_SOFT))
                    .child("Select a call"),
            )
            .child(
                div()
                    .text_size(px(9.5))
                    .line_height(px(14.0))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .text_center()
                    .child("Click a row, or use ↑ ↓ to move through the stream."),
            )
    }

    fn render_inspector_body(&mut self, trace: &Trace, cx: &mut Context<Self>) -> Stateful<Div> {
        let content: AnyElement = match self.tab {
            InspectorTab::Overview => self.render_overview(trace).into_any_element(),
            InspectorTab::Request => self
                .render_exchange(trace, Side::Request, cx)
                .into_any_element(),
            InspectorTab::Response => self
                .render_exchange(trace, Side::Response, cx)
                .into_any_element(),
        };

        div()
            .id(ElementId::Name(format!("inspector-{}", trace.key()).into()))
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .scrollbar_width(px(6.0))
            .child(div().p(px(14.0)).w_full().child(content))
    }

    fn render_overview(&self, trace: &Trace) -> Div {
        let request_id = trace
            .response
            .as_ref()
            .and_then(|response| {
                response
                    .header("x-amz-request-id")
                    .or_else(|| response.header("x-amzn-requestid"))
            })
            .or_else(|| {
                trace.request.as_ref().and_then(|request| {
                    request
                        .header("x-amz-request-id")
                        .or_else(|| request.header("x-amzn-requestid"))
                })
            });

        let mut call = div().flex().flex_col();
        call = call
            .child(meta_row(
                "Status",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(glyph::dot(status_color(trace.status), 4.5))
                    .child(
                        div()
                            .font_family(fonts::mono())
                            .text_size(px(9.5))
                            .text_color(status_color(trace.status))
                            .child(format!(
                                "{} {}",
                                status_label(trace.status),
                                status_text(trace.status)
                            )),
                    ),
            ))
            .child(meta_row("Duration", mono_value(format_latency(trace.ms))))
            .child(meta_row("Started", mono_value(full_time(&trace.at))))
            .child(meta_row(
                "Method",
                mono_value(trace.method.to_ascii_uppercase()),
            ));

        let mut routing = div().flex().flex_col();
        routing = routing
            .child(meta_row(
                "Service",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(glyph::dot(theme::service_color(trace.service_name()), 4.5))
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(theme::service_color(trace.service_name()))
                            .child(trace.service_name().to_string()),
                    ),
            ))
            .child(meta_row(
                "Operation",
                mono_value(trace::ellipsize(operation_label(trace), 44)),
            ))
            .child(meta_row(
                "Host",
                mono_value(if trace.host.is_empty() {
                    "—".to_string()
                } else {
                    trace.host.clone()
                }),
            ))
            .child(meta_row(
                "Request ID",
                mono_value(request_id.unwrap_or("—").to_string()),
            ));
        if let Some(id) = trace.id.as_deref() {
            routing = routing.child(meta_row("Trace ID", mono_value(id.to_string())));
        }

        let auth = if trace.authorization {
            "SigV4 authorization present"
        } else {
            "No authorization header"
        };

        let mut bodies = div().flex().flex_col();
        bodies = bodies
            .child(meta_row(
                "Auth",
                text_value(auth.to_string(), theme::TEXT_SOFT),
            ))
            .child(meta_row(
                "Request",
                text_value(body_summary(trace.request.as_ref()), theme::TEXT_BODY),
            ))
            .child(meta_row(
                "Response",
                text_value(body_summary(trace.response.as_ref()), theme::TEXT_BODY),
            ))
            .child(meta_row(
                "Content-Type",
                mono_value(
                    trace
                        .response
                        .as_ref()
                        .and_then(|response| response.content_type.clone())
                        .or_else(|| {
                            trace
                                .request
                                .as_ref()
                                .and_then(|request| request.content_type.clone())
                        })
                        .unwrap_or_else(|| "—".to_string()),
                ),
            ))
            .child(meta_row(
                "Path",
                mono_value(trace::ellipsize(trace::display_path(trace), 44)),
            ));

        div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(section("CALL", call))
            .child(section("ROUTING", routing))
            .child(section("AUTH & BODIES", bodies))
    }

    fn render_exchange(&self, trace: &Trace, side: Side, cx: &mut Context<Self>) -> Div {
        let exchange = match side {
            Side::Request => trace.request.as_ref(),
            Side::Response => trace.response.as_ref(),
        };
        let Some(exchange) = exchange else {
            return div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(theme::TEXT_SOFT))
                        .child(format!("No {} captured", side.noun())),
                )
                .child(
                    div()
                        .text_size(px(9.5))
                        .line_height(px(14.0))
                        .text_color(rgb(theme::TEXT_MUTED))
                        .child("The trace entry for this call contains metadata only."),
                );
        };

        let target = match side {
            Side::Request => format!(
                "{} {}",
                trace.method.to_ascii_uppercase(),
                trace::display_path(trace)
            ),
            Side::Response => format!(
                "HTTP {} {}",
                status_label(trace.status),
                status_text(trace.status)
            ),
        };

        let headers_text = headers_to_text(&exchange.headers);
        let headers_action = if exchange.headers.is_empty() {
            None
        } else {
            Some(self.copy_button(
                match side {
                    Side::Request => "copy-req-headers",
                    Side::Response => "copy-res-headers",
                },
                "headers",
                Some(headers_text.clone()),
                cx,
            ))
        };

        let body = exchange.body.as_ref();
        let body_text = body.and_then(|body| body.text.clone());
        let body_action = body_text
            .as_ref()
            .filter(|text| !text.is_empty())
            .map(|text| {
                self.copy_button(
                    match side {
                        Side::Request => "copy-req-body",
                        Side::Response => "copy-res-body",
                    },
                    "body",
                    Some(text.clone()),
                    cx,
                )
            });

        let mut header_meta = vec![format!(
            "{} {}",
            exchange.headers.len(),
            if exchange.headers.len() == 1 {
                "entry"
            } else {
                "entries"
            }
        )];
        if let Some(content_type) = exchange.content_type.as_deref() {
            header_meta.push(content_type.to_string());
        }

        let mut body_meta = Vec::new();
        if let Some(body) = body {
            body_meta.push(format_bytes(body.size()));
            if let Some(content_type) = exchange.content_type.as_deref() {
                body_meta.push(body_kind(content_type, body));
            }
            if body.truncated {
                body_meta.push("truncated".to_string());
            }
        }

        div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(section_with_action(
                side.target_label(),
                None,
                Some(self.copy_button(
                    match side {
                        Side::Request => "copy-req-target",
                        Side::Response => "copy-res-target",
                    },
                    match side {
                        Side::Request => "path",
                        Side::Response => "status",
                    },
                    Some(match side {
                        Side::Request => trace.path.clone(),
                        Side::Response => target.clone(),
                    }),
                    cx,
                )),
                code_block(
                    match side {
                        Side::Request => "req-target",
                        Side::Response => "res-target",
                    },
                    target.clone(),
                    68.0,
                ),
            ))
            .child(section_with_action(
                "HEADERS",
                Some(header_meta.join(" · ")),
                headers_action,
                if exchange.headers.is_empty() {
                    empty_note("No headers captured").into_any_element()
                } else {
                    code_block(
                        match side {
                            Side::Request => "req-headers",
                            Side::Response => "res-headers",
                        },
                        headers_text,
                        190.0,
                    )
                    .into_any_element()
                },
            ))
            .child(section_with_action(
                "BODY",
                Some(body_meta.join(" · ")),
                body_action,
                render_body(
                    body,
                    &exchange.content_type,
                    match side {
                        Side::Request => "req-body",
                        Side::Response => "res-body",
                    },
                ),
            ))
    }

    fn render_footer(&self, compact: bool) -> Div {
        let (proxy_dot, proxy_state) = if self.demo {
            (theme::h(theme::AMBER, 0.9), "demo")
        } else {
            match &self.health {
                TraceHealth::Ok => (theme::h(theme::GREEN, 0.9), "watching"),
                TraceHealth::Missing => (theme::h(theme::AMBER, 0.9), "waiting"),
                TraceHealth::Error(_) => (theme::h(theme::RED, 0.9), "error"),
            }
        };

        let center = if let Some((message, _)) = self.toast.as_ref() {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(glyph::dot(theme::h(theme::GREEN, 0.95), 4.5))
                .child(
                    div()
                        .font_family(fonts::mono())
                        .text_size(px(8.5))
                        .text_color(rgb(theme::GREEN))
                        .child(message.clone()),
                )
                .into_any_element()
        } else {
            div()
                .font_family(fonts::mono())
                .text_size(px(8.5))
                .text_color(rgb(theme::TEXT_MUTED))
                .child(format!(
                    "{} · {} · {} {}",
                    call_count(self.counts.total),
                    error_count(self.counts.errors),
                    self.counts.services,
                    if self.counts.services == 1 {
                        "service"
                    } else {
                        "services"
                    }
                ))
                .into_any_element()
        };

        let mut footer = div()
            .flex()
            .items_center()
            .h(px(theme::FOOTER_H))
            .px(px(14.0))
            .gap(px(14.0))
            .bg(rgb(theme::FOOTER))
            .border_t_1()
            .border_color(rgb(theme::BORDER_SUBTLE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(glyph::dot(proxy_dot, 4.5))
                    .child(
                        div()
                            .font_family(fonts::mono())
                            .text_size(px(8.0))
                            .text_color(rgb(theme::TEXT_DIM))
                            .child(format!("proxy :9297 {proxy_state}")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(glyph::dot(theme::h(theme::TEXT_FAINT, 0.55), 4.5))
                    .child(
                        div()
                            .font_family(fonts::mono())
                            .text_size(px(8.0))
                            .text_color(rgb(theme::TEXT_MUTED))
                            .child("emulator :9298"),
                    ),
            )
            .child(div().flex_1())
            .child(center)
            .child(
                div()
                    .text_size(px(8.0))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(concat!("lenslet ", env!("CARGO_PKG_VERSION"))),
            );

        footer = footer.child(
            div()
                .font_family(fonts::mono())
                .text_size(px(8.0))
                .text_color(rgb(theme::TEXT_FAINT))
                .child(if compact {
                    "↑↓ select · space pause · 1 2 3"
                } else {
                    "space pause · 1 2 3 tabs"
                }),
        );
        footer
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

fn trace_row(
    trace: &Trace,
    raw_index: usize,
    selected: bool,
    op_width: f32,
    this: WeakEntity<LensletApp>,
) -> Stateful<Div> {
    let service_color = theme::service_color(trace.service_name());
    let row = div()
        .id(ElementId::named_usize("trace-row", raw_index))
        .relative()
        .flex()
        .w_full()
        .items_center()
        .h(px(theme::ROW_H))
        .px(px(theme::ROW_PAD_X))
        .gap(px(theme::COL_GAP))
        .border_b_1()
        .border_color(rgb(theme::BORDER_SUBTLE))
        .bg(if selected {
            rgb(theme::ROW_SELECTED)
        } else {
            rgb(theme::ROW)
        })
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            this.update(cx, |app, cx| app.select_row(raw_index, window, cx))
                .ok();
        })
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(px(2.0))
                .bg(if selected {
                    rgb(theme::ACCENT)
                } else {
                    rgba(0x00000000)
                }),
        )
        .child(
            div()
                .w(px(theme::COL_TIME))
                .flex_shrink_0()
                .font_family(fonts::mono())
                .text_size(px(10.0))
                .text_color(rgb(theme::TEXT_MUTED))
                .child(short_clock(&trace.at)),
        )
        .child(
            div()
                .w(px(theme::COL_SERVICE))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(5.0))
                .overflow_hidden()
                .child(glyph::dot(service_color, 5.0))
                .child(
                    div()
                        .min_w(px(0.0))
                        .text_size(px(9.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(service_color)
                        .whitespace_nowrap()
                        .child(trace::ellipsize(trace.service_name(), 10)),
                ),
        )
        .child(
            div()
                .w(px(op_width))
                .flex_shrink_0()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .child(
                    div()
                        .text_size(px(12.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(theme::TEXT))
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .overflow_hidden()
                        .child(operation_label(trace).to_string()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .min_w(px(0.0))
                        .child(
                            div()
                                .font_family(fonts::mono())
                                .text_size(px(8.5))
                                .text_color(if status_is_success(trace.status) {
                                    rgb(theme::TEXT_MUTED)
                                } else {
                                    status_color(trace.status)
                                })
                                .child(trace.method.to_ascii_uppercase()),
                        )
                        .child(
                            div()
                                .w(px((op_width - 32.0).max(24.0)))
                                .flex_shrink_0()
                                .font_family(fonts::mono())
                                .text_size(px(10.0))
                                .text_color(rgb(theme::TEXT_MUTED))
                                .min_w(px(0.0))
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .overflow_hidden()
                                .child(trace::display_path(trace).to_string()),
                        ),
                ),
        )
        .child(
            div()
                .w(px(theme::COL_STATUS))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(5.0))
                .child(glyph::dot(status_color(trace.status), 4.5))
                .child(
                    div()
                        .font_family(fonts::mono())
                        .text_size(px(10.0))
                        .text_color(if status_is_error(trace.status) {
                            status_color(trace.status)
                        } else {
                            rgb(theme::TEXT_BODY)
                        })
                        .child(status_label(trace.status)),
                ),
        )
        .child(
            div()
                .w(px(theme::COL_LATENCY))
                .flex_shrink_0()
                .text_right()
                .font_family(fonts::mono())
                .text_size(px(10.0))
                .text_color(latency_color(trace.ms))
                .child(format_latency(trace.ms)),
        )
        .child(
            div()
                .w(px(theme::COL_AUTH))
                .flex_shrink_0()
                .flex()
                .justify_end()
                .child(glyph::auth_mark(trace.authorization)),
        );

    if selected {
        row.hover(|style| style.bg(rgb(theme::ROW_SELECTED_HOVER)))
    } else {
        row.hover(|style| style.bg(rgb(theme::ROW_HOVER)))
    }
}

fn render_body(body: Option<&Body>, content_type: &Option<String>, code_id: &'static str) -> Div {
    let Some(body) = body else {
        return empty_note("No body captured");
    };

    if body.binary {
        let mut block = div()
            .flex()
            .flex_col()
            .gap(px(5.0))
            .p(px(10.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme::BORDER))
            .bg(rgb(theme::CODE))
            .child(
                div()
                    .text_size(px(9.5))
                    .text_color(rgb(theme::TEXT_BODY))
                    .child(format!("Binary body · {}", format_bytes(body.size()))),
            )
            .child(
                div()
                    .text_size(px(9.0))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child("Binary payloads are recorded as metadata."),
            );
        if let Some(sha) = body.sha256.as_deref() {
            block = block.child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(8.5))
                    .text_color(rgb(theme::TEXT_DIM))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(format!("sha256 {sha}")),
            );
        }
        return block;
    }

    let Some(text) = body.text.as_deref() else {
        return empty_note(&format!(
            "Body text not captured · {}",
            format_bytes(body.size())
        ));
    };

    if text.is_empty() && body.size() == 0 {
        return empty_note("Empty body · 0 B");
    }

    let clipped = text.len() > MAX_BODY_DISPLAY;
    let shown = if clipped {
        trace::clip_utf8(text, MAX_BODY_DISPLAY)
    } else {
        text
    };
    let kind = body_kind(content_type.as_deref().unwrap_or(""), body);
    let mut block = div().flex().flex_col().gap(px(6.0));
    block = block.child(code_block(code_id, shown.to_string(), 300.0));
    if clipped {
        block = block.child(note_text(format!(
            "Display capped at {} of {} captured. Copy includes the full captured body.",
            format_bytes(MAX_BODY_DISPLAY),
            format_bytes(body.size())
        )));
    }
    if body.truncated {
        block = block.child(note_text(format!(
            "Truncated by the proxy · {} captured ({kind}).",
            format_bytes(body.size())
        )));
    }
    block
}

fn body_summary(exchange: Option<&Exchange>) -> String {
    let Some(exchange) = exchange else {
        return "not captured".to_string();
    };
    let Some(body) = exchange.body.as_ref() else {
        return "no body".to_string();
    };
    if body.binary {
        let mut summary = format!("binary · {}", format_bytes(body.size()));
        if body.truncated {
            summary.push_str(" · truncated");
        }
        return summary;
    }
    if body.is_empty() {
        return "empty · 0 B".to_string();
    }
    match body.text.as_deref() {
        Some(text) if !text.is_empty() => {
            let mut summary = format!(
                "{} · {}",
                body_kind(exchange.content_type.as_deref().unwrap_or(""), body),
                format_bytes(body.size())
            );
            if body.truncated {
                summary.push_str(" · truncated");
            }
            summary
        }
        _ => format!("text unavailable · {}", format_bytes(body.size())),
    }
}

fn body_kind(content_type: &str, body: &Body) -> String {
    if body.binary {
        return "binary".to_string();
    }
    let content_type = content_type.to_ascii_lowercase();
    if content_type.contains("json") {
        "JSON".to_string()
    } else if content_type.contains("xml") {
        "XML".to_string()
    } else if content_type.starts_with("text/") {
        "text".to_string()
    } else {
        "body".to_string()
    }
}

fn trace_path() -> PathBuf {
    std::env::var_os("LENSLET_TRACE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join(".lenslet/trace.jsonl")
        })
}

fn display_trace_path(path: &Path) -> String {
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

fn micro_label(text: &'static str) -> Div {
    div()
        .text_size(px(8.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text)
}

fn table_label(text: &'static str, width: f32) -> Div {
    div().w(px(width)).child(micro_label(text))
}

fn action_button(id: &'static str, label: &'static str) -> Stateful<Div> {
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
        .text_size(px(8.0))
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

fn section(title: &'static str, content: Div) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(micro_label(title))
        .child(content)
}

fn section_with_action(
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
                .text_size(px(7.5))
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

fn meta_row(label: &'static str, value: impl IntoElement) -> Div {
    div()
        .flex()
        .items_start()
        .gap(px(10.0))
        .py(px(5.0))
        .border_b_1()
        .border_color(rgb(theme::BORDER_SUBTLE))
        .child(
            div()
                .w(px(80.0))
                .flex_shrink_0()
                .text_size(px(8.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(theme::TEXT_MUTED))
                .child(label.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_size(px(9.5))
                .line_height(px(14.0))
                .text_color(rgb(theme::TEXT_SOFT))
                .overflow_hidden()
                .child(value),
        )
}

fn mono_value(text: String) -> Div {
    div()
        .font_family(fonts::mono())
        .text_size(px(9.0))
        .text_color(rgb(theme::TEXT_BODY))
        .whitespace_nowrap()
        .text_ellipsis()
        .child(text)
}

fn text_value(text: String, color: u32) -> Div {
    div().text_color(rgb(color)).child(text)
}

fn empty_note(text: &str) -> Div {
    div()
        .p(px(10.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(theme::BORDER_SUBTLE))
        .bg(rgb(theme::CODE))
        .text_size(px(9.0))
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_string())
}

fn note_text(text: String) -> Div {
    div()
        .font_family(fonts::mono())
        .text_size(px(8.0))
        .line_height(px(12.0))
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text)
}

fn code_block(id: &'static str, text: String, max_height: f32) -> Stateful<Div> {
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
        .text_size(px(10.0))
        .line_height(px(15.0))
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
