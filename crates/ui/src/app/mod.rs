//! The Lenslet window: a header (sidebar toggle, run, mode, baseline, search),
//! the collapsible Workspace Sidebar, the active list (Calls or Access) with
//! its inspector beside it, and the overlays (run switcher, baseline picker,
//! context menus, Request Composer, command palette, settings).
//!
//! The root owns only chrome state. Data lives in the [`Session`] model; the
//! lists and inspectors are child entities that re-render on their own.

use std::{path::PathBuf, process::Child, sync::Arc, time::Duration};

use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Corner, CursorStyle, Div,
    DragMoveEvent, ElementId, Entity, FocusHandle, Focusable, FontWeight, MouseButton,
    PathPromptOptions, Pixels, Point, Render, SharedString, Stateful, Task, Window, actions,
    anchored, deferred, div, point, prelude::*, px, rgb,
};

mod access;
mod bodyview;
mod calls;
mod composer;
mod inspector;
mod palette;
mod session;
mod settings;
mod sidebar;
mod widgets;

use self::{
    access::{AccessDetail, AccessEvent, AccessView},
    calls::{CallsEvent, CallsList},
    composer::{ComposerEvent, RequestComposer, render_composer_overlay},
    inspector::{Inspector, InspectorEvent},
    session::{Baseline, Health, Session, SessionEvent},
    sidebar::SIDEBAR_W,
};
use crate::{
    daemon::{self, DEFAULT_PROXY_PORT, ProxyHealth},
    feed::{FeedMode, Source},
    fonts, glyph, i18n,
    model::{Access, StatusFilter, Trace, format_bytes, search_tokens, to_curl},
    prefs::{self, AccentColor, Density, Lang, ThemeMode, UiPrefs},
    search::{SearchEvent, SearchInput},
    store::{AccessRow, policy_json, statement_json},
    theme::{self, Ui, text},
};

actions!(
    lenslet,
    [
        OpenPalette,
        FocusSearch,
        OpenSettings,
        ToggleSidebar,
        ToggleInspector,
        OpenFile,
        ExportReport,
        RevealTrace,
        NextMatch,
        PrevMatch,
        ReloadRun,
        Replay,
        OpenComposer,
        ShowCalls,
        ShowAccess,
        PrevRun,
        NextRun,
        FollowLatest,
        CopyCurl,
        CopyPolicy,
        Quit,
        SelectNext,
        SelectPrev,
        SelectFirst,
        SelectLast,
        PageDown,
        PageUp,
        Confirm,
        Cancel,
        Copy,
        SelectAll,
    ]
);

const TOAST_MILLIS: u64 = 1_800;
const PREFS_DEBOUNCE: Duration = Duration::from_millis(500);
const DAEMON_POLL: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Calls,
    Access,
}

/// Everything the window can do: palette entries and context-menu items are
/// both lists of these.
#[derive(Clone, Debug)]
pub enum Cmd {
    Mode(Mode),
    FocusSearch,
    OpenSettings,
    ToggleSidebar,
    ToggleInspector,
    OpenFile,
    ExportReport,
    RevealTrace,
    StartProxy,
    StopProxy,
    CopyProxyEnv,
    InstallCli,
    Reload,
    Replay,
    OpenComposer,
    CopyCurl(Arc<Trace>),
    CopyText(&'static str, String),
    CopyPolicy,
    FilterStatus(StatusFilter),
    FilterService(String),
    FilterAccess(Access),
    ClearFilters,
    Accept(Vec<(Access, bool)>),
    SetTestMark(Access, bool),
    PrevRun,
    NextRun,
    FollowLatest,
    PinRun(PathBuf),
    SetBaseline(Baseline),
    Theme(ThemeMode),
    Accent(AccentColor),
    Density(Density),
    Lang(Lang),
    Quit,
}

#[derive(Clone)]
pub struct MenuEntry {
    pub title: SharedString,
    pub hint: Option<String>,
    pub cmd: Cmd,
}

enum Overlay {
    Runs,
    Baseline,
    Menu(Point<Pixels>, Vec<MenuEntry>),
}

#[derive(Clone)]
struct SplitDrag;

struct DragGhost;

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

pub struct LensletApp {
    session: Entity<Session>,
    calls: Entity<CallsList>,
    inspector: Entity<Inspector>,
    access: Entity<AccessView>,
    access_detail: Entity<AccessDetail>,
    composer: Entity<RequestComposer>,
    search: Entity<SearchInput>,
    palette_input: Entity<SearchInput>,
    root_focus: FocusHandle,

    mode: Mode,
    prefs: UiPrefs,
    overlay: Option<Overlay>,
    palette_open: bool,
    palette_index: usize,
    palette_entries: Vec<MenuEntry>,
    settings_open: bool,
    composer_open: bool,
    storage: Option<(usize, u64)>,
    env_probe: ProxyHealth,
    managed_proxy: Option<Child>,
    proxy_port: u16,
    toast: Option<(String, u64)>,
    toast_seq: u64,
    prefs_seq: u64,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl Drop for LensletApp {
    fn drop(&mut self) {
        if let Some(child) = self.managed_proxy.take() {
            daemon::stop_proxy_on_exit(child, self.proxy_port);
        }
    }
}

/// A child view that keeps its last frame until it notifies. It lays out as
/// a flexible column that fills its parent's main axis.
fn cached<V: Render>(view: &Entity<V>) -> gpui::AnyView {
    gpui::AnyView::from(view.clone()).cached(
        gpui::StyleRefinement::default()
            .flex_1()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .flex()
            .flex_col(),
    )
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
        None => Source::Runs {
            dir: lenslet_core::runs::dir(&home()),
            pinned: None,
        },
    }
}

impl LensletApp {
    pub fn new(prefs: UiPrefs, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Ui::install(cx, Ui::new(&prefs, window.appearance()));
        let demo = std::env::var_os("LENSLET_DEMO").is_some_and(|v| v == "1");
        let session = cx.new(|cx| Session::new(trace_source(), demo, cx));
        let calls = cx.new(|cx| CallsList::new(session.clone(), cx));
        let inspector = cx.new(|cx| Inspector::new(session.clone(), home(), cx));
        let access = cx.new(|cx| AccessView::new(session.clone(), cx));
        let access_detail = cx.new(|cx| AccessDetail::new(session.clone(), cx));
        let composer = cx.new(|cx| RequestComposer::new(home(), cx));
        let search = cx.new(SearchInput::new);
        let palette_input = cx.new(SearchInput::new_palette);

        let mut subscriptions = vec![
            cx.subscribe(&session, |this, _, event, cx| match event {
                SessionEvent::Toast(message) => this.show_toast(message.clone(), cx),
                SessionEvent::RunChanged | SessionEvent::Runs => cx.notify(),
                SessionEvent::Derived => {
                    cx.notify();
                }
                SessionEvent::Rows(_) => {}
            }),
            cx.observe(&session, |_, _, cx| cx.notify()),
            cx.subscribe(&calls, |this, _, event, cx| match event {
                CallsEvent::Selected(call) => {
                    let call = call.clone();
                    this.inspector
                        .update(cx, |inspector, cx| inspector.set_call(call, cx));
                }
                CallsEvent::ContextMenu(position, call) => {
                    let entries = this.call_menu(call.clone(), cx);
                    this.overlay = Some(Overlay::Menu(*position, entries));
                    cx.notify();
                }
                CallsEvent::Toast(message) => this.show_toast(message.clone(), cx),
            }),
            cx.subscribe_in(
                &inspector,
                window,
                |this, _, event, window, cx| match event {
                    InspectorEvent::OpenAccess(access) => {
                        let access = access.clone();
                        this.set_mode(Mode::Access, cx);
                        this.access.update(cx, |view, cx| view.select(access, cx));
                    }
                    InspectorEvent::Select(id) => {
                        let id = id.clone();
                        this.calls
                            .update(cx, |list, cx| list.select_id(Some(id), true, cx));
                    }
                    InspectorEvent::OpenComposer(call, target) => {
                        let call = call.clone();
                        let target = target.clone();
                        this.open_composer_with(&call, target, window, cx);
                    }
                    InspectorEvent::Toast(message) => this.show_toast(message.clone(), cx),
                },
            ),
            cx.subscribe_in(
                &composer,
                window,
                |this, _, event, window, cx| match event {
                    ComposerEvent::Close => {
                        this.composer_open = false;
                        this.focus_list(window, cx);
                        cx.notify();
                    }
                    ComposerEvent::Sent(call_id, state) => {
                        let call_id = call_id.clone();
                        let state = state.clone();
                        this.inspector.update(cx, |insp, cx| {
                            insp.apply_composed_replay(call_id, state, cx);
                        });
                    }
                },
            ),
            cx.subscribe(&access, |this, _, event, cx| {
                this.on_access_event(event, cx)
            }),
            cx.subscribe(&access_detail, |this, _, event, cx| {
                this.on_access_event(event, cx)
            }),
            cx.subscribe_in(&search, window, |this, _, event, window, cx| match event {
                SearchEvent::Exit => this.focus_list(window, cx),
            }),
            cx.observe(&search, |this, search, cx| {
                let query = search.read(cx).query().to_string();
                this.apply_query(query, cx);
            }),
            cx.observe(&palette_input, |this, _, cx| {
                this.palette_index = 0;
                this.refresh_palette(cx);
            }),
            cx.observe_window_appearance(window, |this, window, cx| {
                this.restyle(window, cx);
            }),
        ];
        subscriptions.push(cx.observe_window_bounds(window, |this, window, cx| {
            let size = window.viewport_size();
            let (w, h) = (f32::from(size.width), f32::from(size.height));
            if w >= 960.0 && h >= 640.0 {
                this.prefs.window_width = Some(w.round());
                this.prefs.window_height = Some(h.round());
                this.save_prefs(cx);
            }
        }));

        let mut tasks = Vec::new();
        if !demo {
            tasks.push(Self::spawn_daemon_poller(cx));
        }

        let root_focus = cx.focus_handle();
        let app = LensletApp {
            session,
            calls,
            inspector,
            access,
            access_detail,
            composer,
            search,
            palette_input,
            root_focus,
            mode: Mode::Calls,
            prefs,
            overlay: None,
            palette_open: false,
            palette_index: 0,
            palette_entries: Vec::new(),
            settings_open: false,
            composer_open: false,
            storage: None,
            env_probe: ProxyHealth::default(),
            managed_proxy: None,
            proxy_port: DEFAULT_PROXY_PORT,
            toast: None,
            toast_seq: 0,
            prefs_seq: 0,
            _tasks: tasks,
            _subscriptions: subscriptions,
        };
        let selected = app.calls.read(cx).selected();
        app.inspector
            .update(cx, |inspector, cx| inspector.set_call(selected, cx));
        let permission = app.access.read(cx).selected();
        app.access_detail
            .update(cx, |detail, cx| detail.set_row(permission, cx));
        let focus = app.calls.focus_handle(cx);
        window.focus(&focus);
        app
    }

    fn spawn_daemon_poller(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                let Ok((port, managed)) = this.update(cx, |app, _| {
                    if let Some(child) = app.managed_proxy.as_mut()
                        && matches!(child.try_wait(), Ok(Some(_)))
                    {
                        app.managed_proxy = None;
                    }
                    (app.proxy_port, app.managed_proxy.is_some())
                }) else {
                    break;
                };
                let probe = cx
                    .background_executor()
                    .spawn(async move { daemon::probe_environment(port, managed) })
                    .await;
                let gone = this
                    .update(cx, |app, cx| {
                        if app.env_probe != probe {
                            app.env_probe = probe;
                            cx.notify();
                        }
                    })
                    .is_err();
                if gone {
                    break;
                }
                cx.background_executor().timer(DAEMON_POLL).await;
            }
        })
    }

    fn open_composer_with(
        &mut self,
        call: &Trace,
        target: crate::replay::ReplayTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer_open = true;
        self.palette_open = false;
        self.settings_open = false;
        self.overlay = None;
        self.composer
            .update(cx, |comp, cx| comp.load(call, target, window, cx));
        cx.notify();
    }

    fn on_access_event(&mut self, event: &AccessEvent, cx: &mut Context<Self>) {
        match event {
            AccessEvent::Selected(row) => {
                let row = row.clone();
                self.access_detail
                    .update(cx, |detail, cx| detail.set_row(row, cx));
            }
            AccessEvent::ContextMenu(position, row) => {
                let entries = self.access_menu(row, cx);
                self.overlay = Some(Overlay::Menu(*position, entries));
                cx.notify();
            }
            AccessEvent::FilterCalls(access) => {
                self.run_cmd_cx(Cmd::FilterAccess(access.clone()), cx);
            }
            AccessEvent::SelectCall(id) => {
                let id = id.clone();
                self.set_mode(Mode::Calls, cx);
                self.calls
                    .update(cx, |list, cx| list.select_id(Some(id), true, cx));
            }
            AccessEvent::Toast(message) => self.show_toast(message.clone(), cx),
        }
    }

    fn apply_query(&mut self, query: String, cx: &mut Context<Self>) {
        let tokens = search_tokens(&query);
        self.calls
            .update(cx, |list, cx| list.set_query(query.clone(), cx));
        self.access
            .update(cx, |view, cx| view.set_tokens(tokens.clone(), cx));
        self.inspector
            .update(cx, |inspector, cx| inspector.set_tokens(tokens, cx));
    }

    /// Appearance or accent changed: publish the new look and re-highlight.
    fn restyle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if Ui::install(cx, Ui::new(&self.prefs, window.appearance())) {
            self.inspector
                .update(cx, |inspector, cx| inspector.restyle(cx));
            self.access_detail
                .update(cx, |detail, cx| detail.restyle(cx));
            cx.refresh_windows();
        }
    }

    /// Write preferences at most every [`PREFS_DEBOUNCE`], off the main thread.
    fn save_prefs(&mut self, cx: &mut Context<Self>) {
        self.prefs_seq += 1;
        let seq = self.prefs_seq;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PREFS_DEBOUNCE).await;
            let Ok(Some(prefs)) = this.update(cx, |app, _| {
                (app.prefs_seq == seq).then(|| app.prefs.clone())
            }) else {
                return;
            };
            cx.background_executor()
                .spawn(async move { prefs::save(&home(), &prefs) })
                .await;
        })
        .detach();
    }

    fn update_prefs(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut UiPrefs),
    ) {
        f(&mut self.prefs);
        self.restyle(window, cx);
        self.save_prefs(cx);
        cx.notify();
    }

    fn show_toast(&mut self, message: String, cx: &mut Context<Self>) {
        self.toast_seq += 1;
        let seq = self.toast_seq;
        self.toast = Some((message, seq));
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(TOAST_MILLIS))
                .await;
            this.update(cx, |app, cx| {
                if app.toast.as_ref().is_some_and(|(_, s)| *s == seq) {
                    app.toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn copy(&mut self, label: &str, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        let lang = Ui::of(cx).lang;
        self.show_toast(i18n::copied(label, lang), cx);
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            self.search.update(cx, |search, cx| {
                search.set_access_mode(mode == Mode::Access, cx)
            });
            cx.notify();
        }
    }

    pub(super) fn focus_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = match self.mode {
            Mode::Calls => self.calls.focus_handle(cx),
            Mode::Access => self.access.focus_handle(cx),
        };
        window.focus(&focus);
    }

    // -- Commands -----------------------------------------------------------

    fn run_cmd_cx(&mut self, cmd: Cmd, cx: &mut Context<Self>) {
        let ui = Ui::of(cx);
        match cmd {
            Cmd::FilterAccess(access) => {
                self.set_mode(Mode::Calls, cx);
                self.calls.update(cx, |list, cx| {
                    let mut filter = list.filter.clone();
                    filter.access = Some(access);
                    list.set_filter(filter, cx);
                });
            }
            Cmd::CopyText(label, text) => self.copy(label, text, cx),
            Cmd::CopyCurl(call) => {
                let endpoint = self.session.read(cx).header_endpoint().map(str::to_string);
                match to_curl(&call, endpoint.as_deref()) {
                    Some(curl) => self.copy("curl", curl, cx),
                    None => self.show_toast(
                        ui.t(
                            "This request's body can't be rebuilt as curl",
                            "该请求体无法重建为 curl",
                        )
                        .into(),
                        cx,
                    ),
                }
            }
            Cmd::CopyPolicy => {
                let rows = self.session.read(cx).access_rows.clone();
                self.copy("policy JSON", policy_json(&rows), cx);
            }
            Cmd::FilterStatus(status) => {
                self.set_mode(Mode::Calls, cx);
                self.calls
                    .update(cx, |list, cx| list.set_status(status, cx));
            }
            Cmd::FilterService(service) => {
                self.set_mode(Mode::Calls, cx);
                self.calls
                    .update(cx, |list, cx| list.set_service(Some(service), cx));
            }
            Cmd::ClearFilters => {
                self.calls.update(cx, |list, cx| {
                    list.set_filter(crate::model::CallFilter::default(), cx)
                });
                self.access.update(cx, |view, cx| {
                    view.clear_filter(cx);
                });
            }
            Cmd::Accept(entries) => {
                let entries = entries
                    .into_iter()
                    .map(|(a, implied)| (a, false, implied))
                    .collect();
                self.session
                    .update(cx, |session, cx| session.accept(entries, cx));
            }
            Cmd::SetTestMark(access, test) => {
                self.session
                    .update(cx, |session, cx| session.set_test_mark(access, test, cx));
            }
            Cmd::Mode(mode) => self.set_mode(mode, cx),
            Cmd::ToggleSidebar => {
                self.prefs.sidebar_open = !self.prefs.sidebar_open;
                self.save_prefs(cx);
                cx.notify();
            }
            Cmd::ToggleInspector => {
                self.prefs.inspector_open = !self.prefs.inspector_open;
                self.save_prefs(cx);
                cx.notify();
            }
            Cmd::OpenFile => self.prompt_open_file(cx),
            Cmd::ExportReport => self.prompt_export_report(cx),
            Cmd::RevealTrace => {
                if let Some(path) = self.session.read(cx).path.clone() {
                    cx.reveal_path(&path);
                }
            }
            Cmd::StartProxy => self.start_managed_proxy(cx),
            Cmd::StopProxy => self.stop_managed_proxy(cx),
            Cmd::CopyProxyEnv => {
                let snippet = daemon::shell_env_snippet(self.proxy_port);
                self.copy("AWS_ENDPOINT_URL env", snippet, cx);
            }
            Cmd::InstallCli => self.install_cli(cx),
            Cmd::Reload => self.reload(cx),
            Cmd::Replay => self
                .inspector
                .update(cx, |inspector, cx| inspector.replay(cx)),
            Cmd::OpenComposer => self
                .inspector
                .update(cx, |inspector, cx| inspector.open_composer(cx)),
            Cmd::PrevRun => self.session.update(cx, |s, cx| s.step_run(-1, cx)),
            Cmd::NextRun => self.session.update(cx, |s, cx| s.step_run(1, cx)),
            Cmd::FollowLatest => self.follow_latest(cx),
            Cmd::PinRun(path) => self.pin_run(path, cx),
            Cmd::SetBaseline(baseline) => {
                self.session
                    .update(cx, |session, cx| session.set_baseline(baseline, cx));
            }
            Cmd::Quit => {
                if let Some(child) = self.managed_proxy.take() {
                    daemon::stop_proxy_on_exit(child, self.proxy_port);
                }
                cx.quit();
            }
            Cmd::OpenSettings => self.open_settings(cx),
            Cmd::FocusSearch | Cmd::Theme(_) | Cmd::Accent(_) | Cmd::Density(_) | Cmd::Lang(_) => {}
        }
    }

    pub(super) fn run_cmd(&mut self, cmd: Cmd, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = None;
        match cmd {
            Cmd::FocusSearch => {
                let focus = self.search.read(cx).handle();
                window.focus(&focus);
            }
            Cmd::Theme(mode) => self.update_prefs(window, cx, |p| p.theme = mode),
            Cmd::Accent(accent) => self.update_prefs(window, cx, |p| p.accent = accent),
            Cmd::Density(density) => self.update_prefs(window, cx, |p| p.density = density),
            Cmd::Lang(lang) => self.update_prefs(window, cx, |p| p.lang = lang),
            Cmd::Mode(mode) => {
                self.set_mode(mode, cx);
                self.focus_list(window, cx);
            }
            other => self.run_cmd_cx(other, cx),
        }
        cx.notify();
    }

    fn prompt_open_file(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        let session = self.session.clone();
        cx.spawn(async move |_, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next()
            {
                session
                    .update(cx, |s, cx| s.switch(Source::File(path), cx))
                    .ok();
            }
        })
        .detach();
    }

    fn prompt_export_report(&mut self, cx: &mut Context<Self>) {
        let session = self.session.read(cx);
        let report = session
            .run
            .as_ref()
            .and_then(|r| r.report.clone())
            .unwrap_or_else(|| {
                let run_id = session
                    .run
                    .as_ref()
                    .map(|r| r.header.run.clone())
                    .unwrap_or_default();
                session.store.live_report(&run_id)
            });
        let Ok(json) = serde_json::to_string_pretty(&report) else {
            return;
        };
        let dir = session
            .run
            .as_ref()
            .filter(|r| !r.header.cwd.is_empty())
            .map(|r| PathBuf::from(&r.header.cwd))
            .unwrap_or_else(home);
        let lang = Ui::of(cx).lang;
        let rx = cx.prompt_for_new_path(&dir, Some("lenslet-report.json"));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = rx.await {
                let target = path.clone();
                let res = cx
                    .background_executor()
                    .spawn(async move {
                        crate::fsguard::assert_io_allowed();
                        std::fs::write(&target, json)
                    })
                    .await;
                this.update(cx, |app, cx| {
                    let msg = match (res, lang) {
                        (Ok(()), Lang::En) => format!("Saved {}", path.display()),
                        (Ok(()), Lang::Zh) => format!("已保存 {}", path.display()),
                        (Err(e), _) => format!("Save failed: {e}"),
                    };
                    app.show_toast(msg, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn start_managed_proxy(&mut self, cx: &mut Context<Self>) {
        if self.managed_proxy.is_some() || self.env_probe.running {
            return;
        }
        let port = self.proxy_port;
        let cwd = self
            .session
            .read(cx)
            .run
            .as_ref()
            .filter(|r| !r.header.cwd.is_empty())
            .map(|r| PathBuf::from(&r.header.cwd));
        let lang = Ui::of(cx).lang;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { daemon::spawn_proxy(port, false, cwd.as_deref()) })
                .await;
            this.update(cx, |app, cx| match res {
                Ok(child) => {
                    app.managed_proxy = Some(child);
                    app.env_probe.running = true;
                    app.env_probe.managed = true;
                    app.follow_latest(cx);
                    let msg = match lang {
                        Lang::En => format!("Proxy listening on http://127.0.0.1:{port}"),
                        Lang::Zh => format!("本地代理已启动：http://127.0.0.1:{port}"),
                    };
                    app.show_toast(msg, cx);
                }
                Err(err) => app.show_toast(err, cx),
            })
            .ok();
        })
        .detach();
    }

    fn stop_managed_proxy(&mut self, cx: &mut Context<Self>) {
        if let Some(child) = self.managed_proxy.take() {
            let port = self.proxy_port;
            let lang = Ui::of(cx).lang;
            cx.background_executor()
                .spawn(async move {
                    daemon::stop_proxy_gracefully(child, port);
                })
                .detach();
            self.env_probe.running = false;
            self.env_probe.managed = false;
            let msg = match lang {
                Lang::En => "Stopped managed local proxy".to_string(),
                Lang::Zh => "已停止托管本地代理".to_string(),
            };
            self.show_toast(msg, cx);
        }
    }

    fn install_cli(&mut self, cx: &mut Context<Self>) {
        let h = home();
        let lang = Ui::of(cx).lang;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { daemon::install_cli_to_path(&h) })
                .await;
            this.update(cx, |app, cx| {
                let msg = match (res, lang) {
                    (Ok(dest), Lang::En) => format!("Installed `lenslet` to {}", dest.display()),
                    (Ok(dest), Lang::Zh) => format!("已将 `lenslet` 安装至 {}", dest.display()),
                    (Err(err), _) => err,
                };
                app.show_toast(msg, cx);
            })
            .ok();
        })
        .detach();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        if self.session.read(cx).demo {
            let ui = Ui::of(cx);
            self.show_toast(ui.t("Demo data is static", "演示数据为静态快照").into(), cx);
            return;
        }
        self.session.update(cx, |session, cx| session.reload(cx));
    }

    fn follow_latest(&mut self, cx: &mut Context<Self>) {
        let dir = self
            .session
            .read(cx)
            .source
            .runs_dir()
            .map(|d| d.to_path_buf());
        if let Some(dir) = dir {
            self.session.update(cx, |session, cx| {
                session.switch(Source::Runs { dir, pinned: None }, cx)
            });
        }
    }

    fn pin_run(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let dir = self
            .session
            .read(cx)
            .source
            .runs_dir()
            .map(|d| d.to_path_buf());
        let newest = self
            .session
            .read(cx)
            .runs
            .first()
            .map(|run| run.path.clone());
        if let Some(dir) = dir {
            let pinned = (newest.as_ref() != Some(&path)).then_some(path);
            self.session.update(cx, |session, cx| {
                session.switch(Source::Runs { dir, pinned }, cx)
            });
        } else {
            self.session
                .update(cx, |session, cx| session.switch(Source::File(path), cx));
        }
    }

    pub(super) fn open_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_open = true;
        self.composer_open = false;
        self.overlay = None;
        self.palette_open = false;
        self.storage = None;
        if let Some(dir) = self.session.read(cx).runs_dir() {
            cx.spawn(async move |this, cx| {
                let stats = cx
                    .background_executor()
                    .spawn(async move { crate::feed::storage_stats(&dir) })
                    .await;
                this.update(cx, |app, cx| {
                    app.storage = Some(stats);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    fn call_menu(&self, call: Arc<Trace>, cx: &App) -> Vec<MenuEntry> {
        let ui = Ui::of(cx);
        let mut entries = vec![MenuEntry {
            title: ui.t("Copy as curl", "复制为 curl").into(),
            hint: Some(i18n::shortcut("⇧C")),
            cmd: Cmd::CopyCurl(call.clone()),
        }];
        if let Some(first) = call.access.first().or(call.implied.first()) {
            entries.push(MenuEntry {
                title: ui.t("Copy permission", "复制权限").into(),
                hint: None,
                cmd: Cmd::CopyText("permission", format!("{} {}", first.action, first.resource)),
            });
            entries.push(MenuEntry {
                title: ui.t("Copy resource ARN", "复制资源 ARN").into(),
                hint: None,
                cmd: Cmd::CopyText("ARN", first.resource.clone()),
            });
        }
        entries.push(MenuEntry {
            title: ui.t("Copy call id", "复制调用 ID").into(),
            hint: None,
            cmd: Cmd::CopyText("id", call.id.clone()),
        });
        if self.inspector.read(cx).can_replay(cx) {
            entries.push(MenuEntry {
                title: ui.t("Replay", "重放").into(),
                hint: Some(i18n::shortcut("⏎")),
                cmd: Cmd::Replay,
            });
            entries.push(MenuEntry {
                title: ui.t("Edit & Replay…", "编辑并重放…").into(),
                hint: Some(i18n::shortcut("⇧⏎")),
                cmd: Cmd::OpenComposer,
            });
        }
        entries.push(MenuEntry {
            title: format!("{} {}", ui.t("Only", "仅显示"), call.service_name()).into(),
            hint: None,
            cmd: Cmd::FilterService(call.service_key().to_string()),
        });
        entries
    }

    fn access_menu(&self, row: &AccessRow, cx: &App) -> Vec<MenuEntry> {
        let ui = Ui::of(cx);
        let editable = self.session.read(cx).can_edit_lock();
        let mut entries = Vec::new();
        if editable && row.used && !row.in_lock {
            entries.push(MenuEntry {
                title: ui.t("Accept into lock", "写入 lock").into(),
                hint: None,
                cmd: Cmd::Accept(vec![(row.access.clone(), row.implied)]),
            });
        }
        if editable {
            entries.push(MenuEntry {
                title: if row.test {
                    ui.t("Unmark # test", "取消 # test").into()
                } else {
                    ui.t("Mark # test", "标记 # test").into()
                },
                hint: None,
                cmd: Cmd::SetTestMark(row.access.clone(), !row.test),
            });
        }
        entries.push(MenuEntry {
            title: ui.t("Copy statement JSON", "复制 Statement JSON").into(),
            hint: None,
            cmd: Cmd::CopyText("statement", statement_json(&row.access)),
        });
        entries.push(MenuEntry {
            title: ui.t("Copy resource ARN", "复制资源 ARN").into(),
            hint: None,
            cmd: Cmd::CopyText("ARN", row.access.resource.clone()),
        });
        if row.used {
            entries.push(MenuEntry {
                title: ui.t("Show calls", "查看调用").into(),
                hint: None,
                cmd: Cmd::FilterAccess(row.access.clone()),
            });
        }
        entries
    }

    // -- Keyboard -------------------------------------------------------------

    fn select(&mut self, f: impl FnOnce(SelectTarget, &mut App), cx: &mut Context<Self>) {
        if self.settings_open || self.composer_open {
            return;
        }
        match self.mode {
            Mode::Calls => f(SelectTarget::Calls(self.calls.clone()), cx),
            Mode::Access => f(SelectTarget::Access(self.access.clone()), cx),
        }
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay.take().is_some() {
            cx.notify();
            return;
        }
        if self.composer_open {
            self.composer_open = false;
            self.focus_list(window, cx);
            cx.notify();
            return;
        }
        if self.settings_open {
            self.settings_open = false;
            cx.notify();
            return;
        }
        let inspector_focused = self.inspector.focus_handle(cx).contains_focused(window, cx)
            || self
                .inspector
                .read(cx)
                .body()
                .focus_handle(cx)
                .is_focused(window);
        if inspector_focused {
            self.focus_list(window, cx);
            cx.notify();
            return;
        }
        let cleared = match self.mode {
            Mode::Calls => self.calls.update(cx, |list, cx| list.clear_one_filter(cx)),
            Mode::Access => self.access.update(cx, |view, cx| view.clear_filter(cx)),
        };
        if !cleared && !self.search.read(cx).query().is_empty() {
            self.search
                .update(cx, |search, cx| search.clear(window, cx));
            self.focus_list(window, cx);
        }
    }
}

enum SelectTarget {
    Calls(Entity<CallsList>),
    Access(Entity<AccessView>),
}

impl SelectTarget {
    fn step(self, delta: isize, cx: &mut App) {
        match self {
            SelectTarget::Calls(list) => list.update(cx, |l, cx| l.move_by(delta, cx)),
            SelectTarget::Access(view) => view.update(cx, |v, cx| v.move_by(delta, cx)),
        }
    }

    fn page(self, down: bool, cx: &mut App) {
        match self {
            SelectTarget::Calls(list) => list.update(cx, |l, cx| l.page(down, cx)),
            SelectTarget::Access(view) => view.update(cx, |v, cx| v.page(down, cx)),
        }
    }

    fn edge(self, last: bool, cx: &mut App) {
        match self {
            SelectTarget::Calls(list) => list.update(cx, |l, cx| l.select_edge(last, cx)),
            SelectTarget::Access(view) => view.update(cx, |v, cx| v.select_edge(last, cx)),
        }
    }
}

// -- Render ----------------------------------------------------------------------

impl LensletApp {
    fn render_header(&self, ui: &Ui, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = ui.p;
        let session = self.session.read(cx);
        let mac = cfg!(target_os = "macos");
        let mode = self.mode;
        let entity = cx.entity();
        let modes = widgets::segmented(
            ui,
            vec![
                widgets::Segment {
                    id: "mode-calls",
                    label: ui.t("Calls", "请求").into(),
                    active: mode == Mode::Calls,
                    tooltip: Some((
                        ui.t("Every AWS call", "全部 AWS 调用").into(),
                        Some(i18n::shortcut("1")),
                    )),
                },
                widgets::Segment {
                    id: "mode-access",
                    label: ui.t("Access", "权限").into(),
                    active: mode == Mode::Access,
                    tooltip: Some((
                        ui.t("IAM permissions the run needed", "本次运行所需的 IAM 权限")
                            .into(),
                        Some(i18n::shortcut("2")),
                    )),
                },
            ],
            move |index, window, cx| {
                let mode = if index == 0 {
                    Mode::Calls
                } else {
                    Mode::Access
                };
                entity.update(cx, |app, cx| app.run_cmd(Cmd::Mode(mode), window, cx));
            },
        );

        let compact = window.viewport_size().width < px(1_180.0);
        div()
            .id("titlebar")
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(10.0))
            .h(px(theme::HEADER_H))
            .pl(px(if mac { 80.0 } else { 12.0 }))
            .pr(px(8.0))
            .bg(rgb(t.header))
            .border_b_1()
            .border_color(rgb(t.border))
            .child(glyph::lens_mark(&t, 20.0))
            .child(
                widgets::icon_button(
                    ui,
                    "toggle-sidebar",
                    self.prefs.sidebar_open,
                    glyph::left_sidebar_icon(theme::h(t.text_dim, 1.0), self.prefs.sidebar_open),
                )
                .tooltip(widgets::tip(
                    ui.t("Workspace Sidebar", "工作区侧栏"),
                    Some(i18n::shortcut("0")),
                ))
                .on_click(
                    cx.listener(|app, _, window, cx| app.run_cmd(Cmd::ToggleSidebar, window, cx)),
                ),
            )
            .child(self.render_run_button(ui, session, cx))
            .child(modes)
            .child(div().flex_1())
            .child(self.render_baseline_button(ui, session, cx))
            .child(
                div()
                    .w(px(if compact { 196.0 } else { 290.0 }))
                    .child(self.search.clone()),
            )
            .child(
                widgets::icon_button(
                    ui,
                    "palette",
                    self.palette_open,
                    div()
                        .px(px(5.0))
                        .py(px(1.0))
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(rgb(t.border))
                        .font(fonts::mono_font())
                        .text_size(px(10.5))
                        .text_color(theme::h(t.text_dim, 1.0))
                        .child(if cfg!(target_os = "macos") {
                            "⌘K"
                        } else {
                            "^K"
                        }),
                )
                .w_auto()
                .px(px(4.0))
                .tooltip(widgets::tip(
                    ui.t("Commands", "命令"),
                    Some(i18n::shortcut("K")),
                ))
                .on_click(cx.listener(|app, _, window, cx| app.toggle_palette(window, cx))),
            )
            .child(
                widgets::icon_button(
                    ui,
                    "toggle-inspector",
                    self.prefs.inspector_open,
                    glyph::sidebar_icon(theme::h(t.text_dim, 1.0), self.prefs.inspector_open),
                )
                .tooltip(widgets::tip(
                    ui.t("Inspector", "详情栏"),
                    Some(i18n::shortcut("B")),
                ))
                .on_click(
                    cx.listener(|app, _, window, cx| app.run_cmd(Cmd::ToggleInspector, window, cx)),
                ),
            )
            .child(
                widgets::icon_button(
                    ui,
                    "settings",
                    self.settings_open,
                    glyph::gear_icon(theme::h(t.text_dim, 1.0)),
                )
                .tooltip(widgets::tip(
                    ui.t("Settings", "设置"),
                    Some(i18n::shortcut(",")),
                ))
                .on_click(cx.listener(|app, _, _, cx| app.open_settings(cx))),
            )
    }

    fn render_run_button(&self, ui: &Ui, session: &Session, cx: &Context<Self>) -> Div {
        let t = ui.p;
        let (title, status, status_color) = match (&session.health, session.run.as_ref()) {
            (Health::Loading, _) => (
                ui.t("Loading…", "加载中…").to_string(),
                String::new(),
                t.text_muted,
            ),
            (Health::Missing, _) => (
                ui.t("No runs yet", "还没有运行").to_string(),
                String::new(),
                t.text_muted,
            ),
            (Health::Error(_), _) => (
                ui.t("Can't read run", "无法读取运行").to_string(),
                String::new(),
                t.red,
            ),
            (Health::Ready, Some(run)) => {
                let title = if run.header.command.is_empty() {
                    ui.t("proxy session", "代理会话").to_string()
                } else {
                    run.command_line()
                };
                match (&run.report, session.feed_mode) {
                    (Some(report), _) => match report.exit_code {
                        Some(0) => (title, "exit 0".into(), t.green),
                        Some(code) => (title, format!("exit {code}"), t.red),
                        None => (title, ui.t("ended", "已结束").into(), t.text_muted),
                    },
                    (None, Some(FeedMode::Push)) => (title, ui.t("live", "实时").into(), t.green),
                    (None, Some(FeedMode::Tail)) => {
                        (title, ui.t("live · tail", "实时 · 轮询").into(), t.amber)
                    }
                    (None, _) => (title, ui.t("unfinished", "未结束").into(), t.amber),
                }
            }
            (Health::Ready, None) => (
                ui.t("Demo", "演示").to_string(),
                String::new(),
                t.text_muted,
            ),
        };
        let position = session
            .run_position()
            .map(|(i, n)| format!("{i}/{n}"))
            .unwrap_or_default();
        let open = matches!(self.overlay, Some(Overlay::Runs));
        let button = div()
            .id("run-switcher")
            .flex()
            .items_center()
            .gap(px(8.0))
            .h(px(28.0))
            .max_w(px(380.0))
            .px(px(8.0))
            .rounded(px(6.0))
            .when(open, |el| el.bg(theme::h(t.text, 0.08)))
            .cursor_pointer()
            .hover(move |s| s.bg(theme::h(t.text, 0.06)))
            .tooltip(widgets::tip(
                ui.t("Switch run", "切换运行"),
                Some(format!("{} / {}", i18n::shortcut("["), i18n::shortcut("]"))),
            ))
            .on_click(cx.listener(|app, _, _, cx| {
                app.overlay = match app.overlay {
                    Some(Overlay::Runs) => None,
                    _ => {
                        app.session.update(cx, |s, cx| s.refresh_runs(cx));
                        Some(Overlay::Runs)
                    }
                };
                cx.notify();
            }))
            .when(!status.is_empty(), |el| {
                el.child(glyph::dot(theme::h(status_color, 1.0), 7.0))
            })
            .child(
                div()
                    .min_w(px(0.0))
                    .font(fonts::mono_font())
                    .text_size(px(text::MD))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(t.text))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(title),
            )
            .when(!status.is_empty(), |el| {
                el.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(text::SM))
                        .text_color(rgb(status_color))
                        .child(status),
                )
            })
            .when(!position.is_empty(), |el| {
                el.child(
                    div()
                        .flex_shrink_0()
                        .font(fonts::mono_font())
                        .text_size(px(text::XS))
                        .text_color(rgb(t.text_muted))
                        .child(position),
                )
            })
            .child(glyph::chevron_down(theme::h(t.text_muted, 1.0)));
        let mut wrapper = div().relative().flex_shrink().min_w(px(0.0)).child(button);
        if open {
            wrapper = wrapper.child(deferred(
                anchored()
                    .anchor(Corner::TopLeft)
                    .offset(point(px(0.0), px(32.0)))
                    .snap_to_window_with_margin(px(8.0))
                    .child(self.render_runs_popover(ui, session, cx)),
            ));
        }
        wrapper
    }

    fn render_runs_popover(&self, ui: &Ui, session: &Session, cx: &Context<Self>) -> AnyElement {
        let t = ui.p;
        let mut card = widgets::popover_card(ui, "runs-popover", 460.0)
            .max_h(px(420.0))
            .overflow_y_scroll()
            .on_mouse_down_out(cx.listener(|app, _, _, cx| {
                app.overlay = None;
                cx.notify();
            }));
        card = card.child(
            widgets::menu_item(
                ui,
                "open-trace-file",
                ui.t("Open Trace File…", "打开追踪文件…"),
                Some(i18n::shortcut("O")),
                false,
                false,
            )
            .on_click(cx.listener(|app, _, window, cx| app.run_cmd(Cmd::OpenFile, window, cx))),
        );
        if session.source.runs_dir().is_some() {
            card = card.child(
                widgets::menu_item(
                    ui,
                    "follow-latest",
                    ui.t("Follow the newest run", "跟随最新运行"),
                    None,
                    session.source.follows_latest(),
                    false,
                )
                .on_click(
                    cx.listener(|app, _, window, cx| app.run_cmd(Cmd::FollowLatest, window, cx)),
                ),
            );
        }
        card = card.child(widgets::separator(ui));
        if session.runs.is_empty() {
            card = card.child(
                div()
                    .p(px(12.0))
                    .text_size(px(text::MD))
                    .text_color(rgb(t.text_muted))
                    .child(ui.t("No recorded runs", "没有运行记录")),
            );
        }
        for (index, run) in session.runs.iter().take(40).enumerate() {
            let current = session.path.as_ref() == Some(&run.path);
            let (status, color) = match run.report.as_ref() {
                Some(report) => match report.exit_code {
                    Some(0) => (ui.t("pass", "通过").to_string(), t.green),
                    Some(code) => (format!("exit {code}"), t.red),
                    None => (ui.t("ended", "已结束").into(), t.text_muted),
                },
                None => (ui.t("live", "实时").into(), t.amber),
            };
            let calls = run
                .report
                .as_ref()
                .map(|r| r.calls.to_string())
                .unwrap_or_default();
            let path = run.path.clone();
            let pin_prefix = if run.pinned { "★ " } else { "" };
            card = card.child(
                div()
                    .id(ElementId::named_usize("run", index))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .h(px(36.0))
                    .flex_shrink_0()
                    .px(px(8.0))
                    .rounded(px(5.0))
                    .when(current, |el| el.bg(theme::h(t.accent, 0.14)))
                    .cursor_pointer()
                    .hover(move |s| s.bg(theme::h(t.accent, 0.10)))
                    .on_click(cx.listener(move |app, _, window, cx| {
                        app.run_cmd(Cmd::PinRun(path.clone()), window, cx)
                    }))
                    .child(glyph::dot(theme::h(color, 1.0), 6.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .font(fonts::mono_font())
                                    .text_size(px(text::MD))
                                    .text_color(rgb(t.text))
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(format!("{pin_prefix}{}", run.label())),
                            )
                            .child(
                                div()
                                    .font(fonts::mono_font())
                                    .text_size(px(text::XS))
                                    .text_color(rgb(t.text_muted))
                                    .child(format!(
                                        "{} · {} · {}",
                                        run.project_name(),
                                        run.id,
                                        format_bytes(run.bytes as usize)
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .font(fonts::mono_font())
                            .text_size(px(text::SM))
                            .text_color(rgb(t.text_muted))
                            .child(calls),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .w(px(48.0))
                            .text_right()
                            .text_size(px(text::SM))
                            .text_color(rgb(color))
                            .child(status),
                    ),
            );
        }
        widgets::appear(ui, "runs-popover-anim", card)
    }

    fn render_baseline_button(&self, ui: &Ui, session: &Session, cx: &Context<Self>) -> Div {
        let t = ui.p;
        let label: String = match (&session.baseline, session.baseline_run.as_ref()) {
            (Baseline::Lock, _) if session.lock_path.is_some() && session.lock.is_none() => ui
                .t("lenslet.lock (none yet)", "lenslet.lock（尚无）")
                .into(),
            (Baseline::Lock, _) => "lenslet.lock".into(),
            (Baseline::Previous, Some(_)) => ui.t("previous run", "上一次运行").into(),
            (Baseline::Previous, None) => ui.t("previous run (none)", "上一次运行（无）").into(),
            (Baseline::Run(path), _) => lenslet_core::runs::run_id(path),
        };
        let summary: Option<(String, u32)> = match session.diff.as_ref() {
            Some(diff) => {
                let mut parts = Vec::new();
                if !diff.new_failures.is_empty() {
                    parts.push(if ui.zh() {
                        format!("{} 新增失败", diff.new_failures.len())
                    } else {
                        format!("{} new fail", diff.new_failures.len())
                    });
                }
                if !diff.fixed_failures.is_empty() {
                    parts.push(if ui.zh() {
                        format!("{} 已修复", diff.fixed_failures.len())
                    } else {
                        format!("{} fixed", diff.fixed_failures.len())
                    });
                }
                if !diff.access_added.is_empty() {
                    parts.push(if ui.zh() {
                        format!("+{} 新增权限", diff.access_added.len())
                    } else {
                        format!("+{} access", diff.access_added.len())
                    });
                }
                if !diff.access_removed.is_empty() {
                    parts.push(format!("−{}", diff.access_removed.len()));
                }
                (!parts.is_empty()).then(|| {
                    (
                        parts.join(" · "),
                        if diff.new_failures.is_empty() {
                            t.amber
                        } else {
                            t.red
                        },
                    )
                })
            }
            None => {
                let added = session
                    .access_rows
                    .iter()
                    .filter(|row| row.delta == crate::store::AccessDelta::Added)
                    .count();
                (added > 0).then(|| (i18n::not_in_lock_badge(added, ui.lang), t.amber))
            }
        };
        let open = matches!(self.overlay, Some(Overlay::Baseline));
        let button = div()
            .id("baseline")
            .flex()
            .items_center()
            .gap(px(6.0))
            .h(px(28.0))
            .px(px(8.0))
            .rounded(px(6.0))
            .when(open, |el| el.bg(theme::h(t.text, 0.08)))
            .cursor_pointer()
            .hover(move |s| s.bg(theme::h(t.text, 0.06)))
            .tooltip(widgets::tip(
                ui.t(
                    "What this run is compared with: badges, the Changed filter and Access +/−",
                    "本次运行的对比基线：徽标、“有变化”筛选与权限 +/−",
                ),
                None,
            ))
            .on_click(cx.listener(|app, _, _, cx| {
                app.overlay = match app.overlay {
                    Some(Overlay::Baseline) => None,
                    _ => Some(Overlay::Baseline),
                };
                cx.notify();
            }))
            .child(
                div()
                    .text_size(px(text::SM))
                    .text_color(rgb(t.text_muted))
                    .child(ui.t("Compared with", "对比")),
            )
            .child(
                div()
                    .font(fonts::mono_font())
                    .text_size(px(text::SM))
                    .text_color(rgb(t.text_soft))
                    .child(label),
            )
            .when_some(summary, |el, (summary, color)| {
                el.child(
                    div()
                        .font(fonts::mono_font())
                        .text_size(px(text::XS))
                        .text_color(rgb(color))
                        .child(summary),
                )
            })
            .child(glyph::chevron_down(theme::h(t.text_muted, 1.0)));
        let mut wrapper = div().relative().flex_shrink_0().child(button);
        if open {
            let mut card = widgets::popover_card(ui, "baseline-popover", 320.0)
                .max_h(px(360.0))
                .overflow_y_scroll()
                .on_mouse_down_out(cx.listener(|app, _, _, cx| {
                    app.overlay = None;
                    cx.notify();
                }));
            let mut options = vec![
                (Baseline::Lock, "lenslet.lock".to_string()),
                (
                    Baseline::Previous,
                    ui.t("Previous run", "上一次运行").to_string(),
                ),
            ];
            for run in session
                .runs
                .iter()
                .filter(|r| r.ended() && Some(&r.path) != session.path.as_ref())
                .take(12)
            {
                options.push((
                    Baseline::Run(run.path.clone()),
                    format!("{} · {}", run.id, run.label()),
                ));
            }
            for (index, (baseline, title)) in options.into_iter().enumerate() {
                let selected = session.baseline == baseline;
                card = card.child(
                    widgets::menu_item(
                        ui,
                        ElementId::named_usize("baseline-opt", index),
                        title,
                        None,
                        selected,
                        false,
                    )
                    .on_click(cx.listener(move |app, _, window, cx| {
                        app.run_cmd(Cmd::SetBaseline(baseline.clone()), window, cx)
                    })),
                );
                if index == 1 {
                    card = card.child(widgets::separator(ui));
                }
            }
            wrapper = wrapper.child(deferred(
                anchored()
                    .anchor(Corner::TopRight)
                    .offset(point(px(0.0), px(32.0)))
                    .snap_to_window_with_margin(px(8.0))
                    .child(widgets::appear(ui, "baseline-anim", card)),
            ));
        }
        wrapper
    }

    fn render_menu(
        &self,
        ui: &Ui,
        position: Point<Pixels>,
        entries: &[MenuEntry],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut card = widgets::popover_card(ui, "context-menu", 240.0).on_mouse_down_out(
            cx.listener(|app, _, _, cx| {
                app.overlay = None;
                cx.notify();
            }),
        );
        for (index, entry) in entries.iter().enumerate() {
            let cmd = entry.cmd.clone();
            card = card.child(
                widgets::menu_item(
                    ui,
                    ElementId::named_usize("menu", index),
                    entry.title.clone(),
                    entry.hint.clone(),
                    false,
                    false,
                )
                .on_click(
                    cx.listener(move |app, _, window, cx| app.run_cmd(cmd.clone(), window, cx)),
                ),
            );
        }
        deferred(
            anchored()
                .position(position)
                .snap_to_window_with_margin(px(8.0))
                .child(widgets::appear(ui, "menu-anim", card)),
        )
        .with_priority(2)
        .into_any_element()
    }

    fn render_splitter(&self, ui: &Ui, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = ui.p;
        div()
            .id("splitter")
            .flex_shrink_0()
            .w(px(theme::SPLITTER_HIT_W))
            .h_full()
            .flex()
            .justify_center()
            .cursor(CursorStyle::ResizeColumn)
            .bg(rgb(t.panel))
            .group("splitter")
            .child(
                div()
                    .w(px(1.0))
                    .h_full()
                    .bg(rgb(t.border))
                    .group_hover("splitter", |s| s.bg(theme::h(t.accent, 0.8))),
            )
            .on_drag(SplitDrag, |_, _, _, cx| cx.new(|_| DragGhost))
            .on_click(cx.listener(|app, event: &gpui::ClickEvent, _, cx| {
                if event.click_count() >= 2 {
                    app.prefs.inspector_width = theme::INSPECTOR_DEFAULT_W;
                    app.save_prefs(cx);
                    cx.notify();
                }
            }))
    }

    fn render_toast(&self, ui: &Ui) -> Option<AnyElement> {
        let t = ui.p;
        let (message, _) = self.toast.as_ref()?;
        Some(
            div()
                .absolute()
                .bottom(px(20.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .px(px(12.0))
                        .py(px(6.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(rgb(t.border))
                        .bg(rgb(t.panel_raised))
                        .shadow(ui.elevation())
                        .text_size(px(text::MD))
                        .text_color(rgb(t.text_soft))
                        .child(message.clone()),
                )
                .into_any_element(),
        )
    }
}

impl Render for LensletApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::fsguard::report_frame();
        crate::fsguard::milestone_once("first frame");
        let _scope = crate::fsguard::RenderScope::enter();
        let ui = Ui::of(cx);
        let t = ui.p;
        let viewport = f32::from(window.viewport_size().width);
        let sidebar_offset = if self.prefs.sidebar_open {
            SIDEBAR_W
        } else {
            0.0
        };
        let show_inspector = self.prefs.inspector_open;
        let inspector_w = self
            .prefs
            .inspector_width
            .clamp(320.0, (viewport - sidebar_offset - 420.0).max(320.0));

        let (list, detail): (gpui::AnyView, gpui::AnyView) = match self.mode {
            Mode::Calls => (cached(&self.calls), cached(&self.inspector)),
            Mode::Access => (cached(&self.access), cached(&self.access_detail)),
        };
        let mut workspace = div().flex().flex_row().flex_1().min_h(px(0.0));
        if self.prefs.sidebar_open {
            workspace = workspace.child(self.render_sidebar(&ui, cx));
        }
        workspace = workspace.child(list);
        if show_inspector {
            workspace = workspace.child(self.render_splitter(&ui, cx)).child(
                div()
                    .w(px(inspector_w))
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .child(detail),
            );
        }

        let header = self.render_header(&ui, window, cx);
        let menu = match &self.overlay {
            Some(Overlay::Menu(position, entries)) => {
                Some(self.render_menu(&ui, *position, &entries.clone(), cx))
            }
            _ => None,
        };
        let palette = self.palette_open.then(|| self.render_palette(&ui, cx));
        let settings = self.settings_open.then(|| self.render_settings(&ui, cx));
        let composer = self
            .composer_open
            .then(|| render_composer_overlay(&self.composer, cx));
        let toast = self.render_toast(&ui);

        div()
            .id("lenslet")
            .key_context("LensletApp")
            .track_focus(&self.root_focus)
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(t.canvas))
            .text_color(rgb(t.text))
            .font(fonts::sans_font())
            .on_action(
                cx.listener(|app, _: &OpenPalette, window, cx| app.toggle_palette(window, cx)),
            )
            .on_action(cx.listener(|app, _: &FocusSearch, window, cx| {
                app.palette_open = false;
                app.run_cmd(Cmd::FocusSearch, window, cx)
            }))
            .on_action(cx.listener(|app, _: &OpenSettings, _, cx| {
                if app.settings_open {
                    app.settings_open = false;
                    cx.notify();
                } else {
                    app.open_settings(cx);
                }
            }))
            .on_action(cx.listener(|app, _: &ToggleSidebar, window, cx| {
                app.run_cmd(Cmd::ToggleSidebar, window, cx)
            }))
            .on_action(cx.listener(|app, _: &ToggleInspector, window, cx| {
                app.run_cmd(Cmd::ToggleInspector, window, cx)
            }))
            .on_action(
                cx.listener(|app, _: &OpenFile, window, cx| app.run_cmd(Cmd::OpenFile, window, cx)),
            )
            .on_action(cx.listener(|app, _: &ExportReport, window, cx| {
                app.run_cmd(Cmd::ExportReport, window, cx)
            }))
            .on_action(cx.listener(|app, _: &RevealTrace, window, cx| {
                app.run_cmd(Cmd::RevealTrace, window, cx)
            }))
            .on_action(cx.listener(|app, _: &NextMatch, _, cx| {
                app.inspector.update(cx, |insp, cx| insp.step_match(1, cx));
            }))
            .on_action(cx.listener(|app, _: &PrevMatch, _, cx| {
                app.inspector.update(cx, |insp, cx| insp.step_match(-1, cx));
            }))
            .on_action(
                cx.listener(|app, _: &ReloadRun, window, cx| app.run_cmd(Cmd::Reload, window, cx)),
            )
            .on_action(
                cx.listener(|app, _: &Replay, window, cx| app.run_cmd(Cmd::Replay, window, cx)),
            )
            .on_action(cx.listener(|app, _: &OpenComposer, window, cx| {
                app.run_cmd(Cmd::OpenComposer, window, cx)
            }))
            .on_action(cx.listener(|app, _: &ShowCalls, window, cx| {
                app.run_cmd(Cmd::Mode(Mode::Calls), window, cx)
            }))
            .on_action(cx.listener(|app, _: &ShowAccess, window, cx| {
                app.run_cmd(Cmd::Mode(Mode::Access), window, cx)
            }))
            .on_action(
                cx.listener(|app, _: &PrevRun, window, cx| app.run_cmd(Cmd::PrevRun, window, cx)),
            )
            .on_action(
                cx.listener(|app, _: &NextRun, window, cx| app.run_cmd(Cmd::NextRun, window, cx)),
            )
            .on_action(cx.listener(|app, _: &FollowLatest, window, cx| {
                app.run_cmd(Cmd::FollowLatest, window, cx)
            }))
            .on_action(cx.listener(|app, _: &CopyCurl, window, cx| {
                if let Some(call) = app.inspector.read(cx).call().cloned() {
                    app.run_cmd(Cmd::CopyCurl(call), window, cx);
                }
            }))
            .on_action(cx.listener(|app, _: &CopyPolicy, window, cx| {
                app.run_cmd(Cmd::CopyPolicy, window, cx)
            }))
            .on_action(cx.listener(|app, _: &SelectNext, _, cx| {
                if app.palette_open {
                    app.palette_step(1, cx);
                } else {
                    app.select(|target, cx| target.step(1, cx), cx);
                }
            }))
            .on_action(cx.listener(|app, _: &SelectPrev, _, cx| {
                if app.palette_open {
                    app.palette_step(-1, cx);
                } else {
                    app.select(|target, cx| target.step(-1, cx), cx);
                }
            }))
            .on_action(
                cx.listener(|app, _: &SelectFirst, _, cx| {
                    app.select(|t, cx| t.edge(false, cx), cx)
                }),
            )
            .on_action(
                cx.listener(|app, _: &SelectLast, _, cx| app.select(|t, cx| t.edge(true, cx), cx)),
            )
            .on_action(
                cx.listener(|app, _: &PageDown, _, cx| app.select(|t, cx| t.page(true, cx), cx)),
            )
            .on_action(
                cx.listener(|app, _: &PageUp, _, cx| app.select(|t, cx| t.page(false, cx), cx)),
            )
            .on_action(cx.listener(|app, _: &Confirm, window, cx| {
                if app.palette_open {
                    app.palette_confirm(window, cx);
                } else if app.mode == Mode::Calls && app.prefs.inspector_open {
                    let focus = app.inspector.read(cx).body().focus_handle(cx);
                    window.focus(&focus);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|app, _: &Cancel, window, cx| {
                if app.palette_open {
                    app.close_palette(window, cx);
                } else {
                    app.cancel(window, cx);
                }
            }))
            .on_action(cx.listener(|app, _: &Copy, _, cx| {
                let body = app.inspector.read(cx).body().clone();
                body.update(cx, |body, cx| body.copy(cx));
            }))
            .on_action(cx.listener(|app, _: &SelectAll, _, cx| {
                let body = app.inspector.read(cx).body().clone();
                body.update(cx, |body, cx| body.select_all(cx));
            }))
            .on_drag_move(
                cx.listener(|app, event: &DragMoveEvent<SplitDrag>, window, cx| {
                    let viewport = f32::from(window.viewport_size().width);
                    let sidebar_offset = if app.prefs.sidebar_open {
                        SIDEBAR_W
                    } else {
                        0.0
                    };
                    let x = f32::from(event.event.position.x);
                    let width =
                        (viewport - x).clamp(320.0, (viewport - sidebar_offset - 420.0).max(320.0));
                    if (width - app.prefs.inspector_width).abs() >= 1.0 {
                        app.prefs.inspector_width = width;
                        app.save_prefs(cx);
                        cx.notify();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|app, _, _, cx| {
                    if matches!(app.overlay, Some(Overlay::Menu(..))) {
                        app.overlay = None;
                        cx.notify();
                    }
                }),
            )
            .child(header)
            .child(workspace)
            .children(menu)
            .children(settings)
            .children(composer)
            .children(palette)
            .children(toast)
    }
}
