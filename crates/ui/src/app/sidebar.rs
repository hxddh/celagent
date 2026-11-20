//! Collapsible Workspace Sidebar (⌘0):
//! 1. Local Proxy Control Plane — live status (`/_lenslet/health`), detected
//!    local emulators, 1-click Start/Stop Proxy, Copy Shell Env, and Open File.
//! 2. Project-Grouped Runs — groups recorded runs by workspace directory with
//!    one-click switching and `★` pin protection against retention pruning.

use std::collections::BTreeMap;

use gpui::{AnyElement, Context, ElementId, FontWeight, MouseButton, div, prelude::*, px, rgb};

use super::{Cmd, LensletApp, widgets};
use crate::{
    fonts, glyph, i18n, icons,
    theme::{self, Ui, text},
};

pub const SIDEBAR_W: f32 = 240.0;

impl LensletApp {
    pub(super) fn render_sidebar(&self, ui: &Ui, cx: &mut Context<Self>) -> AnyElement {
        let t = ui.p;
        let session = self.session.read(cx);
        let env = &self.env_probe;
        let managed_running = self.managed_proxy.is_some();

        let (status_label, status_color) = if env.running {
            (
                format!("{} :{}", ui.t("PROXY LIVE", "代理运行中"), self.proxy_port),
                t.green,
            )
        } else if !env.emulators.is_empty() {
            (
                format!("{} ({})", ui.t("READY", "就绪"), env.emulators[0].name),
                t.amber,
            )
        } else {
            (ui.t("PROXY IDLE", "代理空闲").into(), t.text_muted)
        };

        let mut proxy_card = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
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
            let upstream = env
                .upstream
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("auto");
            proxy_card = proxy_card.child(
                div()
                    .font(fonts::mono_font())
                    .text_size(px(text::XS))
                    .text_color(rgb(t.text_muted))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(format!("→ {upstream}")),
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
        }

        let mut controls = div().flex().flex_wrap().items_center().gap(px(6.0));
        if managed_running {
            controls = controls.child(
                widgets::danger_button(ui, "sb-stop-proxy", ui.t("Stop", "停止")).on_click(
                    cx.listener(|app, _, window, cx| app.run_cmd(Cmd::StopProxy, window, cx)),
                ),
            );
        } else if !env.running {
            controls = controls.child(
                widgets::primary_button(ui, "sb-start-proxy", ui.t("Start Proxy", "启动代理"))
                    .on_click(
                        cx.listener(|app, _, window, cx| app.run_cmd(Cmd::StartProxy, window, cx)),
                    ),
            );
        }
        controls = controls
            .child(
                widgets::button(ui, "sb-copy-env", ui.t("Copy Env", "复制环境变量"))
                    .tooltip(widgets::tip(
                        ui.t(
                            "Copy export AWS_ENDPOINT_URL=… for your terminal",
                            "复制终端所需的 export AWS_ENDPOINT_URL=…",
                        ),
                        None,
                    ))
                    .on_click(
                        cx.listener(|app, _, window, cx| {
                            app.run_cmd(Cmd::CopyProxyEnv, window, cx)
                        }),
                    ),
            )
            .child(
                widgets::button(ui, "sb-open-file", ui.t("Open…", "打开…"))
                    .tooltip(widgets::tip(
                        ui.t("Open a .jsonl trace file", "打开 .jsonl 追踪文件"),
                        Some(i18n::shortcut("O")),
                    ))
                    .on_click(
                        cx.listener(|app, _, window, cx| app.run_cmd(Cmd::OpenFile, window, cx)),
                    ),
            );
        proxy_card = proxy_card.child(controls);

        // Group runs by project workspace name while preserving chronological order within each group.
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
                    .child(icons::icon("folder", 12.0, theme::h(t.text_muted, 0.9)))
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
