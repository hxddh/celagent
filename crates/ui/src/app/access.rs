//! The IAM Access table and its linked Permission Inspector.

use super::*;

const COL_CALLS: f32 = 56.0;
const COL_FLAGS: f32 = 132.0;

impl LensletApp {
    pub(super) fn filtered_access_rows(&self) -> Vec<trace::AccessEntry> {
        let tokens = search_tokens(&self.query);
        self.access_rows()
            .into_iter()
            .filter(|e| self.access_filter.matches(e) && e.matches_search(&tokens))
            .collect()
    }

    pub(super) fn render_access(&mut self, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let all_rows = self.access_rows();
        let rows = self.filtered_access_rows();
        let gate = self.run.as_ref().and_then(|run| {
            run.header
                .policy
                .clone()
                .or_else(|| run.header.lock.clone())
        });
        let new_count = all_rows.iter().filter(|row| row.new).count();
        let unused_count = all_rows.iter().filter(|row| row.unused).count();
        let unmapped: usize = self
            .run
            .as_ref()
            .and_then(|run| run.report.as_ref())
            .map(|report| report.unmapped.values().sum())
            .unwrap_or(0);

        let mut note = if i18n::is_zh() {
            match (&gate, new_count, unused_count) {
                (None, _, _) => "当前运行未加载 lenslet.lock。执行 `lenslet run --update -- <tests>` 可生成锁文件；也可点击右上角直接复制本次观察到的最小 IAM Policy JSON。".to_string(),
                (Some(gate), 0, 0) => format!("本次运行使用的全部权限均在 {gate} 范围内。"),
                (Some(gate), new, 0) => format!(
                    "有 {new} 项权限不在 {gate} 中：点击行可在右侧查看策略片段与触发请求，确认无误后执行 `lenslet run --update`。"
                ),
                (Some(gate), 0, unused) => format!(
                    "所有权限均在 {gate} 范围内，另有 {unused} 项已锁定权限本次未触发（`--strict` 会对此报错）。"
                ),
                (Some(gate), new, unused) => format!(
                    "有 {new} 项不在 {gate} 中，另有 {unused} 项本次未触发：点击行可在右侧审查具体来源请求。"
                ),
            }
        } else {
            match (&gate, new_count, unused_count) {
                (None, _, _) => "No lenslet.lock loaded. Run `lenslet run --update -- <tests>` to write one, or click Copy Policy JSON above.".to_string(),
                (Some(gate), 0, 0) => format!("Everything this run used is within {gate}."),
                (Some(gate), new, 0) => format!(
                    "{new} {} not in {gate}: select a row to inspect its IAM statement and triggering calls.",
                    if new == 1 { "permission is" } else { "permissions are" }
                ),
                (Some(gate), 0, unused) => format!(
                    "Within {gate}, with {unused} locked {} not exercised by this run (`--strict` fails on unused access).",
                    if unused == 1 { "permission" } else { "permissions" }
                ),
                (Some(gate), new, unused) => format!(
                    "{new} not in {gate}, and {unused} in {gate} not exercised by this run.",
                ),
            }
        };
        if unmapped > 0 {
            if i18n::is_zh() {
                note.push_str(&format!(
                    " · 另有 {unmapped} 次调用无法识别具体 AWS Operation。"
                ));
            } else {
                note.push_str(&format!(
                    " · {unmapped} {} had no recognizable operation.",
                    if unmapped == 1 { "call" } else { "calls" }
                ));
            }
        }

        let header = div()
            .flex()
            .items_center()
            .h(px(theme::TABLE_HEAD_H))
            .px(px(theme::ROW_PAD_X))
            .gap(px(theme::COL_GAP))
            .bg(rgb(t.table_head))
            .border_b_1()
            .border_color(rgb(t.border_subtle))
            .child(
                div()
                    .w(px(230.0))
                    .flex_shrink_0()
                    .child(micro_label("ACTION")),
            )
            .child(div().flex_1().min_w(px(0.0)).child(micro_label("RESOURCE")))
            .child(
                div()
                    .w(px(COL_CALLS))
                    .text_right()
                    .child(micro_label(i18n::pick("CALLS", "次数"))),
            )
            .child(div().w(px(COL_FLAGS)));

        let selected_access = self.selected_access.clone();
        let body: AnyElement = if rows.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.0))
                .text_size(px(12.0))
                .text_color(rgb(t.text_muted))
                .child(if all_rows.is_empty() {
                    i18n::pick(
                        "No call in this run maps to an IAM action yet.",
                        "本次运行暂无可映射至 IAM Action 的调用。",
                    )
                } else {
                    i18n::pick(
                        "No permissions match the current filter.",
                        "没有匹配当前筛选条件的 IAM 权限。",
                    )
                })
                .into_any_element()
        } else {
            let this = cx.entity().downgrade();
            let rows = Rc::new(rows);
            uniform_list("access-rows", rows.len(), move |range, _window, _cx| {
                range
                    .map(|index| {
                        let entry = &rows[index];
                        let is_selected = selected_access.as_ref().is_some_and(|sel| {
                            sel.action == entry.action && sel.resource == entry.resource
                        });
                        access_row(entry, index, is_selected, this.clone())
                    })
                    .collect()
            })
            .w_full()
            .flex_1()
            .min_h(px(0.0))
            .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .child(
                div()
                    .px(px(theme::ROW_PAD_X))
                    .py(px(8.0))
                    .border_b_1()
                    .border_color(rgb(t.border_subtle))
                    .text_size(px(11.5))
                    .line_height(px(16.0))
                    .text_color(rgb(t.text_dim))
                    .child(note),
            )
            .child(header)
            .child(body)
    }

    /// Right-hand Inspector when `Pane::Access` is active: shows the selected
    /// permission's IAM statement JSON, lock status, and triggering calls.
    pub(super) fn render_access_inspector(&self, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let all = self.access_rows();
        let entry = self
            .selected_access
            .as_ref()
            .and_then(|sel| {
                all.iter()
                    .find(|e| e.action == sel.action && e.resource == sel.resource)
            })
            .or_else(|| all.first());

        let Some(entry) = entry else {
            return div()
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .p(px(24.0))
                .child(glyph::lens_mark(28.0))
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(rgb(t.text_muted))
                        .child(i18n::pick("Select a permission", "选择一项 IAM 权限")),
                );
        };

        let (svc, act) = entry
            .action
            .split_once(':')
            .unwrap_or(("", entry.action.as_str()));
        let svc_color = theme::service_color(&trace::service_label(svc));
        let key = trace::Access {
            action: entry.action.clone(),
            resource: entry.resource.clone(),
        };
        let stmt_json = trace::statement_json(entry);
        let stmt_copy = stmt_json.clone();
        let lock_line = format!("{:<30} {}", entry.action, entry.resource);

        let matching_calls: Vec<(usize, Arc<Trace>)> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| !row.pending && row.uses(&key))
            .take(40)
            .map(|(i, row)| (i, row.clone()))
            .collect();

        let filter_key = key.clone();
        let mut head_badges = div().flex().items_center().gap(px(6.0));
        head_badges = head_badges.child(
            div()
                .flex()
                .items_center()
                .gap(px(5.0))
                .h(px(18.0))
                .px(px(6.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(theme::tint(svc_color, 0.35))
                .child(glyph::dot(svc_color, 4.5))
                .child(
                    div()
                        .text_size(px(10.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(svc_color)
                        .child(trace::service_label(svc)),
                ),
        );
        if entry.new {
            head_badges = head_badges.child(new_pill("NOT IN LOCK / POLICY"));
        } else if entry.unused {
            head_badges = head_badges.child(unused_pill("UNUSED"));
        } else {
            head_badges = head_badges.child(badge("ALLOWED", t.green));
        }
        if entry.implied {
            head_badges = head_badges.child(badge("IMPLIED", t.accent));
        }
        if entry.test {
            head_badges = head_badges.child(badge("# test", t.text_muted));
        }

        let header = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .px(px(14.0))
            .py(px(12.0))
            .border_b_1()
            .border_color(rgb(t.border_subtle))
            .child(head_badges)
            .child(
                div()
                    .font_family(fonts::mono())
                    .text_size(px(15.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(t.text))
                    .child(act.to_string()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
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
                            .child(entry.resource.clone()),
                    )
                    .child(self.copy_button(
                        "copy-acc-arn",
                        "ARN",
                        Some(entry.resource.clone()),
                        cx,
                    ))
                    .when(!entry.unused, |row| {
                        row.child(
                            action_button(
                                "jump-acc-calls",
                                i18n::pick("Filter Calls →", "筛选相关请求 →"),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.filter_calls_by_access(filter_key.clone(), cx);
                                },
                            )),
                        )
                    }),
            );

        let mut call_list = div().flex().flex_col();
        if matching_calls.is_empty() {
            call_list = call_list.child(empty_note(if entry.unused {
                i18n::pick(
                    "Not exercised by any call in this run.",
                    "本次运行没有任何调用触发该权限。",
                )
            } else {
                i18n::pick("Recorded in the run summary.", "已记录在运行汇总报告中。")
            }));
        } else {
            for (raw_idx, call) in matching_calls {
                call_list = call_list.child(
                    div()
                        .id(ElementId::named_usize("acc-call-jump", raw_idx))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.0))
                        .py(px(6.0))
                        .px(px(8.0))
                        .border_b_1()
                        .border_color(rgb(t.border_subtle))
                        .cursor_pointer()
                        .hover(move |s| s.bg(rgb(t.row_hover)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.pane = Pane::Calls;
                            this.call_filter = trace::CallFilter::default();
                            this.refresh_view();
                            this.select_row(raw_idx, window, cx);
                            this.reveal_selected();
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .min_w(px(0.0))
                                .flex_1()
                                .child(glyph::dot(status_color(call.status), 4.5))
                                .child(
                                    div()
                                        .font_family(fonts::mono())
                                        .text_size(px(11.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(rgb(t.text))
                                        .child(format!(
                                            "{} {}",
                                            call.service_name(),
                                            operation_label(&call)
                                        )),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .font_family(fonts::mono())
                                        .text_size(px(10.5))
                                        .text_color(rgb(t.text_muted))
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .overflow_hidden()
                                        .child(trace::display_path(&call).to_string()),
                                ),
                        )
                        .child(
                            div()
                                .font_family(fonts::mono())
                                .text_size(px(10.5))
                                .text_color(rgb(t.text_dim))
                                .child(format_latency(call.ms)),
                        ),
                );
            }
        }

        let body = div()
            .id("access-inspector-scroll")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .scrollbar_width(px(6.0))
            .p(px(14.0))
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(section_with_action(
                "IAM STATEMENT JSON",
                None,
                Some(
                    action_button("copy-stmt-json", i18n::pick("Copy JSON", "复制 JSON"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.copy_text("IAM statement", stmt_copy.clone(), cx);
                        }))
                        .into_any_element(),
                ),
                code_block("acc-stmt-code", stmt_json, 180.0, self.prefs.word_wrap),
            ))
            .child(section_with_action(
                "LENSLET.LOCK ENTRY",
                None,
                Some(self.copy_button("copy-lock-line", "lock line", Some(lock_line.clone()), cx)),
                code_block("acc-lock-code", lock_line, 64.0, self.prefs.word_wrap),
            ))
            .child(section(
                i18n::pick("TRIGGERING CALLS", "触发该权限的请求"),
                call_list,
            ));

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .child(header)
            .child(body)
    }
}

fn access_row(
    entry: &trace::AccessEntry,
    index: usize,
    selected: bool,
    this: WeakEntity<LensletApp>,
) -> Stateful<Div> {
    let t = theme::p();
    let access = trace::Access {
        action: entry.action.clone(),
        resource: entry.resource.clone(),
    };
    let (service, action) = entry
        .action
        .split_once(':')
        .unwrap_or(("", entry.action.as_str()));
    let color = theme::service_color(&trace::service_label(service));
    let unused = entry.unused;
    let mut flags = div()
        .w(px(COL_FLAGS))
        .flex()
        .items_center()
        .justify_end()
        .gap(px(5.0));
    if entry.new {
        flags = flags.child(new_pill("NEW"));
    }
    if unused {
        flags = flags.child(unused_pill("UNUSED"));
    }
    if entry.test {
        flags = flags.child(
            div()
                .font_family(fonts::mono())
                .text_size(px(10.0))
                .text_color(rgb(t.text_faint))
                .child("test"),
        );
    }
    if entry.implied {
        flags = flags.child(
            div()
                .font_family(fonts::mono())
                .text_size(px(10.0))
                .text_color(rgb(t.text_muted))
                .child("implied"),
        );
    }
    if entry.errors > 0 {
        flags = flags.child(glyph::dot(rgb(t.red), 4.5));
    }
    let row = div()
        .id(ElementId::named_usize("access-row", index))
        .relative()
        .w_full()
        .flex()
        .items_center()
        .h(px(38.0))
        .px(px(theme::ROW_PAD_X))
        .gap(px(theme::COL_GAP))
        .border_b_1()
        .border_color(rgb(t.border_subtle))
        .bg(if selected {
            theme::h(t.row_selected, 1.0)
        } else if entry.new {
            theme::h(t.amber, 0.055)
        } else if unused {
            theme::h(t.text_faint, 0.04)
        } else {
            theme::h(t.row, 1.0)
        })
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            let access = access.clone();
            this.update(cx, |app, cx| {
                app.selected_access = Some(access);
                if !app.prefs.inspector_open {
                    app.prefs.inspector_open = true;
                }
                cx.notify();
            })
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
                } else {
                    rgba(0x0000_0000)
                }),
        )
        .child(
            div()
                .w(px(230.0))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(6.0))
                .overflow_hidden()
                .child(glyph::dot(color, 4.5))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .font_family(fonts::mono())
                        .text_size(px(11.5))
                        .whitespace_nowrap()
                        .child(
                            div()
                                .text_color(rgb(t.text_muted))
                                .child(format!("{service}:")),
                        )
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if unused { rgb(t.text_dim) } else { rgb(t.text) })
                                .child(action.to_string()),
                        ),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .font_family(fonts::mono())
                .text_size(px(11.0))
                .text_color(if unused {
                    rgb(t.text_faint)
                } else {
                    rgb(t.text_dim)
                })
                .whitespace_nowrap()
                .text_ellipsis()
                .overflow_hidden()
                .child(entry.resource.clone()),
        )
        .child(
            div()
                .w(px(COL_CALLS))
                .flex_shrink_0()
                .text_right()
                .font_family(fonts::mono())
                .text_size(px(11.5))
                .text_color(if unused {
                    rgb(t.text_faint)
                } else {
                    rgb(t.text_body)
                })
                .child(entry.calls.to_string()),
        )
        .child(flags);

    if selected {
        row.hover(move |s| s.bg(rgb(t.row_selected_hover)))
    } else {
        row.hover(move |s| s.bg(rgb(t.row_hover)))
    }
}

pub(super) fn unused_pill(text: impl Into<SharedString>) -> Div {
    let t = theme::p();
    div()
        .h(px(15.0))
        .px(px(4.0))
        .flex()
        .items_center()
        .rounded(px(3.0))
        .border_1()
        .border_color(rgb(t.border_strong))
        .bg(theme::h(t.text, 0.04))
        .font_family(fonts::mono())
        .text_size(px(9.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(t.text_dim))
        .child(text.into())
}
