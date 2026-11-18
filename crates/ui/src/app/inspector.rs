//! The inspector: one call's overview, request and response.

use super::*;

impl LensletApp {
    pub(super) fn render_inspector(&mut self, cx: &mut Context<Self>) -> Div {
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
            .bg(rgb(theme::PANEL_RAISED))
            .child(if let Some(trace) = trace.as_ref() {
                self.render_inspector_header(trace, cx)
            } else {
                self.render_inspector_header_empty()
            })
            .child(body)
    }

    pub(super) fn render_inspector_header_empty(&self) -> Div {
        div()
            .flex()
            .items_center()
            .h(px(theme::STREAM_HEAD_H))
            .px(px(14.0))
            .border_b_1()
            .border_color(rgb(theme::BORDER_SUBTLE))
            .child(micro_label("INSPECTOR"))
    }

    pub(super) fn render_inspector_header(&self, trace: &Trace, cx: &mut Context<Self>) -> Div {
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
            .bg(theme::h(theme::TEXT, 0.03))
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
            .text_color(rgb(theme::TEXT))
            .whitespace_nowrap()
            .text_ellipsis()
            .child(operation_label(trace).to_string());

        let path = div()
            .flex()
            .items_center()
            .gap(px(7.0))
            .mt(px(5.0))
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(11.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(theme::ACCENT))
                    .child(trace.method.to_ascii_uppercase()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .font_family(fonts::mono())
                    .text_size(px(12.0))
                    .text_color(rgb(theme::TEXT_DIM))
                    .whitespace_nowrap()
                    .child(trace::ellipsize(trace::display_path(trace), 48)),
            )
            .child(self.copy_button("copy-path", "path", Some(trace.path.clone()), cx));

        let (status_tint, status_value, status_words) = if trace.pending {
            (rgb(theme::AMBER), "…".to_string(), "pending")
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
                    .text_color(rgb(theme::TEXT_FAINT))
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
                        rgb(theme::AMBER)
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
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(full_time(&trace.at)),
            )
            .child(div().flex_1());
        if trace.authorization || trace.presigned() {
            meta = meta.child(glyph::auth_mark(true)).child(
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(theme::TEXT_MUTED))
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
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child("no auth"),
            );
        }

        let mut top = div().flex().items_center().gap(px(8.0)).child(service);
        top = top.child(div().flex_1());
        if let Some(id) = Some(trace.id.as_str()).filter(|id| !id.is_empty()) {
            top = top.child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(10.0))
                    .text_color(rgb(theme::TEXT_FAINT))
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
                        rgb(theme::ACCENT)
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
                        rgb(theme::TEXT)
                    } else {
                        rgb(theme::TEXT_MUTED)
                    })
                    .cursor_pointer()
                    .hover(|style| style.text_color(rgb(theme::TEXT_SOFT)))
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
            .border_color(rgb(theme::BORDER_SUBTLE))
            .children(tabs)
            .child(div().flex_1())
            .child(
                div()
                    .pb(px(5.0))
                    .font_family(fonts::mono())
                    .text_size(px(9.5))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child("1 2 3"),
            )
    }

    pub(super) fn render_inspector_empty(&self) -> Div {
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
                    .text_color(rgb(theme::TEXT_SOFT))
                    .child("Select a call"),
            )
            .child(
                div()
                    .text_size(px(11.5))
                    .line_height(px(16.0))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .text_center()
                    .child("Click a row, or use ↑ ↓ to move through the stream."),
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
            (rgb(theme::AMBER), "… pending".to_string())
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
                    "still running".to_string()
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
            "Presigned URL (signature in the query)"
        } else {
            "Unsigned"
        };

        let mut bodies = div().flex().flex_col();
        bodies = bodies
            .child(meta_row(
                "Auth",
                text_value(auth.to_string(), theme::TEXT_SOFT),
            ))
            .child(meta_row(
                "Request",
                text_value(body_summary(trace.request.as_ref()), theme::TEXT_BODY),
            ))
            .child(meta_row(
                "Response",
                text_value(body_summary(trace.response.as_ref()), theme::TEXT_BODY),
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
            .child(section("CALL", call))
            .child(section("ROUTING", routing))
            .child(section("AUTH & BODIES", bodies))
    }

    pub(super) fn render_exchange(&self, trace: &Trace, side: Side, cx: &mut Context<Self>) -> Div {
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
                        .text_color(rgb(theme::TEXT_SOFT))
                        .child(format!("No {} captured", side.noun())),
                )
                .child(
                    div()
                        .text_size(px(11.5))
                        .line_height(px(16.0))
                        .text_color(rgb(theme::TEXT_MUTED))
                        .child("The trace entry for this call contains metadata only."),
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
        let body_text = body.and_then(|body| body.text.clone());
        let body_action = body_text
            .as_ref()
            .filter(|text| !text.is_empty())
            .map(|text| {
                self.copy_button(
                    match side {
                        Side::Request => "copy-req-body",
                        Side::Response => "copy-res-body",
                    },
                    "body",
                    Some(text.clone()),
                    cx,
                )
            });

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
                    empty_note("No headers captured").into_any_element()
                } else {
                    code_block(
                        match side {
                            Side::Request => "req-headers",
                            Side::Response => "res-headers",
                        },
                        headers_text,
                        190.0,
                    )
                    .into_any_element()
                },
            ))
            .child(section_with_action(
                "BODY",
                Some(body_meta.join(" · ")),
                body_action,
                render_body(
                    body,
                    &exchange.content_type,
                    match side {
                        Side::Request => "req-body",
                        Side::Response => "res-body",
                    },
                ),
            ))
    }
}

pub(super) fn render_body(
    body: Option<&Body>,
    content_type: &Option<String>,
    code_id: &'static str,
) -> Div {
    let Some(body) = body else {
        return empty_note("No body captured");
    };

    if body.binary {
        let mut block = div()
            .flex()
            .flex_col()
            .gap(px(5.0))
            .p(px(10.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme::BORDER))
            .bg(rgb(theme::CODE))
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(rgb(theme::TEXT_BODY))
                    .child(format!("Binary body · {}", format_bytes(body.size()))),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child("Binary payloads are recorded as metadata."),
            );
        if let Some(sha) = body.sha256.as_deref() {
            block = block.child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(10.5))
                    .text_color(rgb(theme::TEXT_DIM))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(format!("sha256 {sha}")),
            );
        }
        return block;
    }

    let Some(text) = body.text.as_deref() else {
        return empty_note(&format!(
            "Body text not captured · {}",
            format_bytes(body.size())
        ));
    };

    if text.is_empty() && body.size() == 0 {
        return empty_note("Empty body · 0 B");
    }

    // Pretty-print for reading; Copy always gives the bytes as captured.
    let pretty = trace::pretty_body(text, content_type.as_deref().unwrap_or(""));
    let display = pretty.as_deref().unwrap_or(text);
    let clipped = display.len() > MAX_BODY_DISPLAY;
    let shown = if clipped {
        trace::clip_utf8(display, MAX_BODY_DISPLAY)
    } else {
        display
    };
    let mut block = div().flex().flex_col().gap(px(6.0));
    block = block.child(code_block(code_id, shown.to_string(), 340.0));
    let mut notes = Vec::new();
    if pretty.is_some() {
        notes.push("Formatted for reading; Copy gives the raw body.".to_string());
    }
    if clipped {
        notes.push(format!(
            "Display capped at {}; Copy includes everything captured.",
            format_bytes(MAX_BODY_DISPLAY)
        ));
    }
    if body.truncated {
        notes.push(format!(
            "Truncated by the proxy: the first {} of {} were captured.",
            format_bytes(text.len()),
            format_bytes(body.size())
        ));
    }
    if !notes.is_empty() {
        block = block.child(note_text(notes.join(" ")));
    }
    block
}

pub(super) fn body_summary(exchange: Option<&Exchange>) -> String {
    let Some(exchange) = exchange else {
        return "not captured".to_string();
    };
    let Some(body) = exchange.body.as_ref() else {
        return "no body".to_string();
    };
    if body.binary {
        let mut summary = format!("binary · {}", format_bytes(body.size()));
        if body.truncated {
            summary.push_str(" · truncated");
        }
        return summary;
    }
    if body.is_empty() {
        return "empty · 0 B".to_string();
    }
    match body.text.as_deref() {
        Some(text) if !text.is_empty() => {
            let mut summary = format!(
                "{} · {}",
                body_kind(exchange.content_type.as_deref().unwrap_or(""), body),
                format_bytes(body.size())
            );
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
    } else {
        "body".to_string()
    }
}

/// The answer to "why did this fail?", above everything else.
fn problem_panel(trace: &Trace) -> Option<Div> {
    let problem = trace.problem.as_ref()?;
    let mut panel = div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .p(px(10.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(theme::line(theme::RED, 0.35))
        .bg(theme::h(theme::RED, 0.06))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(micro_label("WHY IT FAILED"))
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
                .text_color(rgb(theme::RED))
                .child(problem.code.clone()),
        );
    if let Some(message) = problem.message.as_deref().filter(|m| !m.is_empty()) {
        panel = panel.child(
            div()
                .text_size(px(12.5))
                .line_height(px(17.0))
                .text_color(rgb(theme::TEXT_SOFT))
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
                        .text_color(rgb(theme::ACCENT))
                        .child("→"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .text_size(px(12.0))
                        .line_height(px(17.0))
                        .text_color(rgb(theme::TEXT_BODY))
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
                .text_color(rgb(theme::TEXT))
                .child(access.action.clone()),
        );
        if implied {
            name = name.child(
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child("implied"),
            );
        }
        if trace.is_access_new(access) {
            name = name.child(new_pill("NOT IN LOCK"));
        }
        list = list.child(
            div()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .py(px(6.0))
                .border_b_1()
                .border_color(rgb(theme::BORDER_SUBTLE))
                .child(name)
                .child(mono_value(access.resource.clone())),
        );
    }
    Some(section("ACCESS", list))
}
