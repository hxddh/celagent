//! End-to-End Run Launcher (`⌘N`), Output & Run Telemetry Console (`⌘3`),
//! and Native About Sheet.
//!
//! Enables launching any test suite or command through `lenslet run`, starting
//! local AWS emulators, inspecting live child `stdout`/`stderr` output, and
//! viewing 100% of `RunHeader` and `Report` metadata without leaving the app.

use std::{path::PathBuf, sync::Arc};

use gpui::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, PathPromptOptions, Render, Window, div, prelude::*, px, rgb,
};

use super::{
    bodyview::{BodyView, BodyViewEvent},
    session::{Health, Session, SessionEvent},
    widgets,
};
use crate::{
    body::BodyDoc,
    daemon::{self, CommandRunOpts, DetectedEmulator, EmulatorKind, ProxyHealth, RunGateMode},
    fonts, glyph, i18n, icons,
    model::{format_bytes, format_latency, full_time},
    search::{SearchEvent, SearchInput},
    syntax::SyntaxKind,
    theme::{self, Ui, text},
};

pub enum LauncherEvent {
    Close,
    WorkspacePicked(PathBuf),
    StartEmulator(EmulatorKind),
    StartRun(CommandRunOpts),
}

pub struct RunLauncher {
    cwd: PathBuf,
    recent_workspaces: Vec<PathBuf>,
    presets: Vec<(String, String)>,
    gate_mode: RunGateMode,
    allow_docker: bool,
    advanced_open: bool,
    env_probe: ProxyHealth,
    runs_dir: PathBuf,
    command_input: Entity<SearchInput>,
    upstream_input: Entity<SearchInput>,
    routes_input: Entity<SearchInput>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl EventEmitter<LauncherEvent> for RunLauncher {}

impl RunLauncher {
    pub fn new(runs_dir: PathBuf, home: PathBuf, cx: &mut Context<Self>) -> Self {
        let command_input = cx
            .new(|cx| SearchInput::new_field("pytest", "pytest / npm test / cargo test", true, cx));
        let upstream_input = cx.new(|cx| {
            SearchInput::new_field(
                "",
                "auto (or http://127.0.0.1:4566 / https://127.0.0.1:9298)",
                true,
                cx,
            )
        });
        let routes_input = cx.new(|cx| {
            SearchInput::new_field(
                "",
                "optional per-service routes: s3=http://127.0.0.1:9000, dynamodb=http://127.0.0.1:8000",
                true,
                cx,
            )
        });
        let sub_cmd = cx.subscribe(&command_input, |_, _, event, cx| match event {
            SearchEvent::Exit => cx.emit(LauncherEvent::Close),
        });
        Self {
            cwd: home,
            recent_workspaces: Vec::new(),
            presets: Vec::new(),
            gate_mode: RunGateMode::Record,
            allow_docker: false,
            advanced_open: false,
            env_probe: ProxyHealth::default(),
            runs_dir,
            command_input,
            upstream_input,
            routes_input,
            _subscriptions: vec![sub_cmd],
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &mut self,
        cwd: PathBuf,
        recent_workspaces: Vec<PathBuf>,
        last_command: Option<String>,
        default_upstream: Option<String>,
        default_routes: Vec<String>,
        allow_docker: bool,
        env_probe: ProxyHealth,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cwd = cwd.clone();
        self.recent_workspaces = recent_workspaces;
        self.allow_docker = allow_docker;
        self.env_probe = env_probe;
        self.advanced_open = allow_docker
            || default_upstream
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
            || !default_routes.is_empty();
        if let Some(cmd) = last_command.filter(|s| !s.trim().is_empty()) {
            self.command_input.update(cx, |i, cx| i.set_text(cmd, cx));
        }
        self.upstream_input.update(cx, |i, cx| {
            i.set_text(default_upstream.unwrap_or_default(), cx)
        });
        self.routes_input
            .update(cx, |i, cx| i.set_text(default_routes.join(", "), cx));
        self.refresh_presets(cwd, cx);
        let focus = self.command_input.read(cx).handle();
        window.focus(&focus);
        cx.notify();
    }

    pub fn update_probe(&mut self, probe: ProxyHealth, cx: &mut Context<Self>) {
        if self.env_probe != probe {
            self.env_probe = probe;
            cx.notify();
        }
    }

    pub fn set_cwd(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.cwd = dir.clone();
        cx.emit(LauncherEvent::WorkspacePicked(dir.clone()));
        self.refresh_presets(dir, cx);
        cx.notify();
    }

    fn refresh_presets(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let detected = cx
                .background_executor()
                .spawn(async move { daemon::detect_project_commands(&dir) })
                .await;
            this.update(cx, |launcher, cx| {
                let current_empty = launcher.command_input.read(cx).query().trim().is_empty();
                if current_empty && let Some((_, first_cmd)) = detected.first() {
                    let cmd = first_cmd.clone();
                    launcher
                        .command_input
                        .update(cx, |i, cx| i.set_text(cmd, cx));
                }
                launcher.presets = detected;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn prompt_folder(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(dir) = paths.into_iter().next()
            {
                this.update(cx, |launcher, cx| launcher.set_cwd(dir, cx))
                    .ok();
            }
        })
        .detach();
    }

    pub fn submit(&mut self, cx: &mut Context<Self>) {
        let command_line = self.command_input.read(cx).query().trim().to_string();
        let upstream_raw = self.upstream_input.read(cx).query().trim().to_string();
        let routes_raw = self.routes_input.read(cx).query().trim().to_string();
        let routes: Vec<String> = routes_raw
            .split([',', ';'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let opts = CommandRunOpts {
            command_line,
            cwd: self.cwd.clone(),
            upstream: (!upstream_raw.is_empty()).then_some(upstream_raw),
            routes,
            gate_mode: self.gate_mode,
            allow_docker: self.allow_docker,
            runs_dir: self.runs_dir.clone(),
        };
        cx.emit(LauncherEvent::StartRun(opts));
    }
}

impl Render for RunLauncher {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scope = crate::fsguard::RenderScope::enter();
        let ui = Ui::of(cx);
        let t = ui.p;

        let mut preset_row = div().flex().flex_wrap().items_center().gap(px(6.0));
        let cur_cmd = self.command_input.read(cx).query().trim().to_string();
        for (ix, (label, cmd)) in self.presets.iter().take(6).enumerate() {
            let cmd_val = cmd.clone();
            let active = cur_cmd == *cmd;
            preset_row = preset_row.child(
                widgets::chip(
                    &ui,
                    ElementId::named_usize("preset-cmd", ix),
                    label.clone(),
                    None,
                    active,
                    t.accent,
                )
                .on_click(cx.listener(move |launcher, _, _, cx| {
                    let c = cmd_val.clone();
                    launcher.command_input.update(cx, |i, cx| i.set_text(c, cx));
                    cx.notify();
                })),
            );
        }

        let mut recent_row = div().flex().flex_wrap().items_center().gap(px(6.0));
        for (ix, dir) in self.recent_workspaces.iter().take(4).enumerate() {
            if dir == &self.cwd {
                continue;
            }
            let short = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("workspace")
                .to_string();
            let target_dir = dir.clone();
            recent_row = recent_row.child(
                widgets::chip(
                    &ui,
                    ElementId::named_usize("recent-ws", ix),
                    short,
                    None,
                    false,
                    t.text_muted,
                )
                .tooltip(widgets::tip(dir.display().to_string(), None))
                .on_click(cx.listener(move |launcher, _, _, cx| {
                    launcher.set_cwd(target_dir.clone(), cx);
                })),
            );
        }

        let cur_up = self.upstream_input.read(cx).query().trim().to_string();
        let mut emu_row = div().flex().flex_wrap().items_center().gap(px(6.0));
        emu_row = emu_row.child(
            widgets::chip(
                &ui,
                "emu-auto",
                ui.t("Auto-detect", "自动探测"),
                None,
                cur_up.is_empty(),
                t.green,
            )
            .on_click(cx.listener(|launcher, _, _, cx| {
                launcher
                    .upstream_input
                    .update(cx, |i, cx| i.set_text("", cx));
                cx.notify();
            })),
        );
        for (ix, DetectedEmulator { name, port, url }) in
            self.env_probe.emulators.iter().enumerate()
        {
            let url_val = url.clone();
            let active = cur_up == *url;
            emu_row = emu_row.child(
                widgets::chip(
                    &ui,
                    ElementId::named_usize("emu-live", ix),
                    format!("● {name} :{port}"),
                    None,
                    active,
                    t.green,
                )
                .on_click(cx.listener(move |launcher, _, _, cx| {
                    let u = url_val.clone();
                    launcher
                        .upstream_input
                        .update(cx, |i, cx| i.set_text(u, cx));
                    cx.notify();
                })),
            );
        }
        if self.env_probe.emulators.is_empty() {
            for (ix, kind) in self
                .env_probe
                .installed_emulators
                .iter()
                .copied()
                .enumerate()
            {
                emu_row = emu_row.child(
                    widgets::button(
                        &ui,
                        ElementId::named_usize("start-emu", ix),
                        format!("{} {}", ui.t("Start", "启动"), kind.label()),
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(LauncherEvent::StartEmulator(kind));
                    })),
                );
            }
        }

        let ent = cx.entity();
        let gate_seg = widgets::segmented(
            &ui,
            vec![
                widgets::Segment {
                    id: "gate-record",
                    label: ui.t("Record", "仅记录").into(),
                    active: self.gate_mode == RunGateMode::Record,
                    tooltip: Some((
                        ui.t(
                            "Capture calls and compare against lenslet.lock",
                            "捕获调用并与 lenslet.lock 对比",
                        )
                        .into(),
                        None,
                    )),
                },
                widgets::Segment {
                    id: "gate-update",
                    label: ui.t("Update Lock", "更新 Lock").into(),
                    active: self.gate_mode == RunGateMode::UpdateLock,
                    tooltip: Some((
                        ui.t(
                            "Write observed permissions into lenslet.lock (--update)",
                            "将捕获的权限自动写入 lenslet.lock (--update)",
                        )
                        .into(),
                        None,
                    )),
                },
                widgets::Segment {
                    id: "gate-check",
                    label: ui.t("Check Lock", "严格校验").into(),
                    active: self.gate_mode == RunGateMode::CheckLock,
                    tooltip: Some((
                        ui.t(
                            "Fail exit code if new permissions appear (--check --strict)",
                            "发现未锁定权限时返回非零退出码 (--check --strict)",
                        )
                        .into(),
                        None,
                    )),
                },
                widgets::Segment {
                    id: "gate-enforce",
                    label: ui.t("Enforce 403", "实时拦截").into(),
                    active: self.gate_mode == RunGateMode::Enforce,
                    tooltip: Some((
                        ui.t(
                            "Block unlocked AWS actions live with AccessDenied (--enforce)",
                            "对未授权的 AWS 调用实时返回 403 AccessDenied (--enforce)",
                        )
                        .into(),
                        None,
                    )),
                },
            ],
            move |ix, _, cx| {
                let mode = [
                    RunGateMode::Record,
                    RunGateMode::UpdateLock,
                    RunGateMode::CheckLock,
                    RunGateMode::Enforce,
                ][ix];
                ent.update(cx, |launcher, cx| {
                    launcher.gate_mode = mode;
                    cx.notify();
                });
            },
        );

        let card = div()
            .id("run-launcher-modal")
            .w(px(680.0))
            .flex()
            .flex_col()
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(t.border))
            .bg(rgb(t.panel_raised))
            .shadow(ui.elevation())
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(LauncherEvent::Close)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .h(px(48.0))
                    .px(px(18.0))
                    .border_b_1()
                    .border_color(rgb(t.border_subtle))
                    .child(icons::icon("icons/play.svg", 14.0, theme::h(t.accent, 1.0)))
                    .child(
                        div()
                            .text_size(px(text::XL))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(t.text))
                            .child(ui.t("Run Command Through Lenslet", "在 Lenslet 中发起运行")),
                    )
                    .child(div().flex_1())
                    .child(
                        widgets::button(&ui, "close-launcher", ui.t("Cancel", "取消"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(LauncherEvent::Close))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(14.0))
                    .p(px(18.0))
                    // 1. Workspace Folder
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(widgets::label(
                                &ui,
                                ui.t("WORKSPACE DIRECTORY", "项目工作区目录"),
                            ))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .h(px(28.0))
                                            .px(px(10.0))
                                            .flex()
                                            .items_center()
                                            .gap(px(6.0))
                                            .rounded(px(6.0))
                                            .border_1()
                                            .border_color(rgb(t.border))
                                            .bg(rgb(t.code))
                                            .child(icons::icon(
                                                "icons/folder.svg",
                                                13.0,
                                                theme::h(t.accent, 0.9),
                                            ))
                                            .child(widgets::mono(
                                                &ui,
                                                self.cwd.display().to_string(),
                                            )),
                                    )
                                    .child(
                                        widgets::button(
                                            &ui,
                                            "pick-workspace-btn",
                                            ui.t("Choose Folder…", "选择目录…"),
                                        )
                                        .on_click(
                                            cx.listener(|l, _, _, cx| l.prompt_folder(cx)),
                                        ),
                                    ),
                            )
                            .when(!self.recent_workspaces.is_empty(), |el| el.child(recent_row)),
                    )
                    // 2. Command & Detected Presets
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(widgets::label(
                                &ui,
                                ui.t("COMMAND / TEST SUITE", "执行命令 / 测试套件"),
                            ))
                            .child(self.command_input.clone())
                            .when(!self.presets.is_empty(), |el| el.child(preset_row)),
                    )
                    // 3. IAM Gate Mode
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(widgets::label(
                                &ui,
                                ui.t("PERMISSION MODE", "AWS 权限记录与校验模式"),
                            ))
                            .child(gate_seg),
                    )
                    // 4. Collapsible Advanced Network Options
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .id("launcher-adv-toggle")
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .cursor_pointer()
                                    .text_size(px(text::SM))
                                    .text_color(rgb(t.text_muted))
                                    .hover(move |s| s.text_color(rgb(t.text)))
                                    .on_click(cx.listener(|l, _, _, cx| {
                                        l.advanced_open = !l.advanced_open;
                                        cx.notify();
                                    }))
                                    .child(if self.advanced_open { "▾" } else { "▸" })
                                    .child(if self.env_probe.emulators.is_empty() {
                                        ui.t(
                                            "Advanced options (default: Built-in AWS Sandbox — zero setup required)",
                                            "高级选项（默认自动启用内置 AWS 沙盒，无需安装外部模拟器）",
                                        )
                                        .to_string()
                                    } else {
                                        format!(
                                            "{} ({})",
                                            ui.t("Advanced options", "高级选项"),
                                            self.env_probe.emulators[0].name
                                        )
                                    }),
                            )
                            .when(self.advanced_open, |el| {
                                el.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(8.0))
                                        .p(px(10.0))
                                        .rounded(px(8.0))
                                        .border_1()
                                        .border_color(rgb(t.border_subtle))
                                        .bg(rgb(t.panel))
                                        .child(emu_row)
                                        .child(self.upstream_input.clone())
                                        .child(self.routes_input.clone())
                                        .child(
                                            div().flex().items_center().child(
                                                widgets::chip(
                                                    &ui,
                                                    "launcher-docker",
                                                    ui.t(
                                                        "Allow Docker containers",
                                                        "允许 Docker 容器接入",
                                                    ),
                                                    None,
                                                    self.allow_docker,
                                                    t.accent,
                                                )
                                                .on_click(cx.listener(|l, _, _, cx| {
                                                    l.allow_docker = !l.allow_docker;
                                                    cx.notify();
                                                })),
                                            ),
                                        ),
                                )
                            }),
                    )
                    // 5. Launch Footer
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .pt(px(6.0))
                            .border_t_1()
                            .border_color(rgb(t.border_subtle))
                            .child(
                                div()
                                    .text_size(px(text::XS))
                                    .text_color(rgb(t.text_muted))
                                    .child(ui.t(
                                        "Injects AWS_ENDPOINT_URL & captures calls + stdout/stderr live.",
                                        "自动注入 AWS_ENDPOINT_URL，实时捕获所有 AWS 调用与终端标准输出。",
                                    )),
                            )
                            .child(
                                widgets::primary_button(
                                    &ui,
                                    "launcher-submit",
                                    ui.t("Run Command", "立即运行"),
                                )
                                .on_click(cx.listener(|l, _, _, cx| l.submit(cx))),
                            ),
                    ),
            );

        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme::h(0x000000, if ui.dark() { 0.44 } else { 0.20 }))
            .child(widgets::appear(&ui, "launcher-anim", card))
    }
}

pub fn render_launcher_overlay(launcher: &Entity<RunLauncher>, _cx: &App) -> AnyElement {
    launcher.clone().into_any_element()
}

// ============================================================================
// Output & Run Telemetry Console View (Mode::Console, ⌘3)
// ============================================================================

pub enum ConsoleEvent {
    NewRun,
    StopRun,
    LoadAllCalls,
    Toast(String),
}

pub struct ConsoleView {
    session: Entity<Session>,
    focus: FocusHandle,
    log_view: Entity<BodyView>,
    tokens: Vec<String>,
    last_log: Arc<str>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl EventEmitter<ConsoleEvent> for ConsoleView {}

impl Focusable for ConsoleView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ConsoleView {
    pub fn new(session: Entity<Session>, cx: &mut Context<Self>) -> Self {
        let log_view = cx.new(|cx| BodyView::new("run-console-log", cx));
        let sub_body = cx.subscribe(&log_view, |_, _, event, cx| match event {
            BodyViewEvent::Copied(bytes) => {
                let lang = Ui::of(cx).lang;
                cx.emit(ConsoleEvent::Toast(i18n::copied(
                    &format_bytes(*bytes),
                    lang,
                )));
            }
        });
        let sub_sess = cx.subscribe(&session, |this, _, event, cx| {
            if matches!(
                event,
                SessionEvent::RunChanged | SessionEvent::Derived | SessionEvent::Rows(_)
            ) {
                this.sync_log(false, cx);
                cx.notify();
            }
        });
        let mut view = Self {
            session,
            focus: cx.focus_handle(),
            log_view,
            tokens: Vec::new(),
            last_log: Arc::from(""),
            _subscriptions: vec![sub_body, sub_sess],
        };
        view.sync_log(true, cx);
        view
    }

    pub fn set_tokens(&mut self, tokens: Vec<String>, cx: &mut Context<Self>) {
        if self.tokens != tokens {
            self.tokens = tokens;
            self.sync_log(true, cx);
        }
    }

    pub fn restyle(&mut self, cx: &mut Context<Self>) {
        self.sync_log(true, cx);
    }

    fn sync_log(&mut self, force: bool, cx: &mut Context<Self>) {
        let ui = Ui::of(cx);
        let log = self.session.read(cx).console_log.clone();
        if !force && Arc::ptr_eq(&log, &self.last_log) {
            return;
        }
        self.last_log = log.clone();
        let placeholder = ui.t(
            "No child command output recorded for this run (proxy-only sessions have no child process; launch a command via ⌘N to capture stdout & stderr).",
            "当前运行暂无子进程输出（纯代理会话无子进程；按 ⌘N 发起命令即可实时捕获 stdout / stderr）。",
        );
        let doc = Arc::new(BodyDoc::from_text(
            log.to_string(),
            SyntaxKind::Plain,
            &self.tokens,
            &ui.p,
        ));
        self.log_view.update(cx, |b, cx| {
            b.set_placeholder(placeholder, cx);
            b.set_doc(doc, cx);
        });
    }
}

impl Render for ConsoleView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scope = crate::fsguard::RenderScope::enter();
        let ui = Ui::of(cx);
        let t = ui.p;
        let session = self.session.read(cx);

        let log_view = self.log_view.clone();
        let mut subbar = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .h(px(theme::SUBBAR_H))
            .px(px(theme::ROW_PAD_X))
            .border_b_1()
            .border_color(rgb(t.border_subtle))
            .bg(rgb(t.toolbar))
            .child(icons::icon(
                "icons/terminal.svg",
                13.0,
                theme::h(t.accent, 0.95),
            ))
            .child(widgets::label(
                &ui,
                ui.t("RUN TELEMETRY & CONSOLE OUTPUT", "运行全量元数据与终端输出"),
            ))
            .child(div().flex_1());

        if session.evicted > 0 && !session.store.is_unbounded() {
            subbar = subbar.child(
                widgets::button(
                    &ui,
                    "console-load-all",
                    if ui.zh() {
                        format!("加载全部调用（+{}）", session.evicted)
                    } else {
                        format!("Load all calls (+{})", session.evicted)
                    },
                )
                .on_click(cx.listener(|_, _, _, cx| cx.emit(ConsoleEvent::LoadAllCalls))),
            );
        }
        if session.is_live() {
            subbar = subbar.child(
                widgets::danger_button(&ui, "console-stop-run", ui.t("Stop Run", "停止运行"))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ConsoleEvent::StopRun))),
            );
        }
        subbar = subbar
            .child(
                widgets::primary_button(&ui, "console-new-run", ui.t("New Run…", "新建运行…"))
                    .tooltip(widgets::tip(
                        ui.t("Run a command through Lenslet", "通过 Lenslet 运行命令"),
                        Some(i18n::shortcut("N")),
                    ))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ConsoleEvent::NewRun))),
            )
            .child(
                widgets::button(&ui, "console-copy-log", ui.t("Copy Output", "复制输出"))
                    .on_click(move |_, _, cx| log_view.update(cx, |b, cx| b.copy(cx))),
            );

        if session.health == Health::Missing {
            return div()
                .id("console-view")
                .size_full()
                .track_focus(&self.focus)
                .flex()
                .flex_col()
                .bg(rgb(t.panel))
                .child(subbar)
                .child(widgets::empty_state(
                    &ui,
                    ui.t("No run selected", "尚未选择或发起运行"),
                    ui.t(
                        "Press ⌘N to launch a test suite or command directly inside Lenslet.",
                        "按 ⌘N 直接在 Lenslet 内启动测试套件或命令。",
                    ),
                    Some(
                        widgets::primary_button(
                            &ui,
                            "empty-console-run",
                            ui.t("Run Command…", "运行命令…"),
                        )
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ConsoleEvent::NewRun)))
                        .into_any_element(),
                    ),
                ));
        }

        let mut meta_left = div().flex().flex_col().flex_1().min_w(px(280.0));
        let mut meta_right = div().flex().flex_col().flex_1().min_w(px(280.0));

        if let Some(run) = session.run.as_ref() {
            let h = &run.header;
            let r = run.report.as_ref();
            let cmd_str = if h.command.is_empty() {
                ui.t("(proxy session)", "（独立代理会话）").to_string()
            } else {
                run.command_line()
            };
            meta_left = meta_left
                .child(widgets::meta_row(
                    &ui,
                    "Run ID",
                    widgets::mono(&ui, h.run.clone()),
                ))
                .child(widgets::meta_row(
                    &ui,
                    ui.t("Command", "命令"),
                    widgets::mono(&ui, cmd_str),
                ))
                .child(widgets::meta_row(
                    &ui,
                    ui.t("Workspace", "工作目录"),
                    widgets::mono(
                        &ui,
                        if h.cwd.is_empty() {
                            "—".into()
                        } else {
                            h.cwd.clone()
                        },
                    ),
                ))
                .child(widgets::meta_row(
                    &ui,
                    "Started",
                    widgets::mono(&ui, full_time(&h.started)),
                ))
                .child(widgets::meta_row(
                    &ui,
                    ui.t("Status / Exit", "状态 / 退出码"),
                    widgets::mono(
                        &ui,
                        match r {
                            Some(rep) => format!(
                                "{} · {} · ended {}",
                                match rep.exit_code {
                                    Some(code) => format!("exit {code}"),
                                    None => "ended".into(),
                                },
                                format_latency(rep.duration_ms),
                                full_time(&rep.ended)
                            ),
                            None => format!(
                                "{} · {}",
                                ui.t("live", "运行中"),
                                format_latency(session.run_span_ms().max(0) as u64)
                            ),
                        },
                    ),
                ))
                .child(widgets::meta_row(
                    &ui,
                    "PID",
                    widgets::mono(
                        &ui,
                        if h.pid > 0 {
                            h.pid.to_string()
                        } else {
                            "—".into()
                        },
                    ),
                ));

            if let Some(trace_path) = session.path.as_ref() {
                meta_left = meta_left.child(widgets::meta_row(
                    &ui,
                    ui.t("Trace File", "追踪文件"),
                    widgets::mono(&ui, trace_path.display().to_string()),
                ));
            }

            let routes_str = if h.routes.is_empty() {
                "—".to_string()
            } else {
                h.routes
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let stats = session.store.stats();
            let counts_str = match r {
                Some(rep) => format!(
                    "{} calls · {} errors · {} retries · {} blocked · {} unfinished",
                    rep.calls, rep.errors, rep.retries, rep.blocked, rep.unfinished
                ),
                None => format!(
                    "{} calls · {} errors · {} retries · {} blocked · {} pending",
                    stats.total, stats.errors, stats.retries, stats.blocked, stats.pending
                ),
            };
            meta_right = meta_right
                .child(widgets::meta_row(
                    &ui,
                    "Endpoint",
                    widgets::mono(
                        &ui,
                        if h.endpoint.is_empty() {
                            "—".into()
                        } else {
                            h.endpoint.clone()
                        },
                    ),
                ))
                .child(widgets::meta_row(
                    &ui,
                    "Upstream",
                    widgets::mono(
                        &ui,
                        if h.upstream.is_empty() {
                            "—".into()
                        } else {
                            h.upstream.clone()
                        },
                    ),
                ))
                .child(widgets::meta_row(
                    &ui,
                    ui.t("Routes", "分服务路由"),
                    widgets::mono(&ui, routes_str),
                ))
                .child(widgets::meta_row(
                    &ui,
                    ui.t("Gate / Policy", "权限门禁"),
                    widgets::mono(
                        &ui,
                        format!(
                            "{}{}{}",
                            h.lock.as_deref().or(h.policy.as_deref()).unwrap_or("none"),
                            if h.enforce { " · enforce=403" } else { "" },
                            if h.allow_docker { " · docker=on" } else { "" },
                        ),
                    ),
                ))
                .child(widgets::meta_row(
                    &ui,
                    ui.t("Capture", "捕获统计"),
                    widgets::mono(
                        &ui,
                        format!(
                            "{counts_str}{}{}",
                            if let Some(mb) = h.max_body {
                                format!(" · max_body={}", format_bytes(mb as usize))
                            } else {
                                String::new()
                            },
                            if session.skipped > 0 {
                                format!(" · skipped={}", session.skipped)
                            } else {
                                String::new()
                            }
                        ),
                    ),
                ));

            if let Some(rep) = r {
                if !rep.unmapped.is_empty() {
                    let unmapped = rep
                        .unmapped
                        .iter()
                        .map(|(op, count)| format!("{op} ({count}×)"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    meta_right = meta_right.child(widgets::meta_row(
                        &ui,
                        ui.t("Unmapped", "未映射操作"),
                        widgets::mono(&ui, unmapped),
                    ));
                }
                if !rep.roles.is_empty() {
                    let roles = rep
                        .roles
                        .iter()
                        .map(|(role, entries)| format!("{role} ({} perm)", entries.len()))
                        .collect::<Vec<_>>()
                        .join(", ");
                    meta_right = meta_right.child(widgets::meta_row(
                        &ui,
                        ui.t("Roles", "扮演角色"),
                        widgets::mono(&ui, roles),
                    ));
                }
            }
        }

        div()
            .id("console-view")
            .size_full()
            .key_context("Console")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .bg(rgb(t.panel))
            .child(subbar)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(20.0))
                    .px(px(16.0))
                    .py(px(10.0))
                    .border_b_1()
                    .border_color(rgb(t.border_subtle))
                    .bg(rgb(t.panel_raised))
                    .child(meta_left)
                    .child(meta_right),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.0))
                    .p(px(12.0))
                    .gap(px(6.0))
                    .child(widgets::label(
                        &ui,
                        ui.t(
                            "STDOUT / STDERR STREAM",
                            "标准输出与错误流 (STDOUT / STDERR)",
                        ),
                    ))
                    .child(super::cached(&self.log_view)),
            )
    }
}

// ============================================================================
// Native About Dialog (Lenslet > About Lenslet)
// ============================================================================

pub fn render_about_overlay(
    ui: &Ui,
    on_close: impl Fn(&mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let t = ui.p;
    let close_out = on_close.clone();
    let close_btn = on_close;
    let card = div()
        .id("about-modal")
        .w(px(400.0))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(10.0))
        .p(px(24.0))
        .rounded(px(14.0))
        .border_1()
        .border_color(rgb(t.border))
        .bg(rgb(t.panel_raised))
        .shadow(ui.elevation())
        .on_mouse_down_out(move |_, window, cx| close_out(window, cx))
        .child(glyph::lens_mark(&t, 52.0))
        .child(
            div()
                .mt(px(4.0))
                .text_size(px(20.0))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(t.text))
                .child("Lenslet"),
        )
        .child(
            div()
                .font(fonts::mono_font())
                .text_size(px(text::SM))
                .text_color(rgb(t.text_muted))
                .child(format!(
                    "Version {} ({}-{})",
                    env!("CARGO_PKG_VERSION"),
                    std::env::consts::ARCH,
                    std::env::consts::OS
                )),
        )
        .child(
            div()
                .text_center()
                .text_size(px(text::MD))
                .line_height(px(18.0))
                .text_color(rgb(t.text_soft))
                .child(ui.t(
                    "Local-only AWS SDK flight recorder, zero-loss payload inspector, and least-privilege IAM compiler.",
                    "纯本地 AWS SDK 飞行记录仪、零丢包报文检视器与最小权限 IAM 编译器。",
                )),
        )
        .child(
            div()
                .w_full()
                .p(px(10.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(theme::h(t.green, 0.35))
                .bg(theme::h(t.green, 0.06))
                .text_center()
                .text_size(px(text::XS))
                .text_color(rgb(t.text_body))
                .child(ui.t(
                    "Strict Local Boundary: Refuses *.amazonaws.com & public IPs. Zero telemetry, zero cloud forwarding.",
                    "严格本地边界：拒绝 *.amazonaws.com 与公网 IP。零遥测，零云端转发。",
                )),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .pt(px(6.0))
                .child(
                    widgets::button(ui, "about-github", "GitHub").on_click(|_, _, cx| {
                        cx.open_url("https://github.com/hxddh/lenslet");
                    }),
                )
                .child(
                    widgets::button(ui, "about-issues", ui.t("Report Issue", "反馈问题"))
                        .on_click(|_, _, cx| {
                            cx.open_url("https://github.com/hxddh/lenslet/issues");
                        }),
                )
                .child(
                    widgets::primary_button(ui, "about-close", ui.t("OK", "好"))
                        .px(px(16.0))
                        .on_click(move |_, window, cx| close_btn(window, cx)),
                ),
        );

    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::h(0x000000, if ui.dark() { 0.40 } else { 0.18 }))
        .child(widgets::appear(ui, "about-anim", card))
        .into_any_element()
}
