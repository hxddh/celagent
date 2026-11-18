//! The request stream: table header, virtualized rows, and empty states.

use super::*;
use crate::prefs::{Density, TimeFormat};

impl LensletApp {
    pub(super) fn render_stream(&mut self, op_width: f32, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        // Note: `Pane::Access` works even when `rows.is_empty()` (e.g. showing UNUSED lock entries).
        if self.pane == Pane::Access {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.0))
                .bg(rgb(t.panel))
                .child(self.render_access(cx));
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
            .child(self.render_table_head(op_width))
            .child(body)
    }

    pub(super) fn render_table_head(&self, op_width: f32) -> Div {
        let t = theme::p();
        let filtered = !self.query.is_empty() || self.call_filter.is_active();
        let count_hint = if filtered {
            format!("{}/{}", self.counts.visible, self.counts.total)
        } else {
            String::new()
        };

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
            .child(
                div()
                    .w(px(op_width))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(micro_label("OPERATION / PATH"))
                    .when(!count_hint.is_empty(), |col| {
                        col.child(
                            div()
                                .font_family(fonts::mono())
                                .text_size(px(9.5))
                                .text_color(rgb(t.accent))
                                .child(count_hint),
                        )
                    }),
            )
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
        let density = self.prefs.density;
        let time_format = self.prefs.time_format;
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
                        density,
                        time_format,
                        this.clone(),
                    )
                })
                .collect()
        })
        .w_full()
        .flex_1()
        .min_h(px(0.0))
        .track_scroll(self.scroll.clone());

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
                    .h(px(14.0))
                    .bg(linear_gradient(
                        0.0,
                        linear_color_stop(theme::h(t.panel, 0.0), 0.0),
                        linear_color_stop(theme::h(t.panel, 0.92), 1.0),
                    )),
            )
    }

    pub(super) fn render_first_run(&mut self, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let (title, description) = match &self.health {
            TraceHealth::Missing => (
                i18n::pick("Run your code through Lenslet", "通过 Lenslet 运行你的程序"),
                i18n::pick(
                    "Start your local AWS emulator, then run your program, tests, or script with the command below. Every AWS call appears here live.",
                    "先启动本地 AWS 模拟器，再用下方命令运行程序、测试或脚本。每条 AWS 调用都会实时显示在这里，并在失败或越权时给出根因。",
                ),
            ),
            TraceHealth::Error(error) => (
                i18n::pick("Can't read the trace file", "无法读取 Trace 文件"),
                error.as_str(),
            ),
            TraceHealth::Ok => (
                i18n::pick("No calls captured yet", "尚未捕获到请求"),
                i18n::pick(
                    "The session is connected, waiting for AWS SDK traffic routed via AWS_ENDPOINT_URL.",
                    "当前会话已就绪，正在等待通过 AWS_ENDPOINT_URL 路由的 AWS SDK 调用。",
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
            .px(px(10.0))
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
                    .child(endpoint),
            )
    }

    pub(super) fn render_no_matches(&mut self, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let description = if !self.query.is_empty() {
            let q = trace::clip_utf8(&self.query, 40);
            if i18n::is_zh() {
                format!("没有匹配 \"{q}\" 的请求。")
            } else {
                format!("Nothing matches \"{q}\".")
            }
        } else {
            i18n::pick(
                "No calls match the active filter combination.",
                "当前筛选组合下没有匹配的请求。",
            )
            .to_string()
        };

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
}

#[allow(clippy::too_many_arguments)]
pub(super) fn trace_row(
    trace: &Trace,
    raw_index: usize,
    selected: bool,
    op_width: f32,
    run_start_ms: Option<i64>,
    prev_end_ms: Option<i64>,
    density: Density,
    time_format: TimeFormat,
    this: WeakEntity<LensletApp>,
) -> Stateful<Div> {
    let t = theme::p();
    let compact_row = density == Density::Compact;
    let service_color = theme::service_color(trace.service_name());
    let idle_gap_ms = match (trace.at_ms(), prev_end_ms) {
        (Some(at), Some(prev_end)) if at - prev_end >= 500 => Some((at - prev_end) as u64),
        _ => None,
    };
    let rel_str = match (trace.at_ms(), run_start_ms) {
        (Some(at), Some(start)) => trace::format_offset(at - start),
        _ => short_clock(&trace.at),
    };
    let clock_str = short_clock(&trace.at);
    let (primary_time, secondary_time) = match time_format {
        TimeFormat::Relative => (
            rel_str,
            idle_gap_ms
                .map(|g| (format!("Δ {}", format_latency(g)), rgb(t.amber)))
                .or(Some((clock_str, rgb(t.text_faint)))),
        ),
        TimeFormat::Clock => (
            clock_str,
            idle_gap_ms
                .map(|g| (format!("Δ {}", format_latency(g)), rgb(t.amber)))
                .or(Some((rel_str, rgb(t.text_faint)))),
        ),
    };

    let row = div()
        .id(ElementId::named_usize("trace-row", raw_index))
        .relative()
        .flex()
        .w_full()
        .items_center()
        .h(px(theme::row_height(density)))
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
                .w(px(2.5))
                .bg(if selected {
                    rgb(t.accent)
                } else if trace.blocked {
                    rgb(t.red)
                } else {
                    rgba(0x0000_0000)
                }),
        )
        .child(
            div()
                .w(px(theme::COL_TIME))
                .flex_shrink_0()
                .flex()
                .flex_col()
                .gap(px(1.5))
                .font_family(fonts::mono())
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(rgb(t.text_dim))
                        .child(primary_time),
                )
                .when(!compact_row, |col| {
                    col.when_some(secondary_time, |c, (label, color)| {
                        c.child(div().text_size(px(9.5)).text_color(color).child(label))
                    })
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
                .gap(px(2.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .min_w(px(0.0))
                        .child(
                            div()
                                .text_size(px(12.5))
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
                        })
                        .when(compact_row, |title| {
                            title.child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .font_family(fonts::mono())
                                    .text_size(px(11.0))
                                    .text_color(rgb(t.text_muted))
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .overflow_hidden()
                                    .child(trace::display_path(trace).to_string()),
                            )
                        }),
                )
                .when(!compact_row, |col| col.child(row_detail(trace, op_width))),
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
                        .text_size(px(11.5))
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
                .text_size(px(11.5))
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
                    .text_size(px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(status_color(trace.status.max(400)))
                    .child(trace::ellipsize(&problem.code, 38)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_size(px(12.0))
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
            .w(px((op_width - 34.0).max(24.0)))
            .flex_shrink_0()
            .font_family(fonts::mono())
            .text_size(px(11.5))
            .text_color(rgb(t.text_muted))
            .min_w(px(0.0))
            .whitespace_nowrap()
            .text_ellipsis()
            .overflow_hidden()
            .child(trace::display_path(trace).to_string()),
    )
}

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
        .bg(theme::h(t.amber, 0.10))
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
