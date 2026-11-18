//! The inspector: one call's overview, request and response.

use super::*;

impl LensletApp {
    pub(super) fn render_inspector(&mut self, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let trace = self
            .selected
            .and_then(|index| self.rows.get(index))
            .cloned();
        let body: AnyElement = if let Some(trace) = trace.as_ref() {
            self.render_inspector_body(trace, cx).into_any_element()
        } else {
            self.render_inspector_empty().into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .w(px(theme::INSPECTOR_W))
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
            .h(px(theme::STREAM_HEAD_H))
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

        let title = div()
            .mt(px(7.0))
            .text_size(px(16.0))
            .line_height(px(20.0))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(t.text))
            .whitespace_nowrap()
            .text_ellipsis()
            .child(operation_label(trace).to_string());

        let endpoint = self
            .run
            .as_ref()
            .map(|r| r.header.endpoint.as_str())
            .filter(|e| !e.is_empty());
        let curl_cmd = trace::to_curl(trace, endpoint);

        let path = div()
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
                    .text_size(px(12.0))
                    .text_color(rgb(t.text_dim))
                    .whitespace_nowrap()
                    .child(trace::ellipsize(trace::display_path(trace), 40)),
            )
            .child(self.copy_button("copy-path", "path", Some(trace.path.clone()), cx))
            .when_some(curl_cmd, |el, curl_cmd| {
                el.child(
                    action_button("copy-curl", i18n::pick("Copy curl", "复制 curl")).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.copy_text("curl", curl_cmd.clone(), cx);
                        }),
                    ),
                )
            });

        let (status_tint, status_value, status_words) = if trace.pending {
            (rgb(t.amber), "…".to_string(), "pending")
        } else {
            (
                status_color(trace.status),
                status_label(trace.status),
                status_text(trace.status),
            )
        };
        let status = div()
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
            );

        let mut meta = div()
            .flex()
            .items_center()
            .gap(px(9.0))
            .mt(px(6.0))
            .child(status)
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
                    .child(full_time(&trace.at)),
            )
            .child(div().flex_1());
        if trace.authorization || trace.presigned() {
            meta = meta.child(glyph::auth_mark(true)).child(
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(t.text_muted))
                    .child(if trace.authorization {
                        "sigv4"
                    } else {
                        "presigned"
                    }),
            );
        } else {
            meta = meta.child(glyph::auth_mark(false)).child(
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(t.text_faint))
                    .child("no auth"),
            );
        }

        let mut top = div().flex().items_center().gap(px(8.0)).child(service);
        if trace.blocked {
            top = top.child(blocked_pill("BLOCKED"));
        }
        top = top.child(div().flex_1());
        if let Some(id) = Some(trace.id.as_str()).filter(|id| !id.is_empty()) {
            top = top.child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(10.0))
                    .text_color(rgb(t.text_faint))
                    .child(format!("#{}", trace::clip_utf8(id, 18))),
            );
        }

        div()
            .flex()
            .flex_col()
            .px(px(14.0))
            .pt(px(12.0))
            .pb(px(8.0))
            .child(top)
            .child(title)
            .child(path)
            .child(meta)
            .child(self.render_tabs(cx))
    }

    pub(super) fn render_tabs(&self, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let mut tabs = Vec::new();
        for tab in [
            InspectorTab::Overview,
            InspectorTab::Request,
            InspectorTab::Response,
        ] {
            let active = self.tab == tab;
            tabs.push(
                div()
                    .id(ElementId::Name(format!("tab-{}", tab.index()).into()))
                    .h(px(36.0))
                    .flex()
                    .items_center()
                    .border_b_2()
                    .border_color(if active {
                        rgb(t.accent)
                    } else {
                        rgba(0x00000000)
                    })
                    .text_size(px(12.0))
                    .font_weight(if active {
                        FontWeight::MEDIUM
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
                    .child(tab.label()),
            );
        }

        div()
            .flex()
            .items_end()
            .gap(px(18.0))
            .mt(px(10.0))
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
                        "Click a row, or use ↑ ↓ to move through the stream.",
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
            InspectorTab::Overview => self.render_overview(trace).into_any_element(),
            InspectorTab::Request => self
                .render_exchange(trace, Side::Request, cx)
                .into_any_element(),
            InspectorTab::Response => self
                .render_exchange(trace, Side::Response, cx)
                .into_any_element(),
        };

        div()
            .id(ElementId::Name(format!("inspector-{}", trace.key()).into()))
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .scrollbar_width(px(6.0))
            .child(div().p(px(14.0)).w_full().child(content))
    }

    pub(super) fn render_overview(&self, trace: &Trace) -> Div {
        let t = theme::p();
        let request_id = trace
            .response
            .as_ref()
            .and_then(|response| {
                response
                    .header("x-amz-request-id")
                    .or_else(|| response.header("x-amzn-requestid"))
            })
            .or_else(|| {
                trace.request.as_ref().and_then(|request| {
                    request
                        .header("x-amz-request-id")
                        .or_else(|| request.header("x-amzn-requestid"))
                })
            });

        let (status_tint, status_line) = if trace.pending {
            (rgb(t.amber), "… pending".to_string())
        } else {
            (
                status_color(trace.status),
                format!(
                    "{} {}",
                    status_label(trace.status),
                    status_text(trace.status)
                ),
            )
        };
        let mut call = div().flex().flex_col();
        call = call
            .child(meta_row(
                "Status",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(glyph::dot(status_tint, 4.5))
                    .child(
                        div()
                            .font_family(fonts::mono())
                            .text_size(px(11.5))
                            .text_color(status_tint)
                            .child(status_line),
                    ),
            ))
            .child(meta_row(
                "Duration",
                mono_value(if trace.pending {
                    i18n::pick("still running", "正在运行").to_string()
                } else {
                    format_latency(trace.ms)
                }),
            ))
            .child(meta_row("Started", mono_value(full_time(&trace.at))))
            .child(meta_row(
                "Method",
                mono_value(trace.method.to_ascii_uppercase()),
            ));
        if trace.is_retry() || self.invocation_stats(trace).is_some() {
            let attempt = trace.attempt.unwrap_or(1);
            let mut label = match trace.max_attempts {
                Some(max) => format!("{attempt} of {max}"),
                None => format!("attempt {attempt}"),
            };
            if let Some((count, total_ms)) = self.invocation_stats(trace) {
                label.push_str(&format!(" · {count} tries ({})", format_latency(total_ms)));
            }
            call = call.child(meta_row("Retry", mono_value(label)));
        }

        let mut routing = div().flex().flex_col();
        routing = routing
            .child(meta_row(
                "Service",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(glyph::dot(theme::service_color(trace.service_name()), 4.5))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme::service_color(trace.service_name()))
                            .child(trace.service_name().to_string()),
                    ),
            ))
            .child(meta_row(
                "Operation",
                mono_value(trace::ellipsize(operation_label(trace), 44)),
            ))
            .child(meta_row(
                "Region",
                mono_value(trace.region.clone().unwrap_or_else(|| "—".to_string())),
            ))
            .child(meta_row(
                "Host",
                mono_value(if trace.host.is_empty() {
                    "—".to_string()
                } else {
                    trace.host.clone()
                }),
            ))
            .child(meta_row(
                "Request ID",
                mono_value(request_id.unwrap_or("—").to_string()),
            ));
        if let Some(id) = Some(trace.id.as_str()).filter(|id| !id.is_empty()) {
            routing = routing.child(meta_row("Trace ID", mono_value(id.to_string())));
        }

        let auth = if trace.authorization {
            "SigV4 authorization header"
        } else if trace.presigned() {
            "Presigned URL (query signature)"
        } else {
            "Unsigned"
        };

        let mut bodies = div().flex().flex_col();
        bodies = bodies
            .child(meta_row("Auth", text_value(auth.to_string(), t.text_soft)))
            .child(meta_row(
                "Request",
                text_value(body_summary(trace.request.as_ref()), t.text_body),
            ))
            .child(meta_row(
                "Response",
                text_value(body_summary(trace.response.as_ref()), t.text_body),
            ))
            .child(meta_row(
                "Content-Type",
                mono_value(
                    trace
                        .response
                        .as_ref()
                        .and_then(|response| response.content_type.clone())
                        .or_else(|| {
                            trace
                                .request
                                .as_ref()
                                .and_then(|request| request.content_type.clone())
                        })
                        .unwrap_or_else(|| "—".to_string()),
                ),
            ))
            .child(meta_row(
                "Path",
                mono_value(trace::ellipsize(trace::display_path(trace), 44)),
            ));

        div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .children(problem_panel(trace))
            .children(access_section(trace))
            .child(section(i18n::pick("CALL", "调用概览"), call))
            .child(section(i18n::pick("ROUTING", "路由参数"), routing))
            .child(section(i18n::pick("AUTH & BODIES", "认证与载荷"), bodies))
    }

    pub(super) fn render_exchange(&self, trace: &Trace, side: Side, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let exchange = match side {
            Side::Request => trace.request.as_ref(),
            Side::Response => trace.response.as_ref(),
        };
        let Some(exchange) = exchange else {
            return div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(
                    div()
                        .text_size(px(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(t.text_soft))
                        .child(if i18n::is_zh() {
                            format!("尚未捕获 {}", side.noun())
                        } else {
                            format!("No {} captured", side.noun())
                        }),
                )
                .child(
                    div()
                        .text_size(px(11.5))
                        .line_height(px(16.0))
                        .text_color(rgb(t.text_muted))
                        .child(i18n::pick(
                            "The trace entry for this call contains metadata only.",
                            "当前调用记录仅包含元数据。",
                        )),
                );
        };

        let target = match side {
            Side::Request => format!(
                "{} {}",
                trace.method.to_ascii_uppercase(),
                trace::display_path(trace)
            ),
            Side::Response => format!(
                "HTTP {} {}",
                status_label(trace.status),
                status_text(trace.status)
            ),
        };

        let headers_text = headers_to_text(&exchange.headers);
        let headers_action = if exchange.headers.is_empty() {
            None
        } else {
            Some(self.copy_button(
                match side {
                    Side::Request => "copy-req-headers",
                    Side::Response => "copy-res-headers",
                },
                "headers",
                Some(headers_text.clone()),
                cx,
            ))
        };

        let body = exchange.body.as_ref();
        let body_actions = self.render_body_actions(body, &exchange.content_type, side, cx);

        let mut header_meta = vec![format!(
            "{} {}",
            exchange.headers.len(),
            if exchange.headers.len() == 1 {
                "entry"
            } else {
                "entries"
            }
        )];
        if let Some(content_type) = exchange.content_type.as_deref() {
            header_meta.push(content_type.to_string());
        }

        let mut body_meta = Vec::new();
        if let Some(body) = body {
            body_meta.push(format_bytes(body.size()));
            if let Some(content_type) = exchange.content_type.as_deref() {
                body_meta.push(body_kind(content_type, body));
            } else if !body.binary {
                body_meta.push("UTF-8".to_string());
            }
            if let Some(enc) = body.encoding.as_deref() {
                body_meta.push(enc.to_string());
            }
            if body.truncated {
                body_meta.push("truncated".to_string());
            }
        }

        div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(section_with_action(
                side.target_label(),
                None,
                Some(self.copy_button(
                    match side {
                        Side::Request => "copy-req-target",
                        Side::Response => "copy-res-target",
                    },
                    match side {
                        Side::Request => "path",
                        Side::Response => "status",
                    },
                    Some(match side {
                        Side::Request => trace.path.clone(),
                        Side::Response => target.clone(),
                    }),
                    cx,
                )),
                code_block(
                    match side {
                        Side::Request => "req-target",
                        Side::Response => "res-target",
                    },
                    target.clone(),
                    68.0,
                ),
            ))
            .child(section_with_action(
                "HEADERS",
                Some(header_meta.join(" · ")),
                headers_action,
                if exchange.headers.is_empty() {
                    empty_note(i18n::pick("No headers captured", "未捕获到 Headers"))
                        .into_any_element()
                } else {
                    code_block(
                        match side {
                            Side::Request => "req-headers",
                            Side::Response => "res-headers",
                        },
                        headers_text,
                        210.0,
                    )
                    .into_any_element()
                },
            ))
            .child(section_with_action(
                "BODY",
                Some(body_meta.join(" · ")),
                body_actions,
                self.render_body(
                    body,
                    &exchange.content_type,
                    match side {
                        Side::Request => "req-body",
                        Side::Response => "res-body",
                    },
                ),
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
            let can_pretty =
                trace::pretty_body(text, content_type.as_deref().unwrap_or("")).is_some();
            if can_pretty {
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
            return empty_note(i18n::pick("No body captured", "未捕获到 Body"));
        };

        if body.binary {
            let mut block = div().flex().flex_col().gap(px(6.0));
            if let Some(hex_str) = body.hex.as_deref().filter(|h| !h.is_empty()) {
                let dump = trace::format_hex_dump(hex_str);
                block = block.child(code_block(code_id, dump, 260.0));
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
            return empty_note("Empty body · 0 B");
        }

        let pretty = if self.raw_body {
            None
        } else {
            trace::pretty_body(text, content_type.as_deref().unwrap_or(""))
        };
        let display = pretty.as_deref().unwrap_or(text);
        let clipped = !self.expand_body && display.len() > MAX_BODY_DISPLAY;
        let shown = if clipped {
            trace::clip_utf8(display, MAX_BODY_DISPLAY)
        } else {
            display
        };
        let mut block = div().flex().flex_col().gap(px(6.0));
        block = block.child(code_block(
            code_id,
            shown.to_string(),
            if self.expand_body { 620.0 } else { 380.0 },
        ));
        let mut notes = Vec::new();
        if pretty.is_some() {
            notes.push(
                i18n::pick(
                    "Formatted for reading; Copy gives the raw unredacted body.",
                    "已自动缩进便于阅读；点击“复制”或切换“原始”可查看未脱敏原始字节内容。",
                )
                .to_string(),
            );
        }
        if clipped {
            notes.push(if i18n::is_zh() {
                format!(
                    "预览截断在 {}；点击上方“展开全文”或“复制”可获取全部已捕获内容。",
                    format_bytes(MAX_BODY_DISPLAY)
                )
            } else {
                format!(
                    "Display capped at {}; click Full or Copy for everything captured.",
                    format_bytes(MAX_BODY_DISPLAY)
                )
            });
        }
        if body.truncated {
            notes.push(if i18n::is_zh() {
                format!(
                    "代理按捕获上限保留了前 {}（总计 {}），可用 `--max-body unlimited` 全量捕获。",
                    format_bytes(text.len()),
                    format_bytes(body.size())
                )
            } else {
                format!(
                    "Truncated at proxy limit: captured {} of {} (`--max-body unlimited` keeps all).",
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

pub(super) fn body_summary(exchange: Option<&Exchange>) -> String {
    let Some(exchange) = exchange else {
        return i18n::pick("not captured", "未捕获").to_string();
    };
    let Some(body) = exchange.body.as_ref() else {
        return i18n::pick("no body", "无 Body").to_string();
    };
    if body.binary {
        let mut summary = format!("binary · {}", format_bytes(body.size()));
        if body.hex.is_some() {
            summary.push_str(" · hex");
        }
        if body.truncated {
            summary.push_str(" · truncated");
        }
        return summary;
    }
    if body.is_empty() {
        return "empty · 0 B".to_string();
    }
    match body.text.as_deref() {
        Some(text) if !text.is_empty() || body.encoding.is_some() => {
            let mut summary = format!(
                "{} · {}",
                body_kind(exchange.content_type.as_deref().unwrap_or(""), body),
                format_bytes(body.size())
            );
            if let Some(enc) = body.encoding.as_deref() {
                summary.push_str(&format!(" · {enc}"));
            }
            if body.truncated {
                summary.push_str(" · truncated");
            }
            summary
        }
        _ => format!("text unavailable · {}", format_bytes(body.size())),
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

/// The answer to "why did this fail?", above everything else.
fn problem_panel(trace: &Trace) -> Option<Div> {
    let t = theme::p();
    let problem = trace.problem.as_ref()?;
    let heading = if trace.blocked {
        i18n::pick("BLOCKED BY LENSLET", "已被 LENSLET 实时拦截")
    } else {
        i18n::pick("WHY IT FAILED", "失败原因")
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
                .text_size(px(12.5))
                .line_height(px(17.0))
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
                        .text_size(px(12.0))
                        .line_height(px(17.0))
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

/// The permissions this call needs: its own, and implied ones elsewhere.
fn access_section(trace: &Trace) -> Option<Div> {
    let t = theme::p();
    let entries: Vec<(&trace::CallAccess, bool)> = trace.call_access().collect();
    if entries.is_empty() {
        return None;
    }
    let mut list = div().flex().flex_col();
    for (access, implied) in entries {
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
                .flex()
                .flex_col()
                .gap(px(3.0))
                .py(px(6.0))
                .border_b_1()
                .border_color(rgb(t.border_subtle))
                .child(name)
                .child(mono_value(access.resource.clone())),
        );
    }
    Some(section(i18n::pick("ACCESS", "IAM 权限"), list))
}
