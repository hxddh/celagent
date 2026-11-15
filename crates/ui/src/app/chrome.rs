//! Window chrome: header, toolbar with filters, service strip, footer.

use super::*;

impl LensletApp {
    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> Div {
        let dot = if self.paused {
            theme::h(theme::AMBER, 0.95)
        } else {
            self.proxy_light().dot
        };
        let kind = if self.demo {
            "DEMO".to_string()
        } else {
            self.run
                .as_ref()
                .map(|run| run.kind().to_ascii_uppercase())
                .unwrap_or_default()
        };
        let pause_icon = if self.paused {
            glyph::play_icon(theme::h(theme::AMBER, 0.95)).into_any_element()
        } else {
            glyph::pause_icon(theme::h(theme::TEXT_DIM, 0.95)).into_any_element()
        };

        let mut brand = div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(glyph::lens_mark(24.0))
            .child(
                div()
                    .text_size(px(14.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(theme::TEXT))
                    .child("Lenslet"),
            )
            .child(self.render_run_summary(cx));
        if self.demo {
            brand = brand.child(
                div()
                    .h(px(16.0))
                    .px(px(5.0))
                    .flex()
                    .items_center()
                    .rounded(px(3.0))
                    .border_1()
                    .border_color(theme::line(theme::AMBER, 0.35))
                    .bg(theme::h(theme::AMBER, 0.08))
                    .text_size(px(9.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(theme::AMBER))
                    .child("DEMO"),
            );
        }

        div()
            .flex()
            .items_center()
            .h(px(theme::HEADER_H))
            .px(px(14.0))
            .gap(px(14.0))
            .bg(rgb(theme::HEADER))
            .border_b_1()
            .border_color(rgb(theme::BORDER_SUBTLE))
            .child(brand)
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .h(px(26.0))
                    .px(px(9.0))
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(theme::BORDER))
                    .bg(theme::h(theme::TEXT, 0.02))
                    .child(glyph::dot(dot, 5.5))
                    .child(
                        div()
                            .font_family(fonts::mono())
                            .text_size(px(11.5))
                            .text_color(rgb(theme::TEXT_DIM))
                            .child(self.endpoint_label()),
                    )
                    .child(
                        div()
                            .text_size(px(9.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(theme::TEXT_FAINT))
                            .child(kind),
                    ),
            )
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
                        theme::line(theme::AMBER, 0.35)
                    } else {
                        theme::h(theme::BORDER, 1.0)
                    })
                    .bg(if self.paused {
                        theme::h(theme::AMBER, 0.07)
                    } else {
                        theme::h(theme::TEXT, 0.02)
                    })
                    .text_size(px(11.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if self.paused {
                        rgb(theme::AMBER)
                    } else {
                        rgb(theme::TEXT_DIM)
                    })
                    .cursor_pointer()
                    .hover(|style| {
                        style
                            .bg(theme::h(theme::TEXT, 0.06))
                            .text_color(rgb(theme::TEXT))
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_pause(cx)))
                    .child(pause_icon)
                    .child(if self.paused { "Resume" } else { "Pause" }),
            )
    }

    pub(super) fn render_toolbar(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        let limit = if compact { 3 } else { 5 };
        let visible = self.visible_facets(limit);
        let hidden = self.facets.len().saturating_sub(visible.len());
        let visible_chips: Vec<Stateful<Div>> = visible
            .iter()
            .map(|facet| self.service_chip(facet, "service-chip", cx))
            .collect();
        let search_width = if compact { 248.0 } else { 320.0 };

        let mut chip_row = div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .flex_1()
            .min_w(px(0.0))
            .overflow_hidden()
            .child(self.chip(
                "filter-all",
                "All",
                self.counts.total,
                None,
                self.filter == Filter::All,
                cx,
            ))
            .child(self.chip(
                "filter-errors",
                "Errors",
                self.counts.errors,
                Some(theme::RED),
                self.filter == Filter::Errors,
                cx,
            ))
            .children(visible_chips);
        if let Filter::Access(access) = &self.filter {
            chip_row = chip_row.child(
                div()
                    .id("filter-access")
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(6.0))
                    .h(px(25.0))
                    .px(px(8.0))
                    .rounded(px(5.0))
                    .border_1()
                    .border_color(theme::line(theme::ACCENT, 0.45))
                    .bg(theme::h(theme::ACCENT, 0.08))
                    .font_family(fonts::mono())
                    .text_size(px(10.5))
                    .text_color(rgb(theme::TEXT))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.set_filter(Filter::All, cx)))
                    .child(trace::ellipsize(&access.action, 34))
                    .child(div().text_color(rgb(theme::TEXT_MUTED)).child("×")),
            );
        }

        if hidden > 0 || self.show_all_services {
            chip_row = chip_row.child(
                div()
                    .id("more-services")
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .h(px(25.0))
                    .px(px(6.0))
                    .rounded(px(5.0))
                    .text_size(px(11.5))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .cursor_pointer()
                    .hover(|style| {
                        style
                            .bg(theme::h(theme::TEXT, 0.05))
                            .text_color(rgb(theme::TEXT_SOFT))
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_all_services = !this.show_all_services;
                        cx.notify();
                    }))
                    .child(if self.show_all_services {
                        "Less".to_string()
                    } else {
                        format!("More +{hidden}")
                    }),
            );
        }

        let (state_dot, state_label, state_color) = if self.demo {
            (
                theme::h(theme::AMBER, 0.95),
                if self.paused {
                    "PAUSED · DEMO".to_string()
                } else {
                    "DEMO DATA".to_string()
                },
                rgb(theme::AMBER),
            )
        } else if self.paused {
            (
                theme::h(theme::AMBER, 0.95),
                if self.buffered.is_empty() {
                    "PAUSED".to_string()
                } else {
                    format!("PAUSED · {} BUFFERED", self.buffered.len())
                },
                rgb(theme::AMBER),
            )
        } else {
            match &self.health {
                TraceHealth::Ok => match self.run.as_ref().filter(|run| run.ended()) {
                    Some(run) => (
                        theme::h(theme::TEXT_FAINT, 0.9),
                        match run.exit_code() {
                            Some(code) => format!("RUN ENDED · EXIT {code}"),
                            None => "PROXY STOPPED".to_string(),
                        },
                        rgb(theme::TEXT_MUTED),
                    ),
                    None => (
                        theme::h(theme::GREEN, 0.95),
                        "LIVE".to_string(),
                        rgb(theme::TEXT_MUTED),
                    ),
                },
                TraceHealth::Missing => (
                    theme::h(theme::AMBER, 0.95),
                    "WAITING FOR A RUN".to_string(),
                    rgb(theme::AMBER),
                ),
                TraceHealth::Error(_) => (
                    theme::h(theme::RED, 0.95),
                    "TRACE ERROR".to_string(),
                    rgb(theme::RED),
                ),
            }
        };

        div()
            .flex()
            .items_center()
            .h(px(theme::TOOLBAR_H))
            .px(px(14.0))
            .gap(px(10.0))
            .bg(rgb(theme::TOOLBAR))
            .border_b_1()
            .border_color(rgb(theme::BORDER_SUBTLE))
            .child(div().w(px(search_width)).child(self.search.clone()))
            .child(chip_row)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .flex_shrink_0()
                    .child(glyph::dot(state_dot, 5.0))
                    .child(
                        div()
                            .font_family(fonts::mono())
                            .text_size(px(10.5))
                            .text_color(state_color)
                            .child(state_label),
                    ),
            )
    }

    pub(super) fn render_service_strip(&self, cx: &mut Context<Self>) -> Div {
        let chips: Vec<Stateful<Div>> = self
            .facets
            .iter()
            .map(|facet| self.service_chip(facet, "service-strip", cx))
            .collect();

        div()
            .flex()
            .items_center()
            .h(px(32.0))
            .px(px(14.0))
            .gap(px(8.0))
            .bg(rgb(0x0B0E14))
            .border_b_1()
            .border_color(rgb(theme::BORDER_SUBTLE))
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
                action_button("close-services", "Close").on_click(cx.listener(|this, _, _, cx| {
                    this.show_all_services = false;
                    cx.notify();
                })),
            )
    }

    pub(super) fn visible_facets(&self, limit: usize) -> Vec<&ServiceFacet> {
        let mut visible: Vec<&ServiceFacet> = self.facets.iter().take(limit).collect();
        if let Filter::Service(active) = &self.filter
            && !visible.iter().any(|facet| &facet.id == active)
            && let Some(facet) = self.facets.iter().find(|facet| &facet.id == active)
        {
            if !visible.is_empty() {
                visible.pop();
            }
            visible.push(facet);
        }
        visible
    }

    pub(super) fn chip(
        &self,
        id: &'static str,
        label: &'static str,
        count: usize,
        dot: Option<u32>,
        active: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let error_color = dot == Some(theme::RED);
        let filter = if id == "filter-all" {
            Filter::All
        } else {
            Filter::Errors
        };
        let mut chip = div()
            .id(id)
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(5.0))
            .h(px(25.0))
            .px(px(7.0))
            .rounded(px(5.0))
            .border_1()
            .border_color(if active {
                rgb(theme::BORDER_STRONG)
            } else {
                rgba(0x00000000)
            })
            .bg(if active {
                theme::h(theme::TEXT, 0.06)
            } else {
                theme::h(0x000000, 0.0)
            })
            .text_size(px(11.5))
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
            .hover(|style| {
                style
                    .bg(theme::h(theme::TEXT, 0.05))
                    .text_color(rgb(theme::TEXT_SOFT))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_filter(filter.clone(), cx);
            }));

        if let Some(color) = dot {
            chip = chip.child(glyph::dot(
                theme::h(color, if error_color { 0.95 } else { 0.8 }),
                4.5,
            ));
        }
        chip = chip.child(div().child(label)).child(
            div()
                .font_family(fonts::mono())
                .text_size(px(10.0))
                .text_color(if error_color && count > 0 {
                    rgb(theme::RED)
                } else {
                    rgb(theme::TEXT_FAINT)
                })
                .child(count.to_string()),
        );
        chip
    }

    pub(super) fn service_chip(
        &self,
        facet: &ServiceFacet,
        scope: &'static str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let active = self.filter == Filter::Service(facet.id.clone());
        let color = theme::service_color(&facet.label);
        div()
            .id(ElementId::Name(format!("{scope}-{}", facet.id).into()))
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(5.0))
            .h(px(25.0))
            .px(px(7.0))
            .rounded(px(5.0))
            .border_1()
            .border_color(if active {
                theme::tint(color, 0.55)
            } else {
                theme::h(0x000000, 0.0)
            })
            .bg(if active {
                theme::h(theme::TEXT, 0.06)
            } else {
                theme::h(0x000000, 0.0)
            })
            .text_size(px(11.5))
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
            .hover(|style| {
                style
                    .bg(theme::h(theme::TEXT, 0.05))
                    .text_color(rgb(theme::TEXT_SOFT))
            })
            .on_click(cx.listener({
                let id = facet.id.clone();
                move |this, _, _, cx| {
                    this.set_filter(Filter::Service(id.clone()), cx);
                }
            }))
            .child(glyph::dot(color, 4.5))
            .child(div().whitespace_nowrap().child(facet.label.clone()))
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(10.5))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(facet.count.to_string()),
            )
    }

    pub(super) fn render_footer(&self, compact: bool) -> Div {
        let center = if let Some((message, _)) = self.toast.as_ref() {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(glyph::dot(theme::h(theme::GREEN, 0.95), 4.5))
                .child(
                    div()
                        .font_family(fonts::mono())
                        .text_size(px(10.5))
                        .text_color(rgb(theme::GREEN))
                        .child(message.clone()),
                )
                .into_any_element()
        } else {
            div()
                .font_family(fonts::mono())
                .text_size(px(10.5))
                .text_color(rgb(theme::TEXT_MUTED))
                .child(format!(
                    "{} · {} · {} {}",
                    call_count(self.counts.total),
                    error_count(self.counts.errors),
                    self.counts.services,
                    if self.counts.services == 1 {
                        "service"
                    } else {
                        "services"
                    }
                ))
                .into_any_element()
        };

        let mut footer = div()
            .flex()
            .items_center()
            .h(px(theme::FOOTER_H))
            .px(px(14.0))
            .gap(px(14.0))
            .bg(rgb(theme::FOOTER))
            .border_t_1()
            .border_color(rgb(theme::BORDER_SUBTLE))
            .children(
                [Some(self.proxy_light()), self.emulator_light()]
                    .into_iter()
                    .flatten()
                    .map(|light| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(glyph::dot(light.dot, 4.5))
                            .child(
                                div()
                                    .font_family(fonts::mono())
                                    .text_size(px(10.0))
                                    .text_color(rgb(light.color))
                                    .whitespace_nowrap()
                                    .child(light.text),
                            )
                    }),
            )
            .child(div().flex_1())
            .child(center)
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(concat!("lenslet ", env!("CARGO_PKG_VERSION"))),
            );

        footer = footer.child(
            div()
                .font_family(fonts::mono())
                .text_size(px(10.0))
                .text_color(rgb(theme::TEXT_FAINT))
                .child(if compact {
                    "a access · ↑↓ · space pause"
                } else {
                    "a access · space pause · 1 2 3 tabs"
                }),
        );
        footer
    }

    /// `‹ 3/7 ›  $ npm test  exit 1  +2 access  1 new failure`: the run at a glance.
    fn render_run_summary(&self, cx: &mut Context<Self>) -> Div {
        let mut line = div().flex().items_center().gap(px(8.0)).min_w(px(0.0));
        if let Some((position, total)) = self.run_position() {
            let pinned = position < total;
            line = line.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(
                        nav_button("run-prev", "‹", position > 1)
                            .on_click(cx.listener(|this, _, _, cx| this.step_run(-1, cx))),
                    )
                    .child(
                        div()
                            .font_family(fonts::mono())
                            .text_size(px(10.5))
                            .text_color(if pinned {
                                rgb(theme::AMBER)
                            } else {
                                rgb(theme::TEXT_MUTED)
                            })
                            .child(format!("{position}/{total}")),
                    )
                    .child(
                        nav_button("run-next", "›", pinned)
                            .on_click(cx.listener(|this, _, _, cx| this.step_run(1, cx))),
                    ),
            );
        }
        let Some(run) = self.run.as_ref() else {
            return line.child(
                div()
                    .text_size(px(11.5))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child("local AWS lens"),
            );
        };
        let command = if run.header.command.is_empty() {
            "proxy session".to_string()
        } else {
            format!("$ {}", trace::ellipsize(&run.command_line(), 40))
        };
        line = line.child(
            div()
                .font_family(fonts::mono())
                .text_size(px(11.5))
                .text_color(rgb(theme::TEXT_SOFT))
                .whitespace_nowrap()
                .child(command),
        );
        match run.exit_code() {
            Some(0) => line = line.child(badge("exit 0", theme::GREEN)),
            Some(code) => line = line.child(badge(format!("exit {code}"), theme::RED)),
            None if !run.ended() => line = line.child(badge("running", theme::ACCENT)),
            None => {}
        }
        let added = match run.report.as_ref() {
            Some(report) => report.access_added.len(),
            None => self.access_rows().iter().filter(|row| row.new).count(),
        };
        if added > 0 {
            line = line.child(badge(format!("+{added} access"), theme::AMBER));
        }
        if let Some(regressions) = run
            .report
            .as_ref()
            .and_then(|report| report.regressions.as_ref())
            && !regressions.new_failures.is_empty()
        {
            let count = regressions.new_failures.len();
            line = line.child(badge(
                format!(
                    "{count} new {}",
                    if count == 1 { "failure" } else { "failures" }
                ),
                theme::RED,
            ));
        }
        line
    }
}

/// A small outlined label in an accent color.
fn badge(text: impl Into<SharedString>, color: u32) -> Div {
    div()
        .h(px(18.0))
        .px(px(6.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme::line(color, 0.35))
        .bg(theme::h(color, 0.08))
        .font_family(fonts::mono())
        .text_size(px(10.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(color))
        .whitespace_nowrap()
        .child(text.into())
}

fn nav_button(id: &'static str, label: &'static str, enabled: bool) -> Stateful<Div> {
    let button = div()
        .id(id)
        .w(px(18.0))
        .h(px(20.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .text_size(px(14.0))
        .text_color(if enabled {
            rgb(theme::TEXT_DIM)
        } else {
            rgb(theme::TEXT_FAINT)
        })
        .child(label);
    if enabled {
        button.cursor_pointer().hover(|style| {
            style
                .bg(theme::h(theme::TEXT, 0.06))
                .text_color(rgb(theme::TEXT))
        })
    } else {
        button
    }
}
