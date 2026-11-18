//! The access table: every permission the run's calls needed, one row each.
//! Click a row to see the calls behind it.

use super::*;

/// Widths of the access table's fixed columns.
const COL_CALLS: f32 = 56.0;
const COL_FLAGS: f32 = 126.0;

impl LensletApp {
    pub(super) fn render_access(&mut self, cx: &mut Context<Self>) -> Div {
        let t = theme::p();
        let rows = self.access_rows();
        let gate = self.run.as_ref().and_then(|run| {
            run.header
                .policy
                .clone()
                .or_else(|| run.header.lock.clone())
        });
        let new_count = rows.iter().filter(|row| row.new).count();
        let unused_count = rows.iter().filter(|row| row.unused).count();
        let unmapped: usize = self
            .run
            .as_ref()
            .and_then(|run| run.report.as_ref())
            .map(|report| report.unmapped.values().sum())
            .unwrap_or(0);
        let mut note = if i18n::is_zh() {
            match (&gate, new_count, unused_count) {
                (None, _, _) => "当前运行目录没有 lenslet.lock。执行 `lenslet run --update -- <tests>` 可生成锁文件；后续新增权限会在此高亮，并在 `--check` / `--enforce` 下触发检查。".to_string(),
                (Some(gate), 0, 0) => format!("本次运行使用的全部权限均在 {gate} 范围内。"),
                (Some(gate), new, 0) => format!(
                    "有 {new} 项权限不在 {gate} 中：请审阅变更，确认无误后执行 `lenslet run --update` 更新。"
                ),
                (Some(gate), 0, unused) => format!(
                    "所有权限均在 {gate} 范围内，另有 {unused} 项已锁定权限本次未触发（`--strict` 会对此报错）。"
                ),
                (Some(gate), new, unused) => format!(
                    "有 {new} 项不在 {gate} 中，另有 {unused} 项本次未触发：请审阅后执行 `lenslet run --update`。"
                ),
            }
        } else {
            match (&gate, new_count, unused_count) {
                (None, _, _) => "No lenslet.lock in this run's directory. `lenslet run --update -- <tests>` writes one; then new access is flagged here and fails `--check` / `--enforce`.".to_string(),
                (Some(gate), 0, 0) => format!("Everything this run used is in {gate}."),
                (Some(gate), new, 0) => format!(
                    "{new} {} not in {gate}: review them, then `lenslet run --update` to accept.",
                    if new == 1 { "permission is" } else { "permissions are" }
                ),
                (Some(gate), 0, unused) => format!(
                    "Within {gate}, with {unused} locked {} not exercised by this run (`--strict` fails on unused access).",
                    if unused == 1 { "permission" } else { "permissions" }
                ),
                (Some(gate), new, unused) => format!(
                    "{new} not in {gate}, and {unused} in {gate} not exercised by this run: review changes, then `lenslet run --update`.",
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

        let body: AnyElement = if rows.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.0))
                .text_size(px(12.0))
                .text_color(rgb(t.text_muted))
                .child(i18n::pick(
                    "No call in this run maps to an IAM action yet.",
                    "本次运行暂无可映射至 IAM Action 的调用。",
                ))
                .into_any_element()
        } else {
            let this = cx.entity().downgrade();
            let rows = Rc::new(rows);
            uniform_list("access-rows", rows.len(), move |range, _window, _cx| {
                range
                    .map(|index| access_row(&rows[index], index, this.clone()))
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
                    .py(px(9.0))
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
}

fn access_row(
    entry: &trace::AccessEntry,
    index: usize,
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
    div()
        .id(ElementId::named_usize("access-row", index))
        .w_full()
        .flex()
        .items_center()
        .h(px(38.0))
        .px(px(theme::ROW_PAD_X))
        .gap(px(theme::COL_GAP))
        .border_b_1()
        .border_color(rgb(t.border_subtle))
        .bg(if entry.new {
            theme::h(t.amber, 0.055)
        } else if unused {
            theme::h(t.text_faint, 0.05)
        } else {
            theme::h(t.row, 1.0)
        })
        .cursor_pointer()
        .hover(move |style| style.bg(rgb(t.row_hover)))
        .on_click(move |_, _, cx| {
            let access = access.clone();
            this.update(cx, |app, cx| {
                if unused {
                    let msg = if i18n::is_zh() {
                        format!("{} 在本次运行中未被触发", access.action)
                    } else {
                        format!("{} was not exercised in this run", access.action)
                    };
                    app.show_toast(msg, cx);
                } else {
                    app.show_access(access, cx);
                }
            })
            .ok();
        })
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
        .child(flags)
}

fn unused_pill(text: impl Into<SharedString>) -> Div {
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
