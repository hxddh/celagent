//! The request stream: table, rows, empty states.

use super::*;

impl LensletApp {
    pub(super) fn render_stream(&mut self, op_width: f32, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        if self.pane == Pane::Access && !self.rows.is_empty() {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.0))
                .bg(rgb(t.panel))
                .border_r_1()
                .border_color(rgb(t.border))
                .child(self.render_stream_head(cx))
                .child(self.render_access(cx))
                .child(self.render_stream_footer());
        }
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
            .bg(rgb(t.panel))
            .border_r_1()
            .border_color(rgb(t.border))
            .child(self.render_stream_head(cx))
            .child(self.render_table_head(op_width))
            .child(body)
            .child(self.render_stream_footer())
    }

    pub(super) fn render_stream_head(&self, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let filtered = !self.query.is_empty() || self.filter != Filter::All;
        let access = self.access_rows();
        let used_access = access.iter().filter(|entry| !entry.unused).count();
        let new_access = access.iter().filter(|entry| entry.new).count();
        let unused_access = access.iter().filter(|entry| entry.unused).count();
        let counts = if self.pane == Pane::Access {
            let mut text = if i18n::is_zh() {
                format!("{used_access} 项权限")
            } else {
                format!(
                    "{used_access} {}",
                    if used_access == 1 {
                        "PERMISSION"
                    } else {
                        "PERMISSIONS"
                    }
                )
            };
            if new_access > 0 {
                text.push_str(&format!(" · {new_access} NEW"));
            }
            if unused_access > 0 {
                text.push_str(&format!(" · {unused_access} UNUSED"));
            }
            text
        } else if filtered {
            if i18n::is_zh() {
                format!(
                    "显示 {} / {} · {}",
                    self.counts.visible,
                    self.counts.total,
                    i18n::error_count(self.counts.visible_errors)
                )
            } else {
                format!(
                    "{} OF {} CALLS · {}",
                    self.counts.visible,
                    self.counts.total,
                    i18n::error_count(self.counts.visible_errors)
                )
            }
        } else if let Some(reported) = self.counts.reported_total {
            if i18n::is_zh() {
                format!(
                    "最近 {} / {} 次 · {}",
                    self.counts.total,
                    reported,
                    i18n::error_count(self.counts.errors)
                )
            } else {
                format!(
                    "LATEST {} OF {} CALLS · {}",
                    self.counts.total,
                    reported,
                    i18n::error_count(self.counts.errors)
                )
            }
        } else {
            let mut text = format!(
                "{} · {}",
                i18n::call_count(self.counts.total),
                i18n::error_count(self.counts.errors)
            );
            if self.counts.retries > 0 {
                text.push_str(&format!(
                    " · {} {}",
                    self.counts.retries,
                    if self.counts.retries == 1 {
                        "retry"
                    } else {
                        "retries"
                    }
                ));
            }
            if self.counts.blocked > 0 {
                text.push_str(&format!(" · {} blocked", self.counts.blocked));
            }
            text
        };

        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(theme::STREAM_HEAD_H))
            .px(px(theme::ROW_PAD_X))
            .border_b_1()
            .border_color(rgb(t.border_subtle))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(self.pane_tab(Pane::Calls, i18n::pick("CALLS", "请求流"), None, cx))
                    .child(self.pane_tab(
                        Pane::Access,
                        i18n::pick("ACCESS", "IAM 权限"),
                        (new_access > 0).then(|| format!("+{new_access}")),
                        cx,
                    )),
            )
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(10.0))
                    .text_color(rgb(t.text_faint))
                    .child(counts),
            )
    }

    pub(super) fn render_table_head(&self, op_width: f32) -> Div {
        let t = theme::p();
        div()
            .flex()
            .items_center()
            .h(px(theme::TABLE_HEAD_H))
            .px(px(theme::ROW_PAD_X))
            .gap(px(theme::COL_GAP))
            .bg(rgb(t.table_head))
            .border_b_1()
            .border_color(rgb(t.border_subtle))
            .child(table_label(i18n::pick("TIME", "时间"), theme::COL_TIME))
            .child(table_label("SERVICE", theme::COL_SERVICE))
            .child(div().w(px(op_width)).child(micro_label("OPERATION / PATH")))
            .child(table_label("STATUS", theme::COL_STATUS))
            .child(
                div()
                    .w(px(theme::COL_LATENCY))
                    .text_right()
                    .child(micro_label(i18n::pick("LATENCY", "耗时"))),
            )
            .child(
                div()
                    .w(px(theme::COL_AUTH))
                    .text_right()
                    .child(micro_label("AUTH")),
            )
    }

    pub(super) fn render_rows(&mut self, op_width: f32, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let rows = self.rows.clone();
        let view = self.view.clone();
        let selected = self.selected;
        let run_start_ms = self.run_start_ms();
        let this = cx.entity().downgrade();
        let list = uniform_list("trace-rows", view.len(), move |range, _window, _cx| {
            range
                .map(|position| {
                    let raw = view[position];
                    let row = rows[raw].clone();
                    let prev_end_ms = raw.checked_sub(1).and_then(|prev| {
                        rows.get(prev)
                            .and_then(|r| r.at_ms().map(|at| at + r.ms as i64))
                    });
                    trace_row(
                        &row,
                        raw,
                        selected == Some(raw),
                        op_width,
                        run_start_ms,
                        prev_end_ms,
                        this.clone(),
                    )
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
                        linear_color_stop(theme::h(t.panel, 0.0), 0.0),
                        linear_color_stop(theme::h(t.panel, 0.94), 1.0),
                    )),
            )
    }

    pub(super) fn render_first_run(&mut self, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let (title, description) = match &self.health {
            TraceHealth::Missing => (
                i18n::pick("Run your code through Lenslet", "通过 Lenslet 运行你的程序"),
                i18n::pick(
                    "Start AWS Emulator, then run your program, tests or script with the command below. Every AWS call it makes appears here, with the reason when one fails.",
                    "先启动本地 AWS Emulator，再用下方命令运行程序、测试或脚本。每条 AWS 调用都会实时显示在这里，并在失败时给出原因。",
                ),
            ),
            TraceHealth::Error(error) => (
                i18n::pick("Can't read the trace file", "无法读取 Trace 文件"),
                error.as_str(),
            ),
            TraceHealth::Ok => (
                i18n::pick("No calls captured yet", "尚未捕获到请求"),
                i18n::pick(
                    "The run is live, but nothing has called AWS through it. SDKs that honor AWS_ENDPOINT_URL are routed automatically; others need the endpoint set by hand.",
                    "当前会话已连接，但尚未收到 AWS 调用。支持 AWS_ENDPOINT_URL 的 SDK 会自动路由至此。",
                ),
            ),
        };
        let command = match self.run.as_ref() {
            Some(run) if !run.ended() && !run.header.endpoint.is_empty() => {
                run.header.endpoint.clone()
            }
            _ => START_COMMAND.to_string(),
        };

        let endpoint = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .h(px(30.0))
            .px(px(9.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(t.border))
            .bg(rgb(t.code))
            .child(glyph::dot(theme::h(t.green, 0.9), 5.0))
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(12.0))
                    .text_color(rgb(t.text_soft))
                    .child(command.clone()),
            )
            .child(div().flex_1())
            .child(self.copy_button("copy-endpoint", "command", Some(command), cx));

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
                    .max_w(px(460.0))
                    .child(glyph::lens_mark(38.0))
                    .child(
                        div()
                            .pt(px(2.0))
                            .text_size(px(14.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(t.text))
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .line_height(px(17.0))
                            .text_color(rgb(t.text_muted))
                            .text_center()
                            .child(description.to_string()),
                    )
                    .child(div().h(px(2.0)))
                    .child(endpoint)
                    .child(
                        div()
                            .font_family(fonts::mono())
                            .text_size(px(10.0))
                            .text_color(rgb(t.text_faint))
                            .child(format!(
                                "{} {}",
                                i18n::pick("watching", "监听"),
                                display_trace_path(self.trace_path.as_deref())
                            )),
                    )
                    .when(self.demo, |this| {
                        this.child(
                            div()
                                .text_size(px(10.5))
                                .text_color(rgb(t.amber))
                                .child("Demo data · LENSLET_DEMO=1"),
                        )
                    }),
            )
    }

    pub(super) fn render_no_matches(&mut self, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let query = if self.query.is_empty() {
            None
        } else {
            Some(format!("\"{}\"", trace::clip_utf8(&self.query, 40)))
        };
        let mut description =
            i18n::pick("Try a different search", "请尝试其他搜索关键词").to_string();
        if let Filter::Access(access) = &self.filter {
            description = if self.counts.reported_total.is_some() {
                if i18n::is_zh() {
                    format!(
                        "使用 {} 的早期请求已超出最近 {} 条窗口",
                        access.action, self.counts.total
                    )
                } else {
                    format!(
                        "Earlier calls that needed {} fell outside the latest {}-call window",
                        access.action, self.counts.total
                    )
                }
            } else if i18n::is_zh() {
                format!("当前没有请求使用 {}", access.action)
            } else {
                format!("No calls needed {} here", access.action)
            };
        } else if let Filter::Service(id) = &self.filter {
            description = if i18n::is_zh() {
                format!("当前没有 {id} 的请求")
            } else {
                format!("No calls for the {id} filter")
            };
        } else if self.filter == Filter::Errors {
            description = i18n::pick(
                "No error calls in the current trace",
                "当前 Trace 中没有错误请求",
            )
            .to_string();
        } else if self.filter == Filter::Blocked {
            description = i18n::pick(
                "No blocked calls in the current trace",
                "当前 Trace 中没有被拦截的请求",
            )
            .to_string();
        }
        if let Some(query) = query {
            description = if i18n::is_zh() {
                format!("没有匹配 {query} 的结果。")
            } else {
                format!("Nothing matches {query}.")
            };
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
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(t.text_soft))
                    .child(i18n::pick("No calls match", "没有匹配的请求")),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(t.text_muted))
                    .text_center()
                    .child(description),
            )
            .child(
                action_button(
                    "clear-filters",
                    i18n::pick("Clear search & filters", "清除搜索与筛选"),
                )
                .on_click(
                    cx.listener(|this, _, window, cx| this.clear_search_and_filter(window, cx)),
                ),
            )
    }

    pub(super) fn render_stream_footer(&self) -> Div {
        let t = theme::p();
        let (dot, text) = if self.demo && !self.paused {
            (
                theme::h(t.amber, 0.9),
                i18n::pick("demo data · static", "演示数据 · 静态").to_string(),
            )
        } else if self.paused {
            (
                theme::h(t.amber, 0.95),
                if self.buffered.is_empty() {
                    i18n::pick("paused", "已暂停").to_string()
                } else if i18n::is_zh() {
                    format!("已暂停 · 缓存 {}", self.buffered.len())
                } else {
                    format!("paused · {} buffered", self.buffered.len())
                },
            )
        } else {
            match &self.health {
                TraceHealth::Missing => (
                    theme::h(t.amber, 0.9),
                    if i18n::is_zh() {
                        format!("等待运行记录 {}", display_trace_path(None))
                    } else {
                        format!("waiting for a run in {}", display_trace_path(None))
                    },
                ),
                TraceHealth::Error(error) => {
                    (theme::h(t.red, 0.9), format!("trace error · {error}"))
                }
                TraceHealth::Ok => (
                    self.proxy_light().dot,
                    match self
                        .run
                        .as_ref()
                        .filter(|run| !run.header.command.is_empty())
                    {
                        Some(run) => format!(
                            "$ {} · {}",
                            trace::ellipsize(&run.command_line(), 48),
                            display_trace_path(self.trace_path.as_deref())
                        ),
                        None => format!(
                            "{} {}",
                            i18n::pick("watching", "监听"),
                            display_trace_path(self.trace_path.as_deref())
                        ),
                    },
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
                    .text_size(px(10.0))
                    .text_color(rgb(t.text_muted))
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(text),
            );
        if self.skipped > 0 {
            left = left.child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(10.0))
                    .text_color(rgb(t.amber))
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
            .border_color(rgb(t.border_subtle))
            .bg(rgb(t.strip))
            .child(left)
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn trace_row(
    trace: &Trace,
    raw_index: usize,
    selected: bool,
    op_width: f32,
    run_start_ms: Option<i64>,
    prev_end_ms: Option<i64>,
    this: WeakEntity<LensletApp>,
) -> Stateful<Div> {
    let t = theme::p();
    let service_color = theme::service_color(trace.service_name());
    let idle_gap_ms = match (trace.at_ms(), prev_end_ms) {
        (Some(at), Some(prev_end)) if at - prev_end >= 500 => Some((at - prev_end) as u64),
        _ => None,
    };
    let time_sub = if let Some(gap) = idle_gap_ms {
        Some((format!("Δ {}", format_latency(gap)), rgb(t.amber)))
    } else {
        match (trace.at_ms(), run_start_ms) {
            (Some(at), Some(start)) => Some((trace::format_offset(at - start), rgb(t.text_faint))),
            _ => None,
        }
    };
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
        .border_color(rgb(t.border_subtle))
        .bg(if selected {
            rgb(t.row_selected)
        } else {
            rgb(t.row)
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
                    rgb(t.accent)
                } else if trace.blocked {
                    rgb(t.red)
                } else {
                    rgba(0x00000000)
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
                        .text_size(px(12.0))
                        .text_color(rgb(t.text_muted))
                        .child(short_clock(&trace.at)),
                )
                .when_some(time_sub, |col, (label, color)| {
                    col.child(div().text_size(px(10.0)).text_color(color).child(label))
                }),
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
                        .text_size(px(11.0))
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
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .min_w(px(0.0))
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(t.text))
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .overflow_hidden()
                                .child(operation_label(trace).to_string()),
                        )
                        .when(trace.blocked, |title| title.child(blocked_pill("BLOCKED")))
                        .when_some(trace.retry_badge(), |title, badge| {
                            title.child(retry_pill(badge))
                        })
                        .when(trace.new && !trace.blocked, |title| {
                            title.child(new_pill("NEW ACCESS"))
                        }),
                )
                .child(row_detail(trace, op_width)),
        )
        .child(
            div()
                .w(px(theme::COL_STATUS))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(5.0))
                .child(glyph::dot(
                    if trace.pending {
                        rgb(t.amber)
                    } else {
                        status_color(trace.status)
                    },
                    4.5,
                ))
                .child(
                    div()
                        .font_family(fonts::mono())
                        .text_size(px(12.0))
                        .text_color(if trace.pending {
                            rgb(t.amber)
                        } else if trace.is_error() {
                            status_color(trace.status)
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
                .text_right()
                .font_family(fonts::mono())
                .text_size(px(12.0))
                .text_color(if trace.pending {
                    rgb(t.amber)
                } else {
                    latency_color(trace.ms)
                })
                .child(if trace.pending {
                    "pending".to_string()
                } else {
                    format_latency(trace.ms)
                }),
        )
        .child(
            div()
                .w(px(theme::COL_AUTH))
                .flex_shrink_0()
                .flex()
                .justify_end()
                .child(glyph::auth_mark(trace.authorization || trace.presigned())),
        );

    if selected {
        row.hover(move |style| style.bg(rgb(t.row_selected_hover)))
    } else {
        row.hover(move |style| style.bg(rgb(t.row_hover)))
    }
}

/// The row's second line: the AWS error when the call failed, otherwise the
/// method and path.
fn row_detail(trace: &Trace, op_width: f32) -> Div {
    let t = theme::p();
    let line = div().flex().items_center().gap(px(6.0)).min_w(px(0.0));
    if let Some(problem) = trace.problem.as_ref() {
        let message = problem.message.as_deref().unwrap_or("");
        return line
            .child(
                div()
                    .flex_shrink_0()
                    .font_family(fonts::mono())
                    .text_size(px(12.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(status_color(trace.status.max(400)))
                    .child(trace::ellipsize(&problem.code, 40)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_size(px(12.5))
                    .text_color(rgb(t.text_dim))
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .overflow_hidden()
                    .child(message.to_string()),
            );
    }
    line.child(
        div()
            .font_family(fonts::mono())
            .text_size(px(10.5))
            .text_color(if status_is_success(trace.status) || trace.pending {
                rgb(t.text_muted)
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
            .text_size(px(12.0))
            .text_color(rgb(t.text_muted))
            .min_w(px(0.0))
            .whitespace_nowrap()
            .text_ellipsis()
            .overflow_hidden()
            .child(trace::display_path(trace).to_string()),
    )
}

impl LensletApp {
    /// One segment of the CALLS / ACCESS switch (key: `a`).
    fn pane_tab(
        &self,
        pane: Pane,
        label: &'static str,
        badge: Option<String>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = theme::p();
        let active = self.pane == pane;
        let id = match pane {
            Pane::Calls => "pane-calls",
            Pane::Access => "pane-access",
        };
        let mut tab = div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(5.0))
            .h(px(22.0))
            .px(px(8.0))
            .rounded(px(5.0))
            .bg(if active {
                theme::h(t.text, 0.07)
            } else {
                theme::h(0x000000, 0.0)
            })
            .text_size(px(10.0))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(if active {
                rgb(t.text)
            } else {
                rgb(t.text_muted)
            })
            .cursor_pointer()
            .hover(move |style| style.text_color(rgb(t.text_soft)))
            .on_click(cx.listener(move |this, _, _, cx| this.set_pane(pane, cx)))
            .child(label);
        if let Some(badge) = badge {
            tab = tab.child(new_pill(badge));
        }
        tab
    }
}

/// The small amber marker for access outside the lock file.
pub(super) fn new_pill(text: impl Into<SharedString>) -> Div {
    let t = theme::p();
    div()
        .h(px(15.0))
        .px(px(4.0))
        .flex()
        .items_center()
        .rounded(px(3.0))
        .border_1()
        .border_color(theme::line(t.amber, 0.4))
        .bg(theme::h(t.amber, 0.1))
        .font_family(fonts::mono())
        .text_size(px(9.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(t.amber))
        .child(text.into())
}

pub(super) fn blocked_pill(text: impl Into<SharedString>) -> Div {
    let t = theme::p();
    div()
        .h(px(15.0))
        .px(px(4.0))
        .flex()
        .items_center()
        .rounded(px(3.0))
        .border_1()
        .border_color(theme::line(t.red, 0.45))
        .bg(theme::h(t.red, 0.12))
        .font_family(fonts::mono())
        .text_size(px(9.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(t.red))
        .child(text.into())
}

fn retry_pill(text: impl Into<SharedString>) -> Div {
    let t = theme::p();
    div()
        .h(px(15.0))
        .px(px(4.0))
        .flex()
        .items_center()
        .rounded(px(3.0))
        .border_1()
        .border_color(theme::line(t.accent, 0.38))
        .bg(theme::h(t.accent, 0.09))
        .font_family(fonts::mono())
        .text_size(px(9.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(t.accent))
        .child(text.into())
}
