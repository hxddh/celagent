//! Workbench chrome: 44px global header, 38px mode-linked sub-toolbar,
//! Run Switcher popover, and floating toast.

use super::*;
use crate::trace::{AccessFilter, StatusFilter};

impl LensletApp {
    /// Zone 1: Global Workbench Header (44px) — Run Switcher (left),
    /// Primary Mode Segmented Control (center), Live/Inspector/Settings (right).
    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> Div {
        let t = theme::p();

        let left = div()
            .flex()
            .items_center()
            .gap(px(9.0))
            .min_w(px(0.0))
            .child(glyph::lens_mark(23.0))
            .child(self.render_run_switcher_button(cx));

        let access = self.access_rows();
        let used_access = access.iter().filter(|e| !e.unused).count();
        let new_access = access.iter().filter(|e| e.new).count();
        let diff_changes = self
            .compute_active_diff()
            .map(|d| {
                d.new_failures.len()
                    + d.fixed_failures.len()
                    + d.access_added.len()
                    + d.access_removed.len()
            })
            .unwrap_or(0);

        let mode_switch = div()
            .flex()
            .items_center()
            .p(px(2.5))
            .gap(px(2.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(rgb(t.border))
            .bg(rgb(t.canvas))
            .child(self.mode_segment(
                "mode-calls",
                Pane::Calls,
                i18n::pick("Calls", "请求流"),
                Some(self.counts.total.to_string()),
                (self.counts.errors > 0).then_some(t.red),
                cx,
            ))
            .child(self.mode_segment(
                "mode-access",
                Pane::Access,
                i18n::pick("Access", "IAM 权限"),
                Some(if new_access > 0 {
                    format!("{used_access} +{new_access}")
                } else {
                    used_access.to_string()
                }),
                (new_access > 0).then_some(t.amber),
                cx,
            ))
            .child(self.mode_segment(
                "mode-diff",
                Pane::Diff,
                i18n::pick("Diff", "回归对比"),
                (diff_changes > 0).then(|| format!("Δ{diff_changes}")),
                (diff_changes > 0).then_some(t.accent),
                cx,
            ));

        let pause_icon = if self.paused {
            glyph::play_icon(theme::h(t.amber, 0.95)).into_any_element()
        } else {
            glyph::pause_icon(theme::h(t.text_dim, 0.95)).into_any_element()
        };
        let pause_label = if self.paused {
            if self.buffered.is_empty() {
                i18n::pick("Paused", "已暂停").to_string()
            } else {
                format!("{} ({})", i18n::pick("Resume", "继续"), self.buffered.len())
            }
        } else {
            i18n::pick("Pause", "暂停").to_string()
        };

        let right = div()
            .flex()
            .items_center()
            .gap(px(6.0))
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
                        theme::line(t.amber, 0.45)
                    } else {
                        theme::h(t.border, 1.0)
                    })
                    .bg(if self.paused {
                        theme::h(t.amber, 0.10)
                    } else {
                        theme::h(t.text, 0.02)
                    })
                    .text_size(px(11.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if self.paused {
                        rgb(t.amber)
                    } else {
                        rgb(t.text_dim)
                    })
                    .cursor_pointer()
                    .hover(move |s| s.bg(theme::h(t.text, 0.06)).text_color(rgb(t.text)))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_pause(cx)))
                    .child(pause_icon)
                    .child(pause_label),
            )
            .when(self.pane != Pane::Diff, |bar| {
                bar.child(
                    icon_pill(
                        "toggle-inspector",
                        self.prefs.inspector_open,
                        glyph::sidebar_icon(
                            if self.prefs.inspector_open {
                                theme::h(t.accent, 0.95)
                            } else {
                                theme::h(t.text_dim, 0.9)
                            },
                            self.prefs.inspector_open,
                        ),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_inspector(cx))),
                )
            })
            .child(
                icon_pill(
                    "open-settings",
                    self.settings_open,
                    glyph::gear_icon(if self.settings_open {
                        theme::h(t.accent, 0.95)
                    } else {
                        theme::h(t.text_dim, 0.9)
                    }),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.settings_open = !this.settings_open;
                    this.runs_popover_open = false;
                    cx.notify();
                })),
            );

        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(theme::HEADER_H))
            .px(px(14.0))
            .gap(px(12.0))
            .bg(rgb(t.header))
            .border_b_1()
            .border_color(rgb(t.border_subtle))
            .child(left)
            .child(mode_switch)
            .child(right)
    }

    fn render_run_switcher_button(&self, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let pos = self.run_position();
        let pinned = pos.is_some_and(|(idx, total)| idx < total);

        let mut trigger = div()
            .id("run-switcher")
            .flex()
            .items_center()
            .gap(px(7.0))
            .h(px(28.0))
            .px(px(9.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(if self.runs_popover_open {
                theme::h(t.accent, 1.0)
            } else if pinned {
                theme::line(t.amber, 0.45)
            } else {
                theme::h(t.border, 1.0)
            })
            .bg(if pinned {
                theme::h(t.amber, 0.07)
            } else {
                theme::h(t.text, 0.025)
            })
            .cursor_pointer()
            .hover(move |s| s.bg(theme::h(t.text, 0.06)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.runs_popover_open = !this.runs_popover_open;
                cx.notify();
            }));

        if let Some((idx, total)) = pos {
            trigger = trigger.child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(10.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if pinned { rgb(t.amber) } else { rgb(t.accent) })
                    .child(format!("#{idx}/{total}")),
            );
        } else if self.demo {
            trigger = trigger.child(badge("DEMO", t.amber));
        }

        if let Some(run) = self.run.as_ref() {
            let cmd = if run.header.command.is_empty() {
                i18n::pick("proxy session", "代理会话").to_string()
            } else {
                format!("$ {}", trace::ellipsize(&run.command_line(), 28))
            };
            trigger = trigger.child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(11.5))
                    .text_color(rgb(t.text))
                    .whitespace_nowrap()
                    .child(cmd),
            );
            if run.header.enforce {
                trigger = trigger.child(badge("ENFORCE", t.purple));
            }
            match run.exit_code() {
                Some(0) => trigger = trigger.child(badge("✓ 0", t.green)),
                Some(code) => trigger = trigger.child(badge(format!("✗ {code}"), t.red)),
                None if !run.ended() => {
                    trigger = trigger.child(badge(i18n::pick("LIVE", "运行中"), t.green))
                }
                None => {}
            }
        } else {
            trigger = trigger.child(
                div()
                    .text_size(px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(t.text_dim))
                    .child(i18n::pick("Waiting for run…", "等待运行会话…")),
            );
        }

        trigger = trigger.child(glyph::chevron_down(theme::h(t.text_muted, 0.9)));

        let mut wrap = div().flex().items_center().gap(px(5.0)).child(trigger);
        if pinned {
            wrap = wrap.child(
                action_button("follow-latest", i18n::pick("Latest →", "回到最新 →"))
                    .on_click(cx.listener(|this, _, _, cx| this.follow_latest_run(cx))),
            );
        }
        wrap
    }

    fn mode_segment(
        &self,
        id: &'static str,
        pane: Pane,
        label: &'static str,
        count: Option<String>,
        tint: Option<u32>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = theme::p();
        let active = self.pane == pane;
        let mut seg = div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(6.0))
            .h(px(26.0))
            .px(px(11.0))
            .rounded(px(5.0))
            .text_size(px(11.5))
            .font_weight(if active {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::MEDIUM
            })
            .bg(if active {
                rgb(t.panel_raised)
            } else {
                rgba(0x0000_0000)
            })
            .text_color(if active {
                rgb(t.text)
            } else {
                rgb(t.text_muted)
            })
            .cursor_pointer()
            .hover(move |s| s.text_color(rgb(t.text)))
            .on_click(cx.listener(move |this, _, _, cx| this.set_pane(pane, cx)))
            .child(label);
        if let Some(c) = count {
            seg = seg.child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(10.0))
                    .text_color(if let Some(color) = tint {
                        rgb(color)
                    } else if active {
                        rgb(t.accent)
                    } else {
                        rgb(t.text_faint)
                    })
                    .child(c),
            );
        }
        seg
    }

    /// Zone 2: Mode-Linked Contextual Sub-Toolbar (38px).
    /// Adapts dynamically to `Calls`, `Access`, or `Diff` so no dead controls exist.
    pub(super) fn render_subbar(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let search_width = if compact { 230.0 } else { 296.0 };

        let middle: AnyElement = match self.pane {
            Pane::Calls => self.render_calls_subbar(compact, cx).into_any_element(),
            Pane::Access => self.render_access_subbar(cx).into_any_element(),
            Pane::Diff => self.render_diff_subbar(cx).into_any_element(),
        };

        let conn = self.connection_indicator();

        let mut bar = div()
            .flex()
            .items_center()
            .h(px(theme::SUBBAR_H))
            .px(px(14.0))
            .gap(px(10.0))
            .bg(rgb(t.toolbar))
            .border_b_1()
            .border_color(rgb(t.border_subtle));

        if self.pane != Pane::Diff {
            bar = bar.child(
                div()
                    .w(px(search_width))
                    .flex_shrink_0()
                    .child(self.search.clone()),
            );
        }

        bar.child(middle).child(conn)
    }

    fn render_calls_subbar(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let limit = if compact { 3 } else { 5 };
        let visible = self.visible_facets(limit);
        let hidden = self.facets.len().saturating_sub(visible.len());

        let mut row = div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .flex_1()
            .min_w(px(0.0))
            .overflow_hidden()
            .child(self.status_chip(
                "filter-all",
                i18n::pick("All", "全部"),
                self.counts.total,
                None,
                StatusFilter::All,
                cx,
            ))
            .child(self.status_chip(
                "filter-errors",
                i18n::pick("Errors", "错误"),
                self.counts.errors,
                Some(t.red),
                StatusFilter::Errors,
                cx,
            ));

        if self.counts.blocked > 0 || self.call_filter.status == StatusFilter::Blocked {
            row = row.child(self.status_chip(
                "filter-blocked",
                i18n::pick("Blocked", "已拦截"),
                self.counts.blocked,
                Some(t.red),
                StatusFilter::Blocked,
                cx,
            ));
        }
        if self.counts.retries > 0 || self.call_filter.status == StatusFilter::Retries {
            row = row.child(self.status_chip(
                "filter-retries",
                i18n::pick("Retries", "重试"),
                self.counts.retries,
                Some(t.accent),
                StatusFilter::Retries,
                cx,
            ));
        }
        if self.counts.slow > 0 || self.call_filter.status == StatusFilter::Slow {
            row = row.child(self.status_chip(
                "filter-slow",
                i18n::pick("Slow", "慢请求"),
                self.counts.slow,
                Some(t.amber),
                StatusFilter::Slow,
                cx,
            ));
        }

        if !visible.is_empty() {
            row = row.child(div().w(px(1.0)).h(px(14.0)).mx(px(3.0)).bg(rgb(t.border)));
            for facet in visible {
                row = row.child(self.service_chip(facet, "svc", cx));
            }
        }

        if hidden > 0 || self.show_all_services {
            row = row.child(
                action_button(
                    "more-services",
                    if self.show_all_services {
                        i18n::pick("Less", "收起")
                    } else {
                        i18n::pick("More…", "更多…")
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.show_all_services = !this.show_all_services;
                    cx.notify();
                })),
            );
        }

        if let Some(access) = self.call_filter.access.as_ref() {
            row = row.child(
                div()
                    .id("filter-access-pill")
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(5.0))
                    .h(px(23.0))
                    .px(px(7.0))
                    .rounded(px(5.0))
                    .border_1()
                    .border_color(theme::line(t.accent, 0.5))
                    .bg(theme::h(t.accent, 0.10))
                    .font_family(fonts::mono())
                    .text_size(px(10.5))
                    .text_color(rgb(t.text))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.call_filter.access = None;
                        this.refresh_view();
                        cx.notify();
                    }))
                    .child(trace::ellipsize(&access.action, 26))
                    .child(div().text_color(rgb(t.text_muted)).child("×")),
            );
        }

        row
    }

    fn render_access_subbar(&self, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let all_rows = self.access_rows();
        let total = all_rows.len();
        let new_cnt = all_rows.iter().filter(|r| r.new).count();
        let imp_cnt = all_rows.iter().filter(|r| r.implied).count();
        let unused_cnt = all_rows.iter().filter(|r| r.unused).count();
        let test_cnt = all_rows.iter().filter(|r| r.test).count();

        let mut row = div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .flex_1()
            .min_w(px(0.0))
            .overflow_hidden()
            .child(self.access_chip(
                "acc-all",
                i18n::pick("All", "全部"),
                total,
                None,
                AccessFilter::All,
                cx,
            ))
            .child(self.access_chip(
                "acc-new",
                i18n::pick("New / Denied", "新增 / 越权"),
                new_cnt,
                Some(t.amber),
                AccessFilter::New,
                cx,
            ));

        if imp_cnt > 0 || self.access_filter == AccessFilter::Implied {
            row = row.child(self.access_chip(
                "acc-implied",
                i18n::pick("Implied", "隐式推导"),
                imp_cnt,
                Some(t.accent),
                AccessFilter::Implied,
                cx,
            ));
        }
        if unused_cnt > 0 || self.access_filter == AccessFilter::Unused {
            row = row.child(self.access_chip(
                "acc-unused",
                i18n::pick("Unused", "未触发"),
                unused_cnt,
                None,
                AccessFilter::Unused,
                cx,
            ));
        }
        if test_cnt > 0 || self.access_filter == AccessFilter::Test {
            row = row.child(self.access_chip(
                "acc-test",
                "# test",
                test_cnt,
                None,
                AccessFilter::Test,
                cx,
            ));
        }

        row = row.child(div().flex_1());
        if all_rows.iter().any(|r| !r.unused && !r.test) {
            row = row.child(
                action_button(
                    "copy-policy-json",
                    i18n::pick("Copy Policy JSON", "复制 IAM Policy JSON"),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    let json = trace::export_policy_json(&this.access_rows());
                    this.copy_text("policy JSON", json, cx);
                })),
            );
        }
        row
    }

    fn render_diff_subbar(&self, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let (baselines, prev_path) = self.finished_baselines_with_prev();
        let mut row = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .flex_1()
            .min_w(px(0.0))
            .overflow_hidden()
            .child(micro_label(i18n::pick("BASELINE RUN:", "对比基线运行：")));

        if baselines.is_empty() {
            row = row.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(t.text_muted))
                    .child(i18n::pick("none recorded yet", "暂无已完成的历史运行")),
            );
        } else {
            let active_base = self
                .diff_base
                .as_deref()
                .filter(|pinned| baselines.iter().any(|s| s.path.as_path() == *pinned))
                .or(prev_path.as_deref());
            for (i, base) in baselines.iter().take(5).enumerate() {
                let is_active = active_base == Some(base.path.as_path());
                let path_clone = base.path.clone();
                let is_prev = prev_path.as_deref() == Some(base.path.as_path());
                let label = if is_prev {
                    format!("{} ({})", base.id, i18n::pick("prev", "上一次"))
                } else {
                    base.id.clone()
                };
                row = row.child(
                    div()
                        .id(ElementId::named_usize("diff-base", i))
                        .flex()
                        .items_center()
                        .h(px(24.0))
                        .px(px(8.0))
                        .rounded(px(5.0))
                        .border_1()
                        .border_color(if is_active {
                            rgb(t.accent)
                        } else {
                            rgb(t.border_subtle)
                        })
                        .bg(if is_active {
                            theme::h(t.accent, 0.12)
                        } else {
                            theme::h(t.text, 0.02)
                        })
                        .font_family(fonts::mono())
                        .text_size(px(10.5))
                        .text_color(if is_active {
                            rgb(t.accent)
                        } else {
                            rgb(t.text_dim)
                        })
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.diff_base = Some(path_clone.clone());
                            cx.notify();
                        }))
                        .child(label),
                );
            }
        }
        row
    }

    /// Single source-of-truth connection & upstream indicator in the sub-toolbar.
    fn connection_indicator(&self) -> Div {
        let t = theme::p();
        let (dot_color, text, text_color) = if self.demo {
            (
                theme::h(t.amber, 0.9),
                "demo · :9297 → :9298".to_string(),
                rgb(t.amber),
            )
        } else {
            match &self.health {
                TraceHealth::Missing => (
                    theme::h(t.amber, 0.9),
                    i18n::pick("idle · ~/.lenslet/runs", "空闲 · ~/.lenslet/runs").to_string(),
                    rgb(t.text_muted),
                ),
                TraceHealth::Error(_) => (
                    theme::h(t.red, 0.95),
                    i18n::pick("trace error", "Trace 读取异常").to_string(),
                    rgb(t.red),
                ),
                TraceHealth::Ok => match self.run.as_ref() {
                    None => (
                        theme::h(t.text_faint, 0.8),
                        "idle".to_string(),
                        rgb(t.text_muted),
                    ),
                    Some(run) if run.ended() => {
                        let emu_port = RunInfo::address(&run.header.upstream)
                            .and_then(|a| a.rsplit(':').next())
                            .unwrap_or("9298");
                        let emu_state = match self.probe.emulator {
                            Some(true) => format!("emulator :{emu_port} up"),
                            Some(false) => format!("emulator :{emu_port} down"),
                            None => format!("emulator :{emu_port}"),
                        };
                        (
                            match self.probe.emulator {
                                Some(true) => theme::h(t.green, 0.85),
                                Some(false) => theme::h(t.red, 0.85),
                                None => theme::h(t.text_faint, 0.8),
                            },
                            emu_state,
                            rgb(t.text_muted),
                        )
                    }
                    Some(run) => {
                        let proxy_port = RunInfo::address(&run.header.endpoint)
                            .and_then(|a| a.rsplit(':').next())
                            .unwrap_or("?");
                        let emu_port = RunInfo::address(&run.header.upstream)
                            .and_then(|a| a.rsplit(':').next())
                            .unwrap_or("?");
                        let ok =
                            self.probe.proxy != Some(false) && self.probe.emulator != Some(false);
                        (
                            if ok {
                                theme::h(t.green, 0.95)
                            } else {
                                theme::h(t.red, 0.95)
                            },
                            format!(":{proxy_port} → :{emu_port}"),
                            if ok { rgb(t.text_dim) } else { rgb(t.red) },
                        )
                    }
                },
            }
        };

        div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(6.0))
            .font_family(fonts::mono())
            .text_size(px(10.5))
            .text_color(text_color)
            .child(glyph::dot(dot_color, 5.0))
            .child(text)
    }

    pub(super) fn render_service_strip(&self, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let chips: Vec<Stateful<Div>> = self
            .facets
            .iter()
            .map(|facet| self.service_chip(facet, "service-strip", cx))
            .collect();

        div()
            .flex()
            .items_center()
            .h(px(30.0))
            .px(px(14.0))
            .gap(px(8.0))
            .bg(rgb(t.strip))
            .border_b_1()
            .border_color(rgb(t.border_subtle))
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
                action_button("close-services", i18n::pick("Close", "关闭")).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.show_all_services = false;
                        cx.notify();
                    },
                )),
            )
    }

    /// Run Switcher Popover: lists recorded runs newest-first with status, stats, and `Follow Latest`.
    pub(super) fn render_runs_popover(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = theme::p();
        let summaries = match &self.source {
            Source::Runs { dir, .. } => trace::list_run_summaries(dir),
            Source::File(_) => Vec::new(),
        };
        let is_following_latest = matches!(&self.source, Source::Runs { pinned: None, .. });

        let mut list = div()
            .id("runs-popover-scroll")
            .max_h(px(380.0))
            .overflow_y_scroll()
            .scrollbar_width(px(5.0))
            .flex()
            .flex_col();

        list = list.child(
            div()
                .id("run-follow-latest-row")
                .flex()
                .items_center()
                .justify_between()
                .px(px(12.0))
                .py(px(8.0))
                .border_b_1()
                .border_color(rgb(t.border_subtle))
                .bg(if is_following_latest {
                    theme::h(t.accent, 0.10)
                } else {
                    theme::h(t.accent, 0.0)
                })
                .cursor_pointer()
                .hover(move |s| s.bg(rgb(t.row_hover)))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.follow_latest_run(cx);
                    this.runs_popover_open = false;
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(glyph::dot(theme::h(t.green, 0.95), 5.5))
                        .child(
                            div()
                                .text_size(px(12.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(t.text))
                                .child(i18n::pick(
                                    "Follow Latest Run Automatically",
                                    "自动跟随最新运行会话 (Live)",
                                )),
                        ),
                )
                .when(is_following_latest, |row| {
                    row.child(badge(i18n::pick("ACTIVE", "当前"), t.accent))
                }),
        );

        if summaries.is_empty() {
            list = list.child(
                div()
                    .p(px(14.0))
                    .text_size(px(11.5))
                    .text_color(rgb(t.text_muted))
                    .child(if self.demo {
                        i18n::pick(
                            "Static demo dataset (`LENSLET_DEMO=1`).",
                            "当前为静态演示数据 (`LENSLET_DEMO=1`)。",
                        )
                    } else {
                        i18n::pick(
                            "No runs recorded in ~/.lenslet/runs yet.",
                            "~/.lenslet/runs 中暂无历史运行记录。",
                        )
                    }),
            );
        } else {
            let total = summaries.len();
            for (idx, item) in summaries.into_iter().enumerate() {
                let number = total - idx;
                let selected = self.trace_path.as_deref() == Some(item.path.as_path());
                let target_path = item.path.clone();
                let is_newest = idx == 0;
                let status_badge = match &item.report {
                    Some(rep) => match rep.exit_code {
                        Some(0) => badge("exit 0", t.green),
                        Some(code) => badge(format!("exit {code}"), t.red),
                        None => badge("stopped", t.text_muted),
                    },
                    None => badge("live", t.accent),
                };
                let stats = match &item.report {
                    Some(rep) => format!(
                        "{} calls · {} err · {}",
                        rep.calls,
                        rep.errors,
                        format_latency(rep.duration_ms)
                    ),
                    None => short_clock(&item.header.started),
                };

                list = list.child(
                    div()
                        .id(ElementId::named_usize("run-item", idx))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(10.0))
                        .px(px(12.0))
                        .py(px(8.0))
                        .border_b_1()
                        .border_color(rgb(t.border_subtle))
                        .bg(if selected {
                            rgb(t.row_selected)
                        } else {
                            rgba(0x0000_0000)
                        })
                        .cursor_pointer()
                        .hover(move |s| s.bg(rgb(t.row_hover)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if is_newest {
                                this.follow_latest_run(cx);
                            } else {
                                this.pin_run(target_path.clone(), cx);
                            }
                            this.runs_popover_open = false;
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.0))
                                        .child(
                                            div()
                                                .font_family(fonts::mono())
                                                .text_size(px(10.5))
                                                .text_color(rgb(t.text_muted))
                                                .child(format!("#{number}")),
                                        )
                                        .child(
                                            div()
                                                .font_family(fonts::mono())
                                                .text_size(px(11.5))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(rgb(t.text))
                                                .whitespace_nowrap()
                                                .text_ellipsis()
                                                .overflow_hidden()
                                                .child(trace::ellipsize(&item.label(), 36)),
                                        ),
                                )
                                .child(
                                    div()
                                        .font_family(fonts::mono())
                                        .text_size(px(10.0))
                                        .text_color(rgb(t.text_faint))
                                        .child(format!("{} · {stats}", item.id)),
                                ),
                        )
                        .child(status_badge),
                );
            }
        }

        let card = div()
            .id("runs-popover-card")
            .absolute()
            .top(px(theme::HEADER_H + 4.0))
            .left(px(14.0))
            .w(px(440.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(t.border_strong))
            .bg(rgb(t.panel))
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(list);

        div()
            .id("runs-popover-backdrop")
            .absolute()
            .inset_0()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.runs_popover_open = false;
                    cx.notify();
                }),
            )
            .child(card)
    }

    pub(super) fn render_floating_toast(&self) -> Option<Div> {
        let (message, _) = self.toast.as_ref()?;
        let t = theme::p();
        Some(
            div()
                .absolute()
                .bottom(px(14.0))
                .right(px(16.0))
                .flex()
                .items_center()
                .gap(px(7.0))
                .h(px(28.0))
                .px(px(12.0))
                .rounded_full()
                .border_1()
                .border_color(theme::line(t.green, 0.45))
                .bg(rgb(t.panel))
                .font_family(fonts::mono())
                .text_size(px(11.0))
                .text_color(rgb(t.text))
                .child(glyph::dot(theme::h(t.green, 0.95), 5.0))
                .child(message.clone()),
        )
    }

    pub(super) fn visible_facets(&self, limit: usize) -> Vec<&ServiceFacet> {
        let mut visible: Vec<&ServiceFacet> = self.facets.iter().take(limit).collect();
        if let Some(active) = self.call_filter.service.as_deref()
            && !visible.iter().any(|facet| facet.id == active)
            && let Some(facet) = self.facets.iter().find(|facet| facet.id == active)
        {
            if !visible.is_empty() {
                visible.pop();
            }
            visible.push(facet);
        }
        visible
    }

    fn status_chip(
        &self,
        id: &'static str,
        label: &'static str,
        count: usize,
        dot: Option<u32>,
        status: StatusFilter,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let active = if status == StatusFilter::All {
            !self.call_filter.is_active()
        } else {
            self.call_filter.status == status
        };
        filter_chip_base(id, label, count, dot, active).on_click(cx.listener(
            move |this, _, _, cx| {
                if status == StatusFilter::All {
                    this.call_filter = trace::CallFilter::default();
                } else if this.call_filter.status == status {
                    this.call_filter.status = StatusFilter::All;
                } else {
                    this.call_filter.status = status;
                }
                this.refresh_view();
                this.reveal_selected();
                cx.notify();
            },
        ))
    }

    fn access_chip(
        &self,
        id: &'static str,
        label: &'static str,
        count: usize,
        dot: Option<u32>,
        filter: AccessFilter,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let active = self.access_filter == filter;
        filter_chip_base(id, label, count, dot, active).on_click(cx.listener(
            move |this, _, _, cx| {
                this.access_filter = if this.access_filter == filter {
                    AccessFilter::All
                } else {
                    filter
                };
                cx.notify();
            },
        ))
    }

    pub(super) fn service_chip(
        &self,
        facet: &ServiceFacet,
        scope: &'static str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = theme::p();
        let active = self.call_filter.service.as_deref() == Some(facet.id.as_str());
        let color = theme::service_color(&facet.label);
        let svc_id = facet.id.clone();
        div()
            .id(ElementId::Name(format!("{scope}-{}", facet.id).into()))
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(5.0))
            .h(px(24.0))
            .px(px(7.0))
            .rounded(px(5.0))
            .border_1()
            .border_color(if active {
                theme::tint(color, 0.60)
            } else {
                theme::h(t.border, 0.0)
            })
            .bg(if active {
                theme::h(t.text, 0.07)
            } else {
                theme::h(t.text, 0.0)
            })
            .text_size(px(11.0))
            .font_weight(if active {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::NORMAL
            })
            .text_color(if active {
                rgb(t.text)
            } else {
                rgb(t.text_muted)
            })
            .cursor_pointer()
            .hover(move |s| s.bg(theme::h(t.text, 0.05)).text_color(rgb(t.text_soft)))
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.call_filter.service.as_deref() == Some(svc_id.as_str()) {
                    this.call_filter.service = None;
                } else {
                    this.call_filter.service = Some(svc_id.clone());
                }
                this.refresh_view();
                this.reveal_selected();
                cx.notify();
            }))
            .child(glyph::dot(color, 4.5))
            .child(div().whitespace_nowrap().child(facet.label.clone()))
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(10.0))
                    .text_color(rgb(t.text_faint))
                    .child(facet.count.to_string()),
            )
    }
}

fn filter_chip_base(
    id: &'static str,
    label: &'static str,
    count: usize,
    dot: Option<u32>,
    active: bool,
) -> Stateful<Div> {
    let t = theme::p();
    let mut chip = div()
        .id(id)
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(5.0))
        .h(px(24.0))
        .px(px(7.0))
        .rounded(px(5.0))
        .border_1()
        .border_color(if active {
            rgb(t.accent)
        } else {
            rgba(0x0000_0000)
        })
        .bg(if active {
            theme::h(t.accent, 0.12)
        } else {
            theme::h(t.accent, 0.0)
        })
        .text_size(px(11.0))
        .font_weight(if active {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        })
        .text_color(if active {
            rgb(t.text)
        } else {
            rgb(t.text_muted)
        })
        .cursor_pointer()
        .hover(move |s| s.bg(theme::h(t.text, 0.05)).text_color(rgb(t.text_soft)));

    if let Some(color) = dot {
        chip = chip.child(glyph::dot(theme::h(color, 0.9), 4.5));
    }
    chip.child(div().child(label)).child(
        div()
            .font_family(fonts::mono())
            .text_size(px(10.0))
            .text_color(if let Some(c) = dot.filter(|_| count > 0) {
                rgb(c)
            } else {
                rgb(t.text_faint)
            })
            .child(count.to_string()),
    )
}

fn icon_pill(id: &'static str, active: bool, icon: impl IntoElement) -> Stateful<Div> {
    let t = theme::p();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .w(px(28.0))
        .h(px(26.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(if active { rgb(t.accent) } else { rgb(t.border) })
        .bg(if active {
            theme::h(t.accent, 0.12)
        } else {
            theme::h(t.text, 0.02)
        })
        .cursor_pointer()
        .hover(move |s| s.bg(theme::h(t.text, 0.06)))
        .child(icon)
}
