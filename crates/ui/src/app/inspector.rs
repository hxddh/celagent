//! Context-Aware Inspector (Option A Payload-First design):
//! - In `Calls`: `Exchange` (default — root-cause diagnostic + Request Body + Response Body
//!   on one scrollable page), `Headers` (structured key-value table with per-row Copy),
//!   and `IAM & Meta` (permissions + non-duplicated wire metadata).
//! - In `Access`: delegates to `render_access_inspector`.

use super::*;

impl LensletApp {
    pub(super) fn render_inspector(&mut self, width: f32, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        if self.pane == Pane::Access {
            return div()
                .flex()
                .flex_col()
                .w(px(width))
                .flex_shrink_0()
                .bg(rgb(t.panel_raised))
                .child(self.render_access_inspector(cx));
        }

        let trace = self.selected_hydrated_trace();
        let body: AnyElement = if let Some(trace) = trace.as_ref() {
            self.render_inspector_body(trace, cx).into_any_element()
        } else {
            self.render_inspector_empty().into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .w(px(width))
            .flex_shrink_0()
            .bg(rgb(t.panel_raised))
            .child(if let Some(trace) = trace.as_ref() {
                self.render_inspector_header(trace, cx)
            } else {
                self.render_inspector_header_empty()
            })
            .child(body)
    }

    pub(super) fn render_inspector_header_empty(&self) -> Div {
        let t = theme::p();
        div()
            .flex()
            .items_center()
            .h(px(theme::TABLE_HEAD_H))
            .px(px(14.0))
            .border_b_1()
            .border_color(rgb(t.border_subtle))
            .child(micro_label("INSPECTOR"))
    }

    pub(super) fn render_inspector_header(&self, trace: &Trace, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
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
            .bg(theme::h(t.text, 0.03))
            .child(glyph::dot(color, 4.5))
            .child(
                div()
                    .text_size(px(10.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color)
                    .child(trace::ellipsize(trace.service_name(), 14)),
            );

        let endpoint = self
            .run
            .as_ref()
            .map(|r| r.header.endpoint.as_str())
            .filter(|e| !e.is_empty());
        let curl_cmd = trace::to_curl(trace, endpoint);

        let mut top = div()
            .flex()
            .items_center()
            .gap(px(7.0))
            .child(service)
            .child(
                div()
                    .text_size(px(14.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(t.text))
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .overflow_hidden()
                    .child(operation_label(trace).to_string()),
            );
        if trace.blocked {
            top = top.child(blocked_pill("BLOCKED"));
        }
        if let Some(retry) = trace.retry_badge() {
            top = top.child(badge(retry, t.accent));
        }
        top = top.child(div().flex_1());
        if self.can_replay_trace(trace) {
            let is_replaying = matches!(
                self.replays.get(&trace.id),
                Some(crate::replay::ReplayState::Running)
            );
            let replay_label = if is_replaying {
                i18n::pick("Replaying…", "重放中…")
            } else {
                i18n::pick("Replay", "重放")
            };
            top = top.child(
                div()
                    .id("replay-call")
                    .flex()
                    .items_center()
                    .gap(px(4.5))
                    .h(px(21.0))
                    .px(px(7.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(theme::line(t.accent, 0.42))
                    .bg(theme::h(t.accent, 0.09))
                    .text_size(px(10.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(t.accent))
                    .cursor_pointer()
                    .hover(move |s| s.bg(theme::h(t.accent, 0.16)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.trigger_replay_selected(cx);
                    }))
                    .child(glyph::replay_icon(theme::h(t.accent, 0.95)))
                    .child(replay_label),
            );
        }
        if let Some(curl_cmd) = curl_cmd {
            top = top.child(
                action_button("copy-curl", i18n::pick("Copy curl", "复制 curl")).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.copy_text("curl", curl_cmd.clone(), cx);
                    }),
                ),
            );
        }

        let path_row = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .mt(px(5.0))
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(11.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(t.accent))
                    .child(trace.method.to_ascii_uppercase()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .font_family(fonts::mono())
                    .text_size(px(11.5))
                    .text_color(rgb(t.text_dim))
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .overflow_hidden()
                    .child(trace::display_path(trace).to_string()),
            )
            .child(self.copy_button("copy-path", "path", Some(trace.path.clone()), cx));

        let (status_tint, status_value, status_words) = if trace.pending {
            (rgb(t.amber), "…".to_string(), "pending")
        } else {
            (
                status_color(trace.status),
                status_label(trace.status),
                status_text(trace.status),
            )
        };
        let mut meta = div()
            .flex()
            .items_center()
            .gap(px(9.0))
            .mt(px(5.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .child(glyph::dot(status_tint, 4.5))
                    .child(
                        div()
                            .font_family(fonts::mono())
                            .text_size(px(11.0))
                            .text_color(status_tint)
                            .child(status_value),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(rgb(t.text_faint))
                            .child(status_words),
                    ),
            )
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(10.5))
                    .text_color(if trace.pending {
                        rgb(t.amber)
                    } else {
                        latency_color(trace.ms)
                    })
                    .child(if trace.pending {
                        "running".to_string()
                    } else {
                        format_latency(trace.ms)
                    }),
            )
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(10.5))
                    .text_color(rgb(t.text_faint))
                    .child(short_clock(&trace.at)),
            )
            .child(div().flex_1())
            .child(glyph::auth_mark(trace.authorization || trace.presigned()))
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(t.text_muted))
                    .child(if trace.authorization {
                        "SigV4"
                    } else if trace.presigned() {
                        "Presigned"
                    } else {
                        "Unsigned"
                    }),
            );
        if let Some(region) = trace.region.as_deref().filter(|r| !r.is_empty()) {
            meta = meta.child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(10.0))
                    .text_color(rgb(t.text_faint))
                    .child(region.to_string()),
            );
        }

        div()
            .flex()
            .flex_col()
            .px(px(14.0))
            .pt(px(10.0))
            .child(top)
            .child(path_row)
            .child(meta)
            .child(self.render_tabs(trace, cx))
    }

    pub(super) fn render_tabs(&self, trace: &Trace, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let req_h = trace.request.as_ref().map_or(0, |r| r.headers.len());
        let res_h = trace.response.as_ref().map_or(0, |r| r.headers.len());
        let iam_cnt = trace.access.len() + trace.implied.len();

        let items = [
            (
                InspectorTab::Exchange,
                InspectorTab::Exchange.label().to_string(),
            ),
            (
                InspectorTab::Headers,
                format!("{} ({})", InspectorTab::Headers.label(), req_h + res_h),
            ),
            (
                InspectorTab::IamMeta,
                if iam_cnt > 0 {
                    format!("{} ({iam_cnt})", InspectorTab::IamMeta.label())
                } else {
                    InspectorTab::IamMeta.label().to_string()
                },
            ),
        ];

        let mut tabs = Vec::new();
        for (tab, label) in items {
            let active = self.tab == tab;
            tabs.push(
                div()
                    .id(ElementId::Name(format!("tab-{}", tab.index()).into()))
                    .h(px(32.0))
                    .flex()
                    .items_center()
                    .border_b_2()
                    .border_color(if active {
                        rgb(t.accent)
                    } else {
                        rgba(0x0000_0000)
                    })
                    .text_size(px(11.5))
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
                    .hover(move |style| style.text_color(rgb(t.text_soft)))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)))
                    .child(label),
            );
        }

        div()
            .flex()
            .items_end()
            .gap(px(16.0))
            .mt(px(6.0))
            .border_b_1()
            .border_color(rgb(t.border_subtle))
            .children(tabs)
            .child(div().flex_1())
            .child(
                div()
                    .pb(px(5.0))
                    .font_family(fonts::mono())
                    .text_size(px(9.5))
                    .text_color(rgb(t.text_faint))
                    .child("1 2 3"),
            )
    }

    pub(super) fn render_inspector_empty(&self) -> Div {
        let t = theme::p();
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
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(t.text_soft))
                    .child(i18n::pick("Select a call", "选择一条请求")),
            )
            .child(
                div()
                    .text_size(px(11.5))
                    .line_height(px(16.0))
                    .text_color(rgb(t.text_muted))
                    .text_center()
                    .child(i18n::pick(
                        "Click a row, or use ↑ ↓ to step through the stream.",
                        "点击左侧列表行，或使用 ↑ ↓ 方向键浏览。",
                    )),
            )
    }

    pub(super) fn render_inspector_body(
        &mut self,
        trace: &Trace,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let content: AnyElement = match self.tab {
            InspectorTab::Exchange => self.render_exchange_combined(trace, cx).into_any_element(),
            InspectorTab::Headers => self.render_headers_tab(trace, cx).into_any_element(),
            InspectorTab::IamMeta => self.render_iam_meta_tab(trace, cx).into_any_element(),
        };

        div()
            .id(ElementId::Name(format!("inspector-{}", trace.key()).into()))
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .scrollbar_width(px(6.0))
            .child(div().p(px(14.0)).w_full().child(content))
    }

    /// Option A Default Tab (`Exchange`): Replay comparison (if triggered) +
    /// Root-cause failure card (if any) + SDK Retry Chain (if retried) +
    /// Request Payload + Response Payload directly on one scrollable page.
    fn render_exchange_combined(&self, trace: &Trace, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .children(self.render_replay_panel(trace, cx))
            .children(problem_panel(trace))
            .children(self.render_retry_chain(trace, cx))
            .child(self.render_payload_section(trace.request.as_ref(), Side::Request, cx))
            .child(self.render_payload_section(trace.response.as_ref(), Side::Response, cx))
    }

    fn render_replay_panel(&self, trace: &Trace, cx: &mut Context<Self>) -> Option<Div> {
        let t = theme::p();
        let state = self.replays.get(&trace.id)?;
        let trace_id = trace.id.clone();

        match state {
            crate::replay::ReplayState::Running => Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .p(px(10.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(theme::line(t.accent, 0.40))
                    .bg(theme::h(t.accent, 0.07))
                    .text_size(px(11.5))
                    .text_color(rgb(t.accent))
                    .child(glyph::replay_icon(theme::h(t.accent, 0.95)))
                    .child(i18n::pick(
                        "Replaying request live against local emulator…",
                        "正在向本地模拟器实时重放该请求…",
                    )),
            ),
            crate::replay::ReplayState::Failed(err) => Some(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .p(px(10.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(theme::line(t.red, 0.40))
                    .bg(theme::h(t.red, 0.06))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(micro_label(i18n::pick("REPLAY FAILED", "重放失败")))
                            .child(
                                action_button("dismiss-replay", i18n::pick("Dismiss", "关闭"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.replays.remove(&trace_id);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(note_text(err.clone())),
            ),
            crate::replay::ReplayState::Done(out) => {
                let ok = out.status < 400;
                let border_hex = if ok { t.green } else { t.red };
                let search_toks = search_tokens(&self.query);
                let mut card = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .p(px(10.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(theme::line(border_hex, 0.42))
                    .bg(theme::h(border_hex, 0.06))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(micro_label(i18n::pick(
                                "LIVE REPLAY RESULT (IN-MEMORY)",
                                "实时重放结果（仅内存对比，未写入运行记录）",
                            )))
                            .child(badge(
                                format!(
                                    "{} {} · {}",
                                    out.status,
                                    status_text(out.status),
                                    format_latency(out.ms)
                                ),
                                border_hex,
                            ))
                            .child(
                                div()
                                    .font_family(fonts::mono())
                                    .text_size(px(10.0))
                                    .text_color(rgb(t.text_muted))
                                    .child(format!(
                                        "{} · {} hdrs · was {} ({})",
                                        format_bytes(out.body_bytes),
                                        out.headers.len(),
                                        trace.status,
                                        format_latency(trace.ms)
                                    )),
                            )
                            .child(div().flex_1())
                            .child(
                                action_button("dismiss-replay", i18n::pick("Dismiss", "关闭"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.replays.remove(&trace_id);
                                        cx.notify();
                                    })),
                            ),
                    );
                if let Some(body_txt) = out.body_text.as_deref().filter(|s| !s.is_empty()) {
                    let ct = out.content_type.as_deref().unwrap_or("");
                    let shown =
                        trace::pretty_body(body_txt, ct).unwrap_or_else(|| body_txt.to_string());
                    let kind = crate::syntax::SyntaxKind::detect(&shown, ct, false, false);
                    card = card.child(
                        syntax_code_block(
                            "replay-res-body",
                            shown,
                            kind,
                            &search_toks,
                            200.0,
                            self.prefs.word_wrap,
                            true,
                        )
                        .0,
                    );
                }
                Some(card)
            }
        }
    }

    /// Interactive SDK Retry Chain strip when `amz-sdk-invocation-id` spans ≥2 attempts.
    fn render_retry_chain(&self, trace: &Trace, cx: &mut Context<Self>) -> Option<Div> {
        let inv = trace.invocation_id.as_deref().filter(|s| !s.is_empty())?;
        let peers: Vec<(usize, Arc<Trace>)> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.invocation_id.as_deref() == Some(inv))
            .map(|(i, r)| (i, r.clone()))
            .collect();
        if peers.len() < 2 {
            return None;
        }
        let t = theme::p();
        let mut chain = div().flex().flex_wrap().items_center().gap(px(5.0));
        let mut prev_end: Option<i64> = None;

        for (pos, (raw_idx, item)) in peers.iter().enumerate() {
            if pos > 0 {
                let gap_txt = match (prev_end, item.at_ms()) {
                    (Some(pe), Some(at)) if at > pe => {
                        format!("── +{} ──▶", format_latency((at - pe) as u64))
                    }
                    _ => "──▶".to_string(),
                };
                chain = chain.child(
                    div()
                        .font_family(fonts::mono())
                        .text_size(px(9.5))
                        .text_color(rgb(t.text_faint))
                        .child(gap_txt),
                );
            }
            prev_end = item.at_ms().map(|at| at + item.ms as i64);
            let is_current = item.id == trace.id;
            let att_no = item.attempt.unwrap_or((pos + 1) as u32);
            let hex = if item.blocked {
                t.purple
            } else if item.is_error() {
                t.red
            } else {
                t.green
            };
            let target_idx = *raw_idx;
            chain = chain.child(
                div()
                    .id(ElementId::named_usize("retry-hop", pos))
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .h(px(22.0))
                    .px(px(7.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(if is_current {
                        theme::h(t.accent, 1.0)
                    } else {
                        theme::line(hex, 0.40)
                    })
                    .bg(if is_current {
                        theme::h(t.accent, 0.14)
                    } else {
                        theme::h(hex, 0.08)
                    })
                    .font_family(fonts::mono())
                    .text_size(px(10.5))
                    .text_color(rgb(hex))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_row(target_idx, window, cx);
                    }))
                    .child(format!(
                        "#{att_no} {} ({})",
                        if item.pending {
                            "…".to_string()
                        } else {
                            status_label(item.status)
                        },
                        format_latency(item.ms)
                    )),
            );
        }

        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .p(px(9.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(rgb(t.border_subtle))
                .bg(theme::h(t.text, 0.02))
                .child(micro_label(i18n::pick(
                    "SDK RETRY TIMELINE (CLICK ATTEMPT TO JUMP)",
                    "SDK 重试调用链（点击任意尝试可跳转）",
                )))
                .child(chain),
        )
    }

    fn render_payload_section(
        &self,
        exchange: Option<&Exchange>,
        side: Side,
        cx: &mut Context<Self>,
    ) -> Div {
        let title = match side {
            Side::Request => "REQUEST BODY",
            Side::Response => "RESPONSE BODY",
        };
        let code_id = match side {
            Side::Request => "req-body",
            Side::Response => "res-body",
        };
        let Some(exchange) = exchange else {
            return section_with_action(
                title,
                None,
                None,
                empty_note(if matches!(side, Side::Response) {
                    i18n::pick("Awaiting response…", "等待响应返回…")
                } else {
                    i18n::pick("Not captured", "未捕获")
                }),
            );
        };

        let body = exchange.body.as_ref();
        let mut body_meta = Vec::new();
        if let Some(body) = body {
            body_meta.push(format_bytes(body.size()));
            if let Some(ct) = exchange.content_type.as_deref() {
                body_meta.push(body_kind(ct, body));
            } else if !body.binary && !body.is_empty() {
                body_meta.push("UTF-8".to_string());
            }
            if let Some(enc) = body.encoding.as_deref() {
                body_meta.push(enc.to_string());
            }
            if body.truncated {
                body_meta.push("truncated".to_string());
            }
        } else {
            body_meta.push("0 B".to_string());
        }

        let actions = self.render_body_actions(body, &exchange.content_type, side, cx);
        section_with_action(
            title,
            Some(body_meta.join(" · ")),
            actions,
            self.render_body(body, &exchange.content_type, code_id),
        )
    }

    /// Structured Key-Value Headers table for Request & Response (no horizontal-scrolling blob).
    fn render_headers_tab(&self, trace: &Trace, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(self.render_header_table(
                "REQUEST HEADERS",
                "copy-req-headers",
                "req-hdr",
                trace.request.as_ref().map(|r| r.headers.as_slice()),
                cx,
            ))
            .child(self.render_header_table(
                "RESPONSE HEADERS",
                "copy-res-headers",
                "res-hdr",
                trace.response.as_ref().map(|r| r.headers.as_slice()),
                cx,
            ))
    }

    fn render_header_table(
        &self,
        title: &'static str,
        copy_all_id: &'static str,
        row_prefix: &'static str,
        headers: Option<&[trace::Header]>,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = theme::p();
        let headers = headers.unwrap_or(&[]);
        if headers.is_empty() {
            return section_with_action(
                title,
                Some("0".to_string()),
                None,
                empty_note(i18n::pick("No headers captured", "未捕获到 Headers")),
            );
        }

        let all_text = headers_to_text(headers);
        let copy_all = self.copy_button(copy_all_id, "headers", Some(all_text), cx);
        let wrap = self.prefs.word_wrap;
        let search_toks = search_tokens(&self.query);

        let table = div()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(t.border))
            .bg(rgb(t.code))
            .overflow_hidden()
            .flex()
            .flex_col()
            .children(headers.iter().enumerate().map(|(i, h)| {
                let val_copy = h.value.clone();
                let (val_spans, _) = crate::syntax::highlight_inline(&h.value, &search_toks);
                let styled_val = if val_spans.is_empty() {
                    gpui::StyledText::new(h.value.clone())
                } else {
                    gpui::StyledText::new(h.value.clone()).with_highlights(val_spans)
                };
                let row = div()
                    .id(ElementId::Name(format!("{row_prefix}-{i}").into()))
                    .flex()
                    .items_start()
                    .gap(px(10.0))
                    .px(px(10.0))
                    .py(px(6.0))
                    .border_b_1()
                    .border_color(rgb(t.border_subtle))
                    .font_family(fonts::mono())
                    .text_size(px(11.0))
                    .cursor_pointer()
                    .hover(move |s| s.bg(theme::h(t.text, 0.04)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.copy_text("header", val_copy.clone(), cx);
                    }))
                    .child(
                        div()
                            .w(px(142.0))
                            .flex_shrink_0()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(t.accent))
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .overflow_hidden()
                            .child(h.name.clone()),
                    );
                if wrap {
                    row.child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_color(rgb(t.text_body))
                            .child(styled_val),
                    )
                } else {
                    row.child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_color(rgb(t.text_body))
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .overflow_hidden()
                            .child(styled_val),
                    )
                }
            }));

        section_with_action(
            title,
            Some(format!("{} entries · click row to copy", headers.len())),
            Some(copy_all),
            table,
        )
    }

    /// Non-duplicated IAM & Wire Metadata tab (`3`).
    fn render_iam_meta_tab(&self, trace: &Trace, cx: &mut Context<Self>) -> Div {
        let request_id = trace
            .response
            .as_ref()
            .and_then(|res| {
                res.header("x-amz-request-id")
                    .or_else(|| res.header("x-amzn-requestid"))
            })
            .or_else(|| {
                trace.request.as_ref().and_then(|req| {
                    req.header("x-amz-request-id")
                        .or_else(|| req.header("x-amzn-requestid"))
                })
            });

        let mut meta = div().flex().flex_col();
        meta = meta
            .child(meta_row("Started", mono_value(full_time(&trace.at))))
            .child(meta_row(
                "Host",
                mono_value(if trace.host.is_empty() {
                    "—".to_string()
                } else {
                    trace.host.clone()
                }),
            ))
            .child(meta_row(
                "Region",
                mono_value(trace.region.clone().unwrap_or_else(|| "—".to_string())),
            ))
            .child(meta_row(
                "Request ID",
                mono_value(request_id.unwrap_or("—").to_string()),
            ))
            .child(meta_row("Trace ID", mono_value(trace.id.clone())));

        if let Some(inv) = trace.invocation_id.as_deref() {
            meta = meta.child(meta_row("Invocation ID", mono_value(inv.to_string())));
        }
        if trace.is_retry() || self.invocation_stats(trace).is_some() {
            let attempt = trace.attempt.unwrap_or(1);
            let mut label = match trace.max_attempts {
                Some(max) => format!("{attempt} of {max}"),
                None => format!("attempt {attempt}"),
            };
            if let Some((count, total_ms)) = self.invocation_stats(trace) {
                label.push_str(&format!(" · {count} tries ({})", format_latency(total_ms)));
            }
            meta = meta.child(meta_row("SDK Retries", mono_value(label)));
        }
        if let Some(sha) = trace
            .request
            .as_ref()
            .and_then(|r| r.body.as_ref())
            .and_then(|b| b.sha256.as_deref())
        {
            meta = meta.child(meta_row("Req SHA-256", mono_value(sha.to_string())));
        }
        if let Some(sha) = trace
            .response
            .as_ref()
            .and_then(|r| r.body.as_ref())
            .and_then(|b| b.sha256.as_deref())
        {
            meta = meta.child(meta_row("Res SHA-256", mono_value(sha.to_string())));
        }

        div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .children(self.access_section_interactive(trace, cx))
            .child(section(i18n::pick("WIRE METADATA", "底层协议元数据"), meta))
    }

    fn access_section_interactive(&self, trace: &Trace, cx: &mut Context<Self>) -> Option<Div> {
        let t = theme::p();
        let entries: Vec<(&trace::CallAccess, bool)> = trace.call_access().collect();
        if entries.is_empty() {
            return None;
        }
        let mut list = div().flex().flex_col();
        for (idx, (access, implied)) in entries.into_iter().enumerate() {
            let acc_key = access.key();
            let mut name = div().flex().items_center().gap(px(6.0)).child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(t.text))
                    .child(access.action.clone()),
            );
            if implied {
                name = name.child(
                    div()
                        .text_size(px(10.0))
                        .text_color(rgb(t.text_muted))
                        .child("implied"),
                );
            }
            if trace.is_access_new(access) {
                if trace.blocked {
                    name = name.child(blocked_pill("DENIED"));
                } else {
                    name = name.child(new_pill("NOT IN LOCK"));
                }
            }
            list = list.child(
                div()
                    .id(ElementId::named_usize("call-iam-entry", idx))
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .py(px(6.0))
                    .px(px(6.0))
                    .rounded(px(4.0))
                    .border_b_1()
                    .border_color(rgb(t.border_subtle))
                    .cursor_pointer()
                    .hover(move |s| s.bg(rgb(t.row_hover)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_access = Some(acc_key.clone());
                        this.set_pane(Pane::Access, cx);
                    }))
                    .child(name)
                    .child(mono_value(access.resource.clone())),
            );
        }
        Some(section(
            i18n::pick("IAM PERMISSIONS", "所需 IAM 权限"),
            list,
        ))
    }

    fn render_body_actions(
        &self,
        body: Option<&Body>,
        content_type: &Option<String>,
        side: Side,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let body = body?;
        let mut row = div().flex().items_center().gap(px(4.0));
        let mut any = false;

        if let Some(text) = body.text.as_deref().filter(|t| !t.is_empty()) {
            let ct_str = content_type.as_deref().unwrap_or("");
            let has_semantic =
                trace::decode_aws_semantic(text, ct_str, body.encoding.as_deref()).is_some();
            let can_pretty = trace::pretty_body(text, ct_str).is_some();

            if has_semantic {
                let dec_id = match side {
                    Side::Request => "toggle-req-decoded",
                    Side::Response => "toggle-res-decoded",
                };
                let dec_label = if self.semantic_decode && !self.raw_body {
                    i18n::pick("Wire JSON", "标准格式")
                } else {
                    i18n::pick("AWS Decoded", "AWS 语义解码")
                };
                row = row.child(action_button(dec_id, dec_label).on_click(cx.listener(
                    |this, _, _, cx| {
                        if this.raw_body {
                            this.raw_body = false;
                            this.semantic_decode = true;
                        } else {
                            this.semantic_decode = !this.semantic_decode;
                        }
                        cx.notify();
                    },
                )));
            }
            if can_pretty || has_semantic {
                let toggle_label = if self.raw_body {
                    i18n::pick("Formatted", "格式化")
                } else {
                    i18n::pick("Raw", "原始")
                };
                row = row.child(
                    action_button(
                        match side {
                            Side::Request => "toggle-req-raw",
                            Side::Response => "toggle-res-raw",
                        },
                        toggle_label,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.raw_body = !this.raw_body;
                        cx.notify();
                    })),
                );
            }
            if text.len() > MAX_BODY_DISPLAY {
                let expand_label = if self.expand_body {
                    i18n::pick("Clip", "收起")
                } else {
                    i18n::pick("Full", "展开全文")
                };
                row = row.child(
                    action_button(
                        match side {
                            Side::Request => "expand-req-body",
                            Side::Response => "expand-res-body",
                        },
                        expand_label,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.expand_body = !this.expand_body;
                        cx.notify();
                    })),
                );
            }
            row = row.child(self.copy_button(
                match side {
                    Side::Request => "copy-req-body",
                    Side::Response => "copy-res-body",
                },
                "body",
                Some(text.to_string()),
                cx,
            ));
            any = true;
        } else if let Some(hex_str) = body.hex.as_deref().filter(|h| !h.is_empty()) {
            row = row.child(self.copy_button(
                match side {
                    Side::Request => "copy-req-hex",
                    Side::Response => "copy-res-hex",
                },
                "hex",
                Some(hex_str.to_string()),
                cx,
            ));
            any = true;
        }

        any.then(|| row.into_any_element())
    }

    pub(super) fn render_body(
        &self,
        body: Option<&Body>,
        content_type: &Option<String>,
        code_id: &'static str,
    ) -> Div {
        let t = theme::p();
        let Some(body) = body else {
            return empty_note(i18n::pick("Empty · 0 B", "无内容 · 0 B"));
        };

        let search_toks = search_tokens(&self.query);
        if body.binary {
            let mut block = div().flex().flex_col().gap(px(6.0));
            if let Some(hex_str) = body.hex.as_deref().filter(|h| !h.is_empty()) {
                let dump = trace::format_hex_dump(hex_str);
                let (elem, _) = syntax_code_block(
                    code_id,
                    dump,
                    crate::syntax::SyntaxKind::Hex,
                    &search_toks,
                    240.0,
                    false,
                    false,
                );
                block = block.child(elem);
            } else {
                block = block.child(empty_note(&format!(
                    "Binary body · {}",
                    format_bytes(body.size())
                )));
            }
            let mut meta = format!("Binary · {}", format_bytes(body.size()));
            if let Some(sha) = body.sha256.as_deref() {
                meta.push_str(&format!(" · sha256 {sha}"));
            }
            block = block.child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(10.0))
                    .text_color(rgb(t.text_dim))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(meta),
            );
            return block;
        }

        let Some(text) = body.text.as_deref() else {
            return empty_note(&format!(
                "{} · {}",
                i18n::pick("Body text not captured", "未捕获正文文本"),
                format_bytes(body.size())
            ));
        };

        if text.is_empty() && body.size() == 0 {
            return empty_note(i18n::pick("Empty · 0 B", "无内容 · 0 B"));
        }

        let ct_str = content_type.as_deref().unwrap_or("");
        let semantic = if !self.raw_body && self.semantic_decode {
            trace::decode_aws_semantic(text, ct_str, body.encoding.as_deref())
        } else {
            None
        };
        let (semantic_badge, display_owned, is_form_syntax) =
            if let Some((lbl, dec, is_form)) = semantic {
                (Some(lbl), dec, is_form)
            } else if self.raw_body {
                (None, text.to_string(), false)
            } else {
                (
                    None,
                    trace::pretty_body(text, ct_str).unwrap_or_else(|| text.to_string()),
                    false,
                )
            };
        let display = display_owned.as_str();
        let clipped = !self.expand_body && display.len() > MAX_BODY_DISPLAY;
        let shown = if clipped {
            trace::clip_utf8(display, MAX_BODY_DISPLAY)
        } else {
            display
        };
        let kind = crate::syntax::SyntaxKind::detect(shown, ct_str, false, is_form_syntax);
        let (code_elem, match_count) = syntax_code_block(
            code_id,
            shown.to_string(),
            kind,
            &search_toks,
            if self.expand_body { 560.0 } else { 290.0 },
            self.prefs.word_wrap,
            true,
        );
        let mut block = div().flex().flex_col().gap(px(5.0));
        if semantic_badge.is_some() || match_count > 0 {
            let mut info_row = div().flex().items_center().gap(px(6.0));
            if let Some(lbl) = semantic_badge {
                info_row = info_row.child(badge(lbl, t.purple));
            }
            if match_count > 0 {
                let m_txt = if i18n::is_zh() {
                    format!("{match_count} 处搜索命中")
                } else if match_count == 1 {
                    "1 search match".to_string()
                } else {
                    format!("{match_count} search matches")
                };
                info_row = info_row.child(badge(m_txt, t.accent));
            }
            block = block.child(info_row);
        }
        block = block.child(code_elem);
        let mut notes = Vec::new();
        if clipped {
            notes.push(if i18n::is_zh() {
                format!(
                    "预览截断在 {}；点击“展开全文”或“复制”获取完整内容。",
                    format_bytes(MAX_BODY_DISPLAY)
                )
            } else {
                format!(
                    "Display capped at {}; click Full or Copy for all.",
                    format_bytes(MAX_BODY_DISPLAY)
                )
            });
        }
        if body.truncated {
            notes.push(if i18n::is_zh() {
                format!(
                    "代理按上限保留了前 {} / {}（可用 `--max-body unlimited` 全量捕获）。",
                    format_bytes(text.len()),
                    format_bytes(body.size())
                )
            } else {
                format!(
                    "Proxy captured {} of {} (`--max-body unlimited` keeps all).",
                    format_bytes(text.len()),
                    format_bytes(body.size())
                )
            });
        }
        if !notes.is_empty() {
            block = block.child(note_text(notes.join(" ")));
        }
        block
    }
}

pub(super) fn body_kind(content_type: &str, body: &Body) -> String {
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
    } else if content_type.is_empty() || content_type.contains("octet-stream") {
        "UTF-8".to_string()
    } else {
        "body".to_string()
    }
}

fn problem_panel(trace: &Trace) -> Option<Div> {
    let t = theme::p();
    let problem = trace.problem.as_ref()?;
    let heading = if trace.blocked {
        i18n::pick("BLOCKED BY LENSLET", "已被 LENSLET 实时拦截")
    } else {
        i18n::pick("WHY IT FAILED", "失败根因诊断")
    };
    let mut panel = div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .p(px(10.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(theme::line(t.red, 0.35))
        .bg(theme::h(t.red, 0.06))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(micro_label(heading))
                .child(div().flex_1())
                .child(
                    div()
                        .font_family(fonts::mono())
                        .text_size(px(11.0))
                        .text_color(status_color(trace.status.max(400)))
                        .child(status_label(trace.status)),
                ),
        )
        .child(
            div()
                .font_family(fonts::mono())
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(t.red))
                .child(problem.code.clone()),
        );
    if let Some(message) = problem.message.as_deref().filter(|m| !m.is_empty()) {
        panel = panel.child(
            div()
                .text_size(px(12.0))
                .line_height(px(16.5))
                .text_color(rgb(t.text_soft))
                .child(message.to_string()),
        );
    }
    if let Some(hint) = problem.hint.as_deref() {
        panel = panel.child(
            div()
                .flex()
                .items_start()
                .gap(px(6.0))
                .pt(px(2.0))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(t.accent))
                        .child("→"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .text_size(px(11.5))
                        .line_height(px(16.5))
                        .text_color(rgb(t.text_body))
                        .child(hint.to_string()),
                ),
        );
    }
    if let Some(error) = trace.error.as_deref() {
        panel = panel.child(note_text(error.to_string()));
    }
    Some(panel)
}
