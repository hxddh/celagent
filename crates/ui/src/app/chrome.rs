//! Window chrome: header, toolbar with filters, service strip, footer.

use super::*;

impl LensletApp {
    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let dot = if self.paused {
            theme::h(t.amber, 0.95)
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
            glyph::play_icon(theme::h(t.amber, 0.95)).into_any_element()
        } else {
            glyph::pause_icon(theme::h(t.text_dim, 0.95)).into_any_element()
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
                    .text_color(rgb(t.text))
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
                    .border_color(theme::line(t.amber, 0.35))
                    .bg(theme::h(t.amber, 0.08))
                    .text_size(px(9.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(t.amber))
                    .child("DEMO"),
            );
        }

        div()
            .flex()
            .items_center()
            .h(px(theme::HEADER_H))
            .px(px(14.0))
            .gap(px(8.0))
            .bg(rgb(t.header))
            .border_b_1()
            .border_color(rgb(t.border_subtle))
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
                    .border_color(rgb(t.border))
                    .bg(theme::h(t.text, 0.02))
                    .child(glyph::dot(dot, 5.5))
                    .child(
                        div()
                            .font_family(fonts::mono())
                            .text_size(px(11.5))
                            .text_color(rgb(t.text_dim))
                            .child(self.endpoint_label()),
                    )
                    .child(
                        div()
                            .text_size(px(9.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(t.text_faint))
                            .child(kind),
                    ),
            )
            .child(
                header_pill("theme-button", i18n::theme_button_label(self.prefs.theme))
                    .on_click(cx.listener(|this, _, _, cx| this.cycle_theme(cx))),
            )
            .child(
                header_pill("lang-button", i18n::lang_button_label())
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_lang(cx))),
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
                        theme::line(t.amber, 0.35)
                    } else {
                        theme::h(t.border, 1.0)
                    })
                    .bg(if self.paused {
                        theme::h(t.amber, 0.07)
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
                    .hover(move |style| style.bg(theme::h(t.text, 0.06)).text_color(rgb(t.text)))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_pause(cx)))
                    .child(pause_icon)
                    .child(if self.paused {
                        i18n::pick("Resume", "继续")
                    } else {
                        i18n::pick("Pause", "暂停")
                    }),
            )
    }

    pub(super) fn render_toolbar(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
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
                i18n::pick("All", "全部"),
                self.counts.total,
                None,
                Filter::All,
                self.filter == Filter::All,
                cx,
            ))
            .child(self.chip(
                "filter-errors",
                i18n::pick("Errors", "错误"),
                self.counts.errors,
                Some(t.red),
                Filter::Errors,
                self.filter == Filter::Errors,
                cx,
            ));
        if self.counts.blocked > 0 || self.filter == Filter::Blocked {
            chip_row = chip_row.child(self.chip(
                "filter-blocked",
                i18n::pick("Blocked", "已拦截"),
                self.counts.blocked,
                Some(t.red),
                Filter::Blocked,
                self.filter == Filter::Blocked,
                cx,
            ));
        }
        chip_row = chip_row.children(visible_chips);
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
                    .border_color(theme::line(t.accent, 0.45))
                    .bg(theme::h(t.accent, 0.08))
                    .font_family(fonts::mono())
                    .text_size(px(10.5))
                    .text_color(rgb(t.text))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.set_filter(Filter::All, cx)))
                    .child(trace::ellipsize(&access.action, 34))
                    .child(div().text_color(rgb(t.text_muted)).child("×")),
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
                    .text_color(rgb(t.text_muted))
                    .cursor_pointer()
                    .hover(move |style| {
                        style
                            .bg(theme::h(t.text, 0.05))
                            .text_color(rgb(t.text_soft))
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_all_services = !this.show_all_services;
                        cx.notify();
                    }))
                    .child(if self.show_all_services {
                        i18n::pick("Less", "收起").to_string()
                    } else if i18n::is_zh() {
                        format!("更多 +{hidden}")
                    } else {
                        format!("More +{hidden}")
                    }),
            );
        }

        let (state_dot, state_label, state_color) = if self.demo {
            (
                theme::h(t.amber, 0.95),
                if self.paused {
                    i18n::pick("PAUSED · DEMO", "已暂停 · DEMO").to_string()
                } else {
                    i18n::pick("DEMO DATA", "演示数据").to_string()
                },
                rgb(t.amber),
            )
        } else if self.paused {
            (
                theme::h(t.amber, 0.95),
                if self.buffered.is_empty() {
                    i18n::pick("PAUSED", "已暂停").to_string()
                } else if i18n::is_zh() {
                    format!("已暂停 · 缓存 {}", self.buffered.len())
                } else {
                    format!("PAUSED · {} BUFFERED", self.buffered.len())
                },
                rgb(t.amber),
            )
        } else {
            match &self.health {
                TraceHealth::Ok => match self.run.as_ref().filter(|run| run.ended()) {
                    Some(run) => (
                        theme::h(t.text_faint, 0.9),
                        match run.exit_code() {
                            Some(code) => {
                                if i18n::is_zh() {
                                    format!("已结束 · EXIT {code}")
                                } else {
                                    format!("RUN ENDED · EXIT {code}")
                                }
                            }
                            None => i18n::pick("PROXY STOPPED", "代理已停止").to_string(),
                        },
                        rgb(t.text_muted),
                    ),
                    None => (
                        theme::h(t.green, 0.95),
                        i18n::pick("LIVE", "实时").to_string(),
                        rgb(t.text_muted),
                    ),
                },
                TraceHealth::Missing => (
                    theme::h(t.amber, 0.95),
                    i18n::pick("WAITING FOR A RUN", "等待运行").to_string(),
                    rgb(t.amber),
                ),
                TraceHealth::Error(_) => (
                    theme::h(t.red, 0.95),
                    i18n::pick("TRACE ERROR", "TRACE 异常").to_string(),
                    rgb(t.red),
                ),
            }
        };

        div()
            .flex()
            .items_center()
            .h(px(theme::TOOLBAR_H))
            .px(px(14.0))
            .gap(px(10.0))
            .bg(rgb(t.toolbar))
            .border_b_1()
            .border_color(rgb(t.border_subtle))
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
        let t = theme::p();
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

    #[allow(clippy::too_many_arguments)]
    pub(super) fn chip(
        &self,
        id: &'static str,
        label: &'static str,
        count: usize,
        dot: Option<u32>,
        filter: Filter,
        active: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = theme::p();
        let error_color = dot == Some(t.red);
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
                rgb(t.border_strong)
            } else {
                rgba(0x00000000)
            })
            .bg(if active {
                theme::h(t.text, 0.06)
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
                rgb(t.text)
            } else {
                rgb(t.text_muted)
            })
            .cursor_pointer()
            .hover(move |style| {
                style
                    .bg(theme::h(t.text, 0.05))
                    .text_color(rgb(t.text_soft))
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
                    rgb(t.red)
                } else {
                    rgb(t.text_faint)
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
        let t = theme::p();
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
                theme::h(t.text, 0.06)
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
                rgb(t.text)
            } else {
                rgb(t.text_muted)
            })
            .cursor_pointer()
            .hover(move |style| {
                style
                    .bg(theme::h(t.text, 0.05))
                    .text_color(rgb(t.text_soft))
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
                    .text_color(rgb(t.text_faint))
                    .child(facet.count.to_string()),
            )
    }

    pub(super) fn render_footer(&self, compact: bool) -> Div {
        let t = theme::p();
        let center = if let Some((message, _)) = self.toast.as_ref() {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(glyph::dot(theme::h(t.green, 0.95), 4.5))
                .child(
                    div()
                        .font_family(fonts::mono())
                        .text_size(px(10.5))
                        .text_color(rgb(t.green))
                        .child(message.clone()),
                )
                .into_any_element()
        } else {
            let mut summary = format!(
                "{} · {} · {}",
                i18n::call_count(self.counts.total),
                i18n::error_count(self.counts.errors),
                i18n::service_count(self.counts.services),
            );
            if self.counts.blocked > 0 {
                summary.push_str(&format!(
                    " · {} {}",
                    self.counts.blocked,
                    i18n::pick("blocked", "已拦截")
                ));
            }
            div()
                .font_family(fonts::mono())
                .text_size(px(10.5))
                .text_color(rgb(t.text_muted))
                .child(summary)
                .into_any_element()
        };

        let mut footer = div()
            .flex()
            .items_center()
            .h(px(theme::FOOTER_H))
            .px(px(14.0))
            .gap(px(14.0))
            .bg(rgb(t.footer))
            .border_t_1()
            .border_color(rgb(t.border_subtle))
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
                    .text_color(rgb(t.text_faint))
                    .child(concat!("lenslet ", env!("CARGO_PKG_VERSION"))),
            );

        footer = footer.child(
            div()
                .font_family(fonts::mono())
                .text_size(px(10.0))
                .text_color(rgb(t.text_faint))
                .child(if compact {
                    i18n::pick(
                        "a access · c curl · t theme",
                        "a 权限 · c curl · t 主题 · l 语言",
                    )
                } else {
                    i18n::pick(
                        "a access · c curl · t theme · l 中文 · space pause · 1 2 3 tabs",
                        "a 权限 · c curl · t 主题 · l EN · space 暂停 · 1 2 3 切换",
                    )
                }),
        );
        footer
    }

    /// `‹ 3/7 ›  $ npm test  exit 1  +2 access  1 new failure`: the run at a glance.
    fn render_run_summary(&self, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
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
                                rgb(t.amber)
                            } else {
                                rgb(t.text_muted)
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
                    .text_color(rgb(t.text_muted))
                    .child(i18n::pick("local AWS lens", "本地 AWS 观测镜")),
            );
        };
        let command = if run.header.command.is_empty() {
            i18n::pick("proxy session", "代理会话").to_string()
        } else {
            format!("$ {}", trace::ellipsize(&run.command_line(), 36))
        };
        line = line.child(
            div()
                .font_family(fonts::mono())
                .text_size(px(11.5))
                .text_color(rgb(t.text_soft))
                .whitespace_nowrap()
                .child(command),
        );
        if run.header.enforce {
            line = line.child(badge("ENFORCE", t.purple));
        }
        match run.exit_code() {
            Some(0) => line = line.child(badge("exit 0", t.green)),
            Some(code) => line = line.child(badge(format!("exit {code}"), t.red)),
            None if !run.ended() => {
                line = line.child(badge(i18n::pick("running", "运行中"), t.accent))
            }
            None => {}
        }
        if self.counts.blocked > 0 {
            line = line.child(badge(format!("{} blocked", self.counts.blocked), t.red));
        }
        let added = match run.report.as_ref() {
            Some(report) => report.access_added.len(),
            None => self.access_rows().iter().filter(|row| row.new).count(),
        };
        if added > 0 {
            line = line.child(badge(format!("+{added} access"), t.amber));
        }
        let unused = run
            .report
            .as_ref()
            .map(|report| report.access_unused.len())
            .unwrap_or(0);
        if unused > 0 && run.exit_code() == Some(3) {
            line = line.child(badge(format!("−{unused} unused"), t.amber));
        }
        if let Some(regressions) = run
            .report
            .as_ref()
            .and_then(|report| report.regressions.as_ref())
        {
            if !regressions.new_failures.is_empty() {
                let count = regressions.new_failures.len();
                line = line.child(badge(
                    if i18n::is_zh() {
                        format!("{count} 新增异常")
                    } else {
                        format!(
                            "{count} new {}",
                            if count == 1 { "failure" } else { "failures" }
                        )
                    },
                    t.red,
                ));
            } else if added == 0 && !regressions.new_access.is_empty() {
                line = line.child(badge(
                    format!("+{} vs prev", regressions.new_access.len()),
                    t.amber,
                ));
            }
        }
        line
    }
}

fn header_pill(id: &'static str, label: &'static str) -> Stateful<Div> {
    let t = theme::p();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(26.0))
        .px(px(8.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(t.border))
        .bg(theme::h(t.text, 0.02))
        .text_size(px(11.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(t.text_dim))
        .cursor_pointer()
        .hover(move |style| style.bg(theme::h(t.text, 0.06)).text_color(rgb(t.text)))
        .child(label)
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
    let t = theme::p();
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
            rgb(t.text_dim)
        } else {
            rgb(t.text_faint)
        })
        .child(label);
    if enabled {
        button
            .cursor_pointer()
            .hover(move |style| style.bg(theme::h(t.text, 0.06)).text_color(rgb(t.text)))
    } else {
        button
    }
}
