//! The request stream: table, rows, empty states.

use super::*;

impl LensletApp {
    pub(super) fn render_stream(&mut self, op_width: f32, cx: &mut Context<Self>) -> Div {
        if self.pane == Pane::Access && !self.rows.is_empty() {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.0))
                .bg(rgb(theme::PANEL))
                .border_r_1()
                .border_color(rgb(theme::BORDER))
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
            .bg(rgb(theme::PANEL))
            .border_r_1()
            .border_color(rgb(theme::BORDER))
            .child(self.render_stream_head(cx))
            .child(self.render_table_head(op_width))
            .child(body)
            .child(self.render_stream_footer())
    }

    pub(super) fn render_stream_head(&self, cx: &mut Context<Self>) -> Div {
        let filtered = !self.query.is_empty() || self.filter != Filter::All;
        let access = self.access_rows();
        let used_access = access.iter().filter(|entry| !entry.unused).count();
        let new_access = access.iter().filter(|entry| entry.new).count();
        let unused_access = access.iter().filter(|entry| entry.unused).count();
        let counts = if self.pane == Pane::Access {
            let mut text = format!(
                "{used_access} {}",
                if used_access == 1 {
                    "PERMISSION"
                } else {
                    "PERMISSIONS"
                }
            );
            if new_access > 0 {
                text.push_str(&format!(" · {new_access} NEW"));
            }
            if unused_access > 0 {
                text.push_str(&format!(" · {unused_access} UNUSED"));
            }
            text
        } else if filtered {
            format!(
                "{} OF {} CALLS · {}",
                self.counts.visible,
                self.counts.total,
                error_count(self.counts.visible_errors)
            )
        } else if let Some(reported) = self.counts.reported_total {
            format!(
                "LATEST {} OF {} CALLS · {}",
                self.counts.total,
                reported,
                error_count(self.counts.errors)
            )
        } else {
            let mut text = format!(
                "{} · {}",
                call_count(self.counts.total),
                error_count(self.counts.errors)
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
            text
        };

        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(theme::STREAM_HEAD_H))
            .px(px(theme::ROW_PAD_X))
            .border_b_1()
            .border_color(rgb(theme::BORDER_SUBTLE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(self.pane_tab(Pane::Calls, "CALLS", None, cx))
                    .child(self.pane_tab(
                        Pane::Access,
                        "ACCESS",
                        (new_access > 0).then(|| format!("+{new_access}")),
                        cx,
                    )),
            )
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(10.0))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(counts),
            )
    }

    pub(super) fn render_table_head(&self, op_width: f32) -> Div {
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

    pub(super) fn render_rows(&mut self, op_width: f32, cx: &mut Context<Self>) -> Div {
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
                        linear_color_stop(theme::h(theme::PANEL, 0.0), 0.0),
                        linear_color_stop(theme::h(theme::PANEL, 0.94), 1.0),
                    )),
            )
    }

    pub(super) fn render_first_run(&mut self, cx: &mut Context<Self>) -> Div {
        let (title, description) = match &self.health {
            TraceHealth::Missing => (
                "Run your code through Lenslet",
                "Start AWS Emulator, then run your program, tests or script with the command below. Every AWS call it makes appears here, with the reason when one fails.",
            ),
            TraceHealth::Error(error) => ("Can't read the trace file", error.as_str()),
            TraceHealth::Ok => (
                "No calls captured yet",
                "The run is live, but nothing has called AWS through it. SDKs that honor AWS_ENDPOINT_URL are routed automatically; others need the endpoint set by hand.",
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
            .border_color(rgb(theme::BORDER))
            .bg(rgb(theme::CODE))
            .child(glyph::dot(theme::h(theme::GREEN, 0.9), 5.0))
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(12.0))
                    .text_color(rgb(theme::TEXT_SOFT))
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
                            .text_color(rgb(theme::TEXT))
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .line_height(px(17.0))
                            .text_color(rgb(theme::TEXT_MUTED))
                            .text_center()
                            .child(description.to_string()),
                    )
                    .child(div().h(px(2.0)))
                    .child(endpoint)
                    .child(
                        div()
                            .font_family(fonts::mono())
                            .text_size(px(10.0))
                            .text_color(rgb(theme::TEXT_FAINT))
                            .child(format!(
                                "watching {}",
                                display_trace_path(self.trace_path.as_deref())
                            )),
                    )
                    .when(self.demo, |this| {
                        this.child(
                            div()
                                .text_size(px(10.5))
                                .text_color(rgb(theme::AMBER))
                                .child("Demo data · LENSLET_DEMO=1"),
                        )
                    }),
            )
    }

    pub(super) fn render_no_matches(&mut self, cx: &mut Context<Self>) -> Div {
        let query = if self.query.is_empty() {
            None
        } else {
            Some(format!("\"{}\"", trace::clip_utf8(&self.query, 40)))
        };
        let mut description = String::from("Try a different search");
        if let Filter::Access(access) = &self.filter {
            description = if self.counts.reported_total.is_some() {
                format!(
                    "Earlier calls that needed {} fell outside the latest {}-call window",
                    access.action, self.counts.total
                )
            } else {
                format!("No calls needed {} here", access.action)
            };
        } else if let Filter::Service(id) = &self.filter {
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
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(theme::TEXT_SOFT))
                    .child("No calls match"),
            )
            .child(
                div()
                    .text_size(px(12.0))
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

    pub(super) fn render_stream_footer(&self) -> Div {
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
                    format!("waiting for a run in {}", display_trace_path(None)),
                ),
                TraceHealth::Error(error) => {
                    (theme::h(theme::RED, 0.9), format!("trace error · {error}"))
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
                            "watching {}",
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
                    .text_color(rgb(theme::TEXT_MUTED))
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(text),
            );
        if self.skipped > 0 {
            left = left.child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(10.0))
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
    let service_color = theme::service_color(trace.service_name());
    let idle_gap_ms = match (trace.at_ms(), prev_end_ms) {
        (Some(at), Some(prev_end)) if at - prev_end >= 500 => Some((at - prev_end) as u64),
        _ => None,
    };
    let time_sub = if let Some(gap) = idle_gap_ms {
        Some((format!("Δ {}", format_latency(gap)), rgb(theme::AMBER)))
    } else {
        match (trace.at_ms(), run_start_ms) {
            (Some(at), Some(start)) => {
                Some((trace::format_offset(at - start), rgb(theme::TEXT_FAINT)))
            }
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
                .flex()
                .flex_col()
                .gap(px(2.0))
                .font_family(fonts::mono())
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(theme::TEXT_MUTED))
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
                                .text_color(rgb(theme::TEXT))
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .overflow_hidden()
                                .child(operation_label(trace).to_string()),
                        )
                        .when_some(trace.retry_badge(), |title, badge| {
                            title.child(retry_pill(badge))
                        })
                        .when(trace.new, |title| title.child(new_pill("NEW ACCESS"))),
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
                        rgb(theme::AMBER)
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
                            rgb(theme::AMBER)
                        } else if trace.is_error() {
                            status_color(trace.status)
                        } else {
                            rgb(theme::TEXT_BODY)
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
                    rgb(theme::AMBER)
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
        row.hover(|style| style.bg(rgb(theme::ROW_SELECTED_HOVER)))
    } else {
        row.hover(|style| style.bg(rgb(theme::ROW_HOVER)))
    }
}

/// The row's second line: the AWS error when the call failed, otherwise the
/// method and path.
fn row_detail(trace: &Trace, op_width: f32) -> Div {
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
                    .text_color(rgb(theme::TEXT_DIM))
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
            .text_size(px(12.0))
            .text_color(rgb(theme::TEXT_MUTED))
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
        let active = self.pane == pane;
        let mut tab = div()
            .id(label)
            .flex()
            .items_center()
            .gap(px(5.0))
            .h(px(22.0))
            .px(px(8.0))
            .rounded(px(5.0))
            .bg(if active {
                theme::h(theme::TEXT, 0.07)
            } else {
                theme::h(0x000000, 0.0)
            })
            .text_size(px(10.0))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(if active {
                rgb(theme::TEXT)
            } else {
                rgb(theme::TEXT_MUTED)
            })
            .cursor_pointer()
            .hover(|style| style.text_color(rgb(theme::TEXT_SOFT)))
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
    div()
        .h(px(15.0))
        .px(px(4.0))
        .flex()
        .items_center()
        .rounded(px(3.0))
        .border_1()
        .border_color(theme::line(theme::AMBER, 0.4))
        .bg(theme::h(theme::AMBER, 0.1))
        .font_family(fonts::mono())
        .text_size(px(9.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(theme::AMBER))
        .child(text.into())
}

fn retry_pill(text: impl Into<SharedString>) -> Div {
    div()
        .h(px(15.0))
        .px(px(4.0))
        .flex()
        .items_center()
        .rounded(px(3.0))
        .border_1()
        .border_color(theme::line(theme::ACCENT, 0.38))
        .bg(theme::h(theme::ACCENT, 0.09))
        .font_family(fonts::mono())
        .text_size(px(9.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(theme::ACCENT))
        .child(text.into())
}
