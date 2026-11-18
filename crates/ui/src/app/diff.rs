//! Visual Run-to-Run Diff workspace (`Pane::Diff`): compare the active run
//! against the previous (or any picked) finished run.

use super::*;

impl LensletApp {
    pub(super) fn compute_active_diff(&self) -> Option<trace::UiRunDiff> {
        let current = self.run.as_ref()?;
        let summaries = self.finished_baselines();
        let base = match self.diff_base.as_deref() {
            Some(pinned) => summaries
                .iter()
                .find(|s| s.path == pinned)
                .or_else(|| summaries.first())?,
            None => summaries.first()?,
        };
        let access = self.access_rows();
        trace::UiRunDiff::from_summaries(base, current, &self.rows, &access)
    }

    pub(super) fn finished_baselines(&self) -> Vec<trace::RunSummary> {
        let Source::Runs { dir, .. } = &self.source else {
            return Vec::new();
        };
        let current_path = self.trace_path.as_deref();
        trace::list_run_summaries(dir)
            .into_iter()
            .filter(|s| s.report.is_some() && Some(s.path.as_path()) != current_path)
            .collect()
    }

    pub(super) fn render_diff(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = theme::p();
        let Some(diff) = self.compute_active_diff() else {
            return div()
                .id("diff-empty")
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .p(px(28.0))
                .bg(rgb(t.panel))
                .child(glyph::lens_mark(34.0))
                .child(
                    div()
                        .text_size(px(13.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(t.text))
                        .child(i18n::pick(
                            "Need an earlier finished run to compare",
                            "需要至少一次已完成的历史运行才能对比",
                        )),
                )
                .child(
                    div()
                        .max_w(px(420.0))
                        .text_size(px(12.0))
                        .line_height(px(17.0))
                        .text_color(rgb(t.text_muted))
                        .text_center()
                        .child(i18n::pick(
                            "Run your test command twice under `lenslet run`; Diff will highlight new failures, fixed calls, IAM permission deltas, and latency shifts.",
                            "使用 `lenslet run` 运行两次测试后，此处将自动对比新增报错、已修复项、IAM 权限增减与耗时变化。",
                        )),
                );
        };

        let call_delta = diff.target_calls as isize - diff.base_calls as isize;
        let err_delta = diff.target_errors as isize - diff.base_errors as isize;
        let dur_delta = diff.target_duration_ms as i64 - diff.base_duration_ms as i64;

        let kpis = div()
            .flex()
            .gap(px(10.0))
            .child(kpi_card(
                i18n::pick("CALLS", "请求总数"),
                format!("{} → {}", diff.base_calls, diff.target_calls),
                format!("{call_delta:+}"),
                if call_delta == 0 {
                    t.text_muted
                } else {
                    t.accent
                },
            ))
            .child(kpi_card(
                i18n::pick("ERRORS", "错误调用"),
                format!("{} → {}", diff.base_errors, diff.target_errors),
                format!("{err_delta:+}"),
                if err_delta > 0 {
                    t.red
                } else if err_delta < 0 {
                    t.green
                } else {
                    t.text_muted
                },
            ))
            .child(kpi_card(
                i18n::pick("DURATION", "总耗时"),
                format!(
                    "{} → {}",
                    format_latency(diff.base_duration_ms),
                    format_latency(diff.target_duration_ms)
                ),
                format!("{dur_delta:+} ms"),
                if dur_delta > 250 {
                    t.amber
                } else if dur_delta < -100 {
                    t.green
                } else {
                    t.text_muted
                },
            ))
            .child(kpi_card(
                i18n::pick("ACCESS DELTA", "权限变更"),
                format!(
                    "+{} / −{}",
                    diff.access_added.len(),
                    diff.access_removed.len()
                ),
                if diff.access_added.is_empty() && diff.access_removed.is_empty() {
                    i18n::pick("unchanged", "无变化").to_string()
                } else {
                    i18n::pick("changed", "有变更").to_string()
                },
                if !diff.access_added.is_empty() {
                    t.amber
                } else {
                    t.green
                },
            ));

        let mut content = div()
            .w_full()
            .max_w(px(920.0))
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(12.0))
                    .py(px(9.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(t.border))
                    .bg(rgb(t.panel_raised))
                    .font_family(fonts::mono())
                    .text_size(px(11.5))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(badge(format!("BASE {}", diff.base_id), t.text_muted))
                            .child(
                                div()
                                    .text_color(rgb(t.text_dim))
                                    .child(diff.base_cmd.clone()),
                            )
                            .child(div().text_color(rgb(t.accent)).child("→"))
                            .child(badge(format!("NOW {}", diff.target_id), t.accent))
                            .child(div().text_color(rgb(t.text)).child(diff.target_cmd.clone())),
                    ),
            )
            .child(kpis);

        if !diff.new_failures.is_empty() {
            let list = div()
                .flex()
                .flex_col()
                .children(diff.new_failures.iter().map(|f| {
                    diff_row_line(
                        "✗",
                        t.red,
                        format!("{} {}", f.service.to_ascii_uppercase(), f.operation),
                        f.code.clone(),
                    )
                }));
            content = content.child(diff_card(
                i18n::pick("NEW FAILURES", "新增报错（上次通过，本次失败）"),
                t.red,
                list,
            ));
        }

        if !diff.fixed_failures.is_empty() {
            let list = div()
                .flex()
                .flex_col()
                .children(diff.fixed_failures.iter().map(|f| {
                    diff_row_line(
                        "✓",
                        t.green,
                        format!("{} {}", f.service.to_ascii_uppercase(), f.operation),
                        f.code.clone(),
                    )
                }));
            content = content.child(diff_card(
                i18n::pick("FIXED CALLS", "已修复调用（上次失败，本次恢复）"),
                t.green,
                list,
            ));
        }

        if !diff.access_added.is_empty() || !diff.access_removed.is_empty() {
            let mut list = div().flex().flex_col();
            for (idx, a) in diff.access_added.iter().enumerate() {
                let acc = a.clone();
                list = list.child(
                    diff_row_line("+", t.amber, a.action.clone(), a.resource.clone())
                        .id(ElementId::named_usize("diff-access-added", idx))
                        .cursor_pointer()
                        .hover(move |s| s.bg(rgb(t.row_hover)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.filter_calls_by_access(acc.clone(), cx);
                        })),
                );
            }
            for a in &diff.access_removed {
                list = list.child(diff_row_line(
                    "−",
                    t.text_muted,
                    a.action.clone(),
                    a.resource.clone(),
                ));
            }
            content = content.child(diff_card(
                i18n::pick("IAM ACCESS CHANGES", "IAM 权限差异"),
                t.amber,
                list,
            ));
        }

        if !diff.service_deltas.is_empty() {
            let list = div()
                .flex()
                .flex_col()
                .children(diff.service_deltas.iter().map(|(svc, b, now)| {
                    let delta = *now as isize - *b as isize;
                    diff_row_line(
                        "·",
                        t.accent,
                        trace::service_label(svc),
                        format!("{b} → {now} calls ({delta:+})"),
                    )
                }));
            content = content.child(diff_card(
                i18n::pick("CALLS BY SERVICE", "各 AWS 服务调用量对比"),
                t.accent,
                list,
            ));
        }

        div()
            .id("diff-scroll")
            .flex_1()
            .min_h(px(0.0))
            .min_w(px(0.0))
            .bg(rgb(t.panel))
            .overflow_y_scroll()
            .scrollbar_width(px(6.0))
            .p(px(20.0))
            .flex()
            .flex_col()
            .items_center()
            .child(content)
    }
}

fn kpi_card(label: &'static str, main: String, delta: String, delta_color: u32) -> Div {
    let t = theme::p();
    div()
        .flex_1()
        .p(px(12.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(t.border))
        .bg(rgb(t.panel_raised))
        .flex()
        .flex_col()
        .gap(px(5.0))
        .child(micro_label(label))
        .child(
            div()
                .flex()
                .items_baseline()
                .justify_between()
                .gap(px(6.0))
                .font_family(fonts::mono())
                .child(
                    div()
                        .text_size(px(14.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(t.text))
                        .child(main),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(delta_color))
                        .child(delta),
                ),
        )
}

fn diff_card(title: &'static str, tint: u32, list: Div) -> Div {
    let t = theme::p();
    div()
        .rounded(px(6.0))
        .border_1()
        .border_color(theme::line(tint, 0.30))
        .bg(rgb(t.panel_raised))
        .overflow_hidden()
        .flex()
        .flex_col()
        .child(
            div()
                .px(px(12.0))
                .py(px(7.0))
                .border_b_1()
                .border_color(rgb(t.border_subtle))
                .bg(theme::h(tint, 0.06))
                .child(micro_label(title)),
        )
        .child(list)
}

fn diff_row_line(symbol: &'static str, color: u32, left: String, right: String) -> Div {
    let t = theme::p();
    div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(12.0))
        .py(px(7.0))
        .border_b_1()
        .border_color(rgb(t.border_subtle))
        .font_family(fonts::mono())
        .text_size(px(11.5))
        .child(
            div()
                .w(px(14.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(color))
                .child(symbol),
        )
        .child(
            div()
                .w(px(230.0))
                .flex_shrink_0()
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(t.text))
                .whitespace_nowrap()
                .text_ellipsis()
                .overflow_hidden()
                .child(left),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_color(rgb(t.text_dim))
                .whitespace_nowrap()
                .text_ellipsis()
                .overflow_hidden()
                .child(right),
        )
}
