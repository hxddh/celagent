//! Collapsible Workspace Sidebar (⌘0):
//! 1. Active Workspace & End-to-End Launcher (`New Run…` ⌘N, `Open Folder…`, `Init CI`).
//! 2. Local Proxy & Emulator Control Plane — live status (`/_lenslet/health`),
//!    1-click Start/Stop Proxy, `--enforce` toggle, multi-shell `Copy Env`,
//!    and 1-click Local Emulator starter (`AWS Emulator`, `Moto`, `LocalStack`, `MinIO`).
//! 3. Project-Grouped Runs — groups recorded runs by workspace directory with
//!    one-click switching and `★` pin protection against retention pruning.

use std::collections::BTreeMap;

use gpui::{
    AnyElement, Context, ElementId, FontWeight, MouseButton, MouseDownEvent, div, prelude::*, px,
    rgb,
};

use super::{Cmd, LensletApp, MenuEntry, widgets};
use crate::{
    daemon::ShellFormat,
    fonts, glyph, i18n, icons,
    model::ellipsize,
    theme::{self, Ui, text},
};

pub const SIDEBAR_W: f32 = 252.0;

impl LensletApp {
    pub(super) fn render_sidebar(&self, ui: &Ui, cx: &mut Context<Self>) -> AnyElement {
        let t = ui.p;
        let session = self.session.read(cx);
        let env = &self.env_probe;
        let managed_running = self.managed_proxy.is_some();
        let active_cmd_running = self.managed_run.is_some();

        // 1. Workspace & End-to-End Run Card
        let ws_path = self.active_workspace_dir(cx);
        let ws_name = ws_path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("workspace")
            .to_string();

        let mut ws_card = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .p(px(10.0))
            .mx(px(8.0))
            .mt(px(8.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(t.border_subtle))
            .bg(rgb(t.panel_raised))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(icons::icon(
                        "icons/folder.svg",
                        13.0,
                        theme::h(t.accent, 0.95),
                    ))
                    .child(
                        div()
                            .id("sb-ws-name")
                            .flex_1()
                            .min_w(px(0.0))
                            .font(fonts::mono_font())
                            .text_size(px(text::SM))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(t.text))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .tooltip(widgets::tip(ws_path.display().to_string(), None))
                            .child(ws_name),
                    )
                    .child(
                        widgets::button(ui, "sb-pick-ws", ui.t("Folder…", "目录…"))
                            .px(px(6.0))
                            .tooltip(widgets::tip(
                                ui.t("Open Workspace Folder…", "选择项目工作区目录…"),
                                Some(i18n::shortcut("⌥O")),
                            ))
                            .on_click(cx.listener(|app, _, window, cx| {
                                app.run_cmd(Cmd::OpenWorkspace, window, cx)
                            })),
                    ),
            );

        let mut ws_actions = div().flex().flex_wrap().items_center().gap(px(6.0));
        if active_cmd_running {
            ws_actions = ws_actions.child(
                widgets::danger_button(ui, "sb-stop-run", ui.t("■ Stop", "■ 停止运行"))
                    .tooltip(widgets::tip(
                        ui.t("Stop active command cleanly", "平滑终止当前运行"),
                        Some(i18n::shortcut(".")),
                    ))
                    .on_click(cx.listener(|app, _, window, cx| {
                        app.run_cmd(Cmd::StopActiveRun, window, cx)
                    })),
            );
        } else {
            let short_cmd = ellipsize(&self.detected_cmd, 16);
            ws_actions =
                ws_actions
                    .child(
                        widgets::primary_button(ui, "sb-quick-run", format!("▶ {}", short_cmd))
                            .tooltip(widgets::tip(
                                format!(
                                    "{}: `{}`",
                                    ui.t("1-click run in workspace", "一键运行当前项目命令"),
                                    self.detected_cmd
                                ),
                                None,
                            ))
                            .on_click(cx.listener(|app, _, window, cx| {
                                app.run_cmd(Cmd::QuickRun, window, cx)
                            })),
                    )
                    .child(
                        widgets::button(ui, "sb-new-run", ui.t("Options…", "选项…"))
                            .tooltip(widgets::tip(
                                ui.t("Customize command or lock mode", "自定义运行命令或模式"),
                                Some(i18n::shortcut("N")),
                            ))
                            .on_click(cx.listener(|app, _, window, cx| {
                                app.run_cmd(Cmd::OpenLauncher, window, cx)
                            })),
                    );
        }

        let term_row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .child(
                widgets::button(ui, "sb-open-term", ui.t("Open Terminal", "打开抓包终端"))
                    .tooltip(widgets::tip(
                        ui.t(
                            "Auto-start proxy & open a terminal pre-configured with AWS_ENDPOINT_URL",
                            "自动启动本地代理并打开已配置 AWS_ENDPOINT_URL 的系统终端",
                        ),
                        None,
                    ))
                    .on_click(cx.listener(|app, _, window, cx| {
                        app.run_cmd(Cmd::OpenTerminal, window, cx)
                    })),
            )
            .child(
                widgets::button(ui, "sb-new-req", ui.t("Request…", "发包…"))
                    .tooltip(widgets::tip(
                        ui.t(
                            "Compose a local AWS request from scratch",
                            "从零构造本地 AWS 请求",
                        ),
                        Some(i18n::shortcut("⇧N")),
                    ))
                    .on_click(
                        cx.listener(|app, _, window, cx| app.run_cmd(Cmd::NewRequest, window, cx)),
                    ),
            )
            .child(
                widgets::button(ui, "sb-init-ci", "CI")
                    .tooltip(widgets::tip(
                        ui.t(
                            "Generate .github/workflows/lenslet.yml in workspace",
                            "在当前工作区生成 .github/workflows/lenslet.yml",
                        ),
                        None,
                    ))
                    .on_click(cx.listener(|app, _, window, cx| {
                        app.run_cmd(Cmd::InitCiWorkflow, window, cx)
                    })),
            );
        ws_card = ws_card.child(ws_actions).child(term_row);

        // 2. Local Proxy & Built-in Sandbox Card
        let is_sandbox_live = env
            .upstream
            .as_deref()
            .is_some_and(|u| u.starts_with("sandbox://"));
        let (status_label, status_color) = if env.running {
            let suffix = if is_sandbox_live {
                ui.t(" · Sandbox", " · 内置沙盒")
            } else {
                ""
            };
            (
                format!(
                    "{} :{}{}{}",
                    ui.t("Proxy Live", "代理运行中"),
                    self.proxy_port,
                    suffix,
                    if env.enforce {
                        ui.t(" · Enforcing", " · 门禁开启")
                    } else {
                        ""
                    }
                ),
                t.green,
            )
        } else if !env.emulators.is_empty() {
            (
                format!(
                    "{} ({})",
                    ui.t("Emulator Ready", "模拟器就绪"),
                    env.emulators[0].name
                ),
                t.green,
            )
        } else {
            (
                ui.t("Built-in AWS Sandbox Ready", "内置 AWS 沙盒就绪")
                    .into(),
                t.accent,
            )
        };

        let mut proxy_card = div()
            .flex()
            .flex_col()
            .gap(px(7.0))
            .p(px(10.0))
            .m(px(8.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(t.border_subtle))
            .bg(rgb(t.panel_raised))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(glyph::dot(theme::h(status_color, 1.0), 7.0))
                    .child(
                        div()
                            .flex_1()
                            .font(fonts::mono_font())
                            .text_size(px(text::XS))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(status_color))
                            .child(status_label),
                    ),
            );

        if env.running {
            let upstream = match env.upstream.as_deref() {
                Some(u) if u.starts_with("sandbox://") => {
                    ui.t("Built-in AWS Sandbox", "内置 AWS 沙盒")
                }
                Some(u) if !u.is_empty() => u,
                _ => "auto",
            };
            proxy_card = proxy_card.child(
                div()
                    .font(fonts::mono_font())
                    .text_size(px(text::XS))
                    .text_color(rgb(t.text_muted))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(format!(
                        "→ {upstream} · {} {} / {} {}",
                        env.calls,
                        ui.t("calls", "调用"),
                        env.errors,
                        ui.t("err", "错误")
                    )),
            );
        } else if !env.emulators.is_empty() {
            let emu_line = env
                .emulators
                .iter()
                .map(|e| format!("{} :{}", e.name, e.port))
                .collect::<Vec<_>>()
                .join(" · ");
            proxy_card = proxy_card.child(
                div()
                    .font(fonts::mono_font())
                    .text_size(px(text::XS))
                    .text_color(rgb(t.text_muted))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(emu_line),
            );
        } else {
            proxy_card = proxy_card.child(
                div()
                    .text_size(px(text::XS))
                    .text_color(rgb(t.text_muted))
                    .child(ui.t(
                        "S3 · DynamoDB · SQS · SNS · STS · Secrets · KMS (zero setup)",
                        "免装模拟器，直接支持 S3 / DynamoDB / SQS / STS 等状态调用",
                    )),
            );
        }

        let mut controls = div().flex().flex_wrap().items_center().gap(px(6.0));
        if managed_running {
            controls = controls.child(
                widgets::danger_button(ui, "sb-stop-proxy", ui.t("Stop Proxy", "停止代理"))
                    .on_click(
                        cx.listener(|app, _, window, cx| app.run_cmd(Cmd::StopProxy, window, cx)),
                    ),
            );
        } else if !env.running {
            controls = controls.child(
                widgets::button(ui, "sb-start-proxy", ui.t("Start Proxy", "后台代理")).on_click(
                    cx.listener(|app, _, window, cx| app.run_cmd(Cmd::StartProxy, window, cx)),
                ),
            );
        }
        controls = controls
            .child(
                widgets::chip(
                    ui,
                    "sb-proxy-enforce",
                    ui.t("Enforce Lock", "拦截未授权"),
                    None,
                    self.prefs.proxy_enforce,
                    t.purple,
                )
                .tooltip(widgets::tip(
                    ui.t(
                        "Block AWS actions not in lenslet.lock with AccessDenied (--enforce)",
                        "对不在 lenslet.lock 中的 AWS 调用实时返回拒绝 (--enforce)",
                    ),
                    None,
                ))
                .on_click(cx.listener(|app, _, window, cx| {
                    let next = !app.prefs.proxy_enforce;
                    app.update_prefs(window, cx, |p| p.proxy_enforce = next);
                })),
            )
            .child(
                widgets::button(ui, "sb-copy-env", ui.t("Copy Env ▾", "环境变量 ▾"))
                    .tooltip(widgets::tip(
                        ui.t(
                            "Copy AWS_ENDPOINT_URL snippet for sh/zsh, fish, PowerShell, .env, or Docker",
                            "复制终端环境变量（支持 sh/zsh、fish、PowerShell、.env、Docker）",
                        ),
                        None,
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|app, event: &MouseDownEvent, _, cx| {
                            let ui = Ui::of(cx);
                            let items: Vec<MenuEntry> = ShellFormat::ALL
                                .iter()
                                .map(|&fmt| MenuEntry {
                                    title: format!(
                                        "{} {}",
                                        ui.t("Copy", "复制"),
                                        fmt.label()
                                    )
                                    .into(),
                                    hint: None,
                                    cmd: Cmd::CopyProxyEnv(fmt),
                                })
                                .collect();
                            super::this_menu_open(app, event.position, items, cx);
                        }),
                    ),
            );
        proxy_card = proxy_card.child(controls);

        // 3. Project-Grouped Runs
        let mut groups: BTreeMap<String, Vec<&crate::feed::RunSummary>> = BTreeMap::new();
        let mut group_order: Vec<String> = Vec::new();
        for run in session.runs.iter().take(60) {
            let proj = run.project_name();
            if !groups.contains_key(&proj) {
                group_order.push(proj.clone());
            }
            groups.entry(proj).or_default().push(run);
        }

        let mut runs_list = div()
            .id("sidebar-runs")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .px(px(6.0))
            .pb(px(10.0))
            .gap(px(2.0));

        if session.source.runs_dir().is_some() {
            let following = session.source.follows_latest();
            runs_list =
                runs_list.child(
                    div()
                        .id("sb-follow-latest")
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .h(px(26.0))
                        .px(px(8.0))
                        .rounded(px(5.0))
                        .when(following, |el| el.bg(theme::h(t.accent, 0.14)))
                        .cursor_pointer()
                        .hover(move |s| s.bg(theme::h(t.text, 0.05)))
                        .on_click(cx.listener(|app, _, window, cx| {
                            app.run_cmd(Cmd::FollowLatest, window, cx)
                        }))
                        .child(glyph::dot(
                            theme::h(if following { t.green } else { t.text_muted }, 1.0),
                            6.0,
                        ))
                        .child(
                            div()
                                .text_size(px(text::SM))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(if following { t.text } else { t.text_soft }))
                                .child(ui.t("Follow newest run", "跟随最新运行")),
                        ),
                );
        }

        let mut row_counter = 0usize;
        for proj in group_order {
            let Some(items) = groups.get(&proj) else {
                continue;
            };
            runs_list = runs_list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .pt(px(8.0))
                    .pb(px(2.0))
                    .px(px(6.0))
                    .child(icons::icon(
                        "icons/folder.svg",
                        12.0,
                        theme::h(t.text_muted, 0.9),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_size(px(text::XS))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(t.text_muted))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(proj),
                    ),
            );
            for run in items {
                row_counter += 1;
                let current = session.path.as_ref() == Some(&run.path);
                let dot_color = match run.report.as_ref() {
                    Some(report) => match report.exit_code {
                        Some(0) => t.green,
                        Some(_) => t.red,
                        None => t.text_muted,
                    },
                    None => t.amber,
                };
                let calls_str = run
                    .report
                    .as_ref()
                    .map(|r| r.calls.to_string())
                    .unwrap_or_else(|| "…".into());
                let path = run.path.clone();
                let run_id = run.id.clone();
                let is_pinned = run.pinned;
                let session_ent = self.session.clone();

                runs_list = runs_list.child(
                    div()
                        .id(ElementId::named_usize("sb-run", row_counter))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .h(px(28.0))
                        .px(px(8.0))
                        .rounded(px(5.0))
                        .when(current, |el| el.bg(theme::h(t.accent, 0.15)))
                        .cursor_pointer()
                        .hover(move |s| s.bg(theme::h(t.text, 0.06)))
                        .on_click(cx.listener(move |app, _, window, cx| {
                            app.run_cmd(Cmd::PinRun(path.clone()), window, cx)
                        }))
                        .child(glyph::dot(theme::h(dot_color, 1.0), 6.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .font(fonts::mono_font())
                                .text_size(px(text::SM))
                                .text_color(rgb(if current { t.text } else { t.text_soft }))
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(run.label()),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .font(fonts::mono_font())
                                .text_size(px(text::XS))
                                .text_color(rgb(t.text_faint))
                                .child(calls_str),
                        )
                        .child(
                            div()
                                .id(ElementId::named_usize("sb-pin", row_counter))
                                .flex_shrink_0()
                                .size(px(18.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.0))
                                .text_size(px(11.0))
                                .text_color(if is_pinned {
                                    theme::h(t.amber, 1.0)
                                } else {
                                    theme::h(t.text_faint, 0.65)
                                })
                                .hover(move |s| {
                                    s.bg(theme::h(t.text, 0.08)).text_color(rgb(t.amber))
                                })
                                .tooltip(widgets::tip(
                                    if is_pinned {
                                        ui.t("Unpin run", "取消置顶保留")
                                    } else {
                                        ui.t("Pin run (keep forever)", "置顶保留（不被自动清理）")
                                    },
                                    None,
                                ))
                                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                    cx.stop_propagation();
                                    let id = run_id.clone();
                                    session_ent.update(cx, |s, cx| {
                                        s.toggle_pin_run(id, !is_pinned, cx);
                                    });
                                })
                                .child(if is_pinned { "★" } else { "☆" }),
                        ),
                );
            }
        }

        div()
            .id("workspace-sidebar")
            .w(px(SIDEBAR_W))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(t.toolbar))
            .border_r_1()
            .border_color(rgb(t.border))
            .child(ws_card)
            .child(proxy_card)
            .child(
                div()
                    .px(px(12.0))
                    .pb(px(4.0))
                    .child(widgets::label(ui, ui.t("RUNS BY PROJECT", "项目运行记录"))),
            )
            .child(runs_list)
            .into_any_element()
    }
}
