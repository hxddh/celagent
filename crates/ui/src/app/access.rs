//! The access table: every permission the run's calls needed, one row each.
//! Click a row to see the calls behind it.

use super::*;

/// Widths of the access table's fixed columns.
const COL_CALLS: f32 = 56.0;
const COL_FLAGS: f32 = 92.0;

impl LensletApp {
    pub(super) fn render_access(&mut self, cx: &mut Context<Self>) -> Div {
        let rows = self.access_rows();
        let lock = self.run.as_ref().and_then(|run| run.header.lock.clone());
        let note = match (&lock, rows.iter().filter(|row| row.new).count()) {
            (None, _) => "No lenslet.lock in this run's directory. `lenslet run --update -- <tests>` writes one; then new access is flagged here and fails `--check`.".to_string(),
            (Some(lock), 0) => format!("Everything this run used is in {lock}."),
            (Some(lock), new) => format!(
                "{new} {} not in {lock}: review them, then `lenslet run --update` to accept.",
                if new == 1 { "permission is" } else { "permissions are" }
            ),
        };
        let header = div()
            .flex()
            .items_center()
            .h(px(theme::TABLE_HEAD_H))
            .px(px(theme::ROW_PAD_X))
            .gap(px(theme::COL_GAP))
            .bg(rgb(0x0C0F14))
            .border_b_1()
            .border_color(rgb(theme::BORDER_SUBTLE))
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
                    .child(micro_label("CALLS")),
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
                .text_color(rgb(theme::TEXT_MUTED))
                .child("No call in this run maps to an IAM action yet.")
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
                    .border_color(rgb(theme::BORDER_SUBTLE))
                    .text_size(px(11.5))
                    .line_height(px(16.0))
                    .text_color(rgb(theme::TEXT_DIM))
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
    let access = trace::Access {
        action: entry.action.clone(),
        resource: entry.resource.clone(),
    };
    let (service, action) = entry
        .action
        .split_once(':')
        .unwrap_or(("", entry.action.as_str()));
    let color = theme::service_color(&trace::service_label(service));
    let mut flags = div()
        .w(px(COL_FLAGS))
        .flex()
        .items_center()
        .justify_end()
        .gap(px(5.0));
    if entry.new {
        flags = flags.child(new_pill("NEW"));
    }
    if entry.implied {
        flags = flags.child(
            div()
                .font_family(fonts::mono())
                .text_size(px(10.0))
                .text_color(rgb(theme::TEXT_MUTED))
                .child("implied"),
        );
    }
    if entry.errors > 0 {
        flags = flags.child(glyph::dot(rgb(theme::RED), 4.5));
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
        .border_color(rgb(theme::BORDER_SUBTLE))
        .bg(if entry.new {
            theme::h(theme::AMBER, 0.045)
        } else {
            theme::h(theme::ROW, 1.0)
        })
        .cursor_pointer()
        .hover(|style| style.bg(rgb(theme::ROW_HOVER)))
        .on_click(move |_, _, cx| {
            let access = access.clone();
            this.update(cx, |app, cx| app.show_access(access, cx)).ok();
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
                                .text_color(rgb(theme::TEXT_MUTED))
                                .child(format!("{service}:")),
                        )
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(theme::TEXT))
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
                .text_color(rgb(theme::TEXT_DIM))
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
                .text_color(rgb(theme::TEXT_BODY))
                .child(entry.calls.to_string()),
        )
        .child(flags)
}
