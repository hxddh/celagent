//! The call inspector: what the call was, why it failed, and its payloads.
//! Three tabs — Exchange (diagnosis, replay, retries, body & unified diff view
//! with Decoded/Raw toggle + search match stepper), Headers (structured SigV4 &
//! query/header table or raw wire text), and Access.

use std::{collections::HashMap, sync::Arc};

use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Div, ElementId, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, Render, Stateful, Task, Window, div, prelude::*, px, rgb,
};

use super::{
    bodyview::{BodyView, BodyViewEvent},
    session::Session,
    widgets,
};
use crate::{
    body::{self, BodyDoc, BodyMode, DocCache, DocKey},
    fonts, glyph, i18n,
    model::{
        Access, Body, Exchange, Header, Trace, decode_aws_semantic, decode_sigv4_header,
        decoded_query_params, display_path, format_bytes, format_latency, full_time,
        operation_label, pretty_body, short_clock, status_label, status_text, to_curl,
    },
    replay::{self, ReplayOutcome, ReplayState, ReplayTarget},
    syntax::SyntaxKind,
    theme::{self, Ui, text},
};

/// Bodies up to this size are built synchronously (no loading flash).
const SYNC_BUILD_BYTES: usize = 48 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Exchange,
    Headers,
    Access,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Request,
    Response,
    Replay,
    Diff,
}

pub enum InspectorEvent {
    /// Show this permission in the Access view.
    OpenAccess(Access),
    /// Select another call (a retry attempt).
    Select(String),
    /// Open the interactive Request Composer for this call & target.
    OpenComposer(Arc<Trace>, ReplayTarget),
    Toast(String),
}

pub struct Inspector {
    session: Entity<Session>,
    call: Option<Arc<Trace>>,
    full: Option<Arc<Trace>>,
    hydrating: bool,
    generation: u64,
    pub tab: Tab,
    side: Side,
    mode: BodyMode,
    headers_raw: bool,
    tokens: Vec<String>,
    body: Entity<BodyView>,
    docs: DocCache,
    replays: HashMap<String, ReplayState>,
    peers: Vec<Arc<Trace>>,
    focus: FocusHandle,
    home: std::path::PathBuf,
    _build: Option<Task<()>>,
    _subscription: gpui::Subscription,
}

impl EventEmitter<InspectorEvent> for Inspector {}

impl Focusable for Inspector {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Inspector {
    pub fn new(session: Entity<Session>, home: std::path::PathBuf, cx: &mut Context<Self>) -> Self {
        let body = cx.new(|cx| BodyView::new("body", cx));
        let subscription = cx.subscribe(&body, |_, _, event, cx| match event {
            BodyViewEvent::Copied(bytes) => {
                let lang = Ui::of(cx).lang;
                cx.emit(InspectorEvent::Toast(i18n::copied(
                    &format_bytes(*bytes),
                    lang,
                )));
            }
        });
        Inspector {
            session,
            call: None,
            full: None,
            hydrating: false,
            generation: 0,
            tab: Tab::Exchange,
            side: Side::Response,
            mode: BodyMode::Decoded,
            headers_raw: false,
            tokens: Vec::new(),
            body,
            docs: DocCache::default(),
            replays: HashMap::new(),
            peers: Vec::new(),
            focus: cx.focus_handle(),
            home,
            _build: None,
            _subscription: subscription,
        }
    }

    pub fn body(&self) -> &Entity<BodyView> {
        &self.body
    }

    pub fn call(&self) -> Option<&Arc<Trace>> {
        self.full.as_ref().or(self.call.as_ref())
    }

    /// Show `call`. Hydrates offloaded bodies in the background.
    pub fn set_call(&mut self, call: Option<Arc<Trace>>, cx: &mut Context<Self>) {
        let same = match (&self.call, &call) {
            (Some(a), Some(b)) => a.id == b.id && a.pending == b.pending && a.status == b.status,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        let new_call = self.call.as_ref().map(|c| &c.id) != call.as_ref().map(|c| &c.id);
        self.call = call.clone();
        self.generation += 1;
        if new_call {
            self.side = default_side(call.as_deref());
            if matches!(self.side, Side::Replay | Side::Diff) {
                self.side = Side::Response;
            }
        }
        self.peers = match call.as_deref() {
            Some(call) => self.session.read(cx).store.invocation_peers(call),
            None => Vec::new(),
        };
        match call {
            Some(call) if call.is_dehydrated() => {
                self.full = None;
                self.hydrating = true;
                self.body.update(cx, |body, cx| body.set_loading(cx));
                let generation = self.generation;
                self._build = Some(cx.spawn(async move |this, cx| {
                    let full = cx
                        .background_executor()
                        .spawn(async move { call.hydrated().map(Arc::new).unwrap_or(call) })
                        .await;
                    this.update(cx, |inspector, cx| {
                        if inspector.generation == generation {
                            inspector.hydrating = false;
                            inspector.full = Some(full);
                            inspector.rebuild(cx);
                        }
                    })
                    .ok();
                }));
            }
            Some(call) => {
                self.hydrating = false;
                self.full = Some(call);
                self.rebuild(cx);
            }
            None => {
                self.hydrating = false;
                self.full = None;
                self.rebuild(cx);
            }
        }
        cx.notify();
    }

    pub fn set_tokens(&mut self, tokens: Vec<String>, cx: &mut Context<Self>) {
        if self.tokens != tokens {
            self.tokens = tokens;
            self.rebuild(cx);
        }
    }

    pub fn step_match(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.body.update(cx, |body, cx| body.step_match(delta, cx));
    }

    /// The look changed: cached highlights are stale.
    pub fn restyle(&mut self, cx: &mut Context<Self>) {
        self.rebuild(cx);
    }

    pub fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            self.rebuild(cx);
            cx.notify();
        }
    }

    fn set_side(&mut self, side: Side, cx: &mut Context<Self>) {
        if self.side != side {
            self.side = side;
            self.rebuild(cx);
            cx.notify();
        }
    }

    fn toggle_mode(&mut self, cx: &mut Context<Self>) {
        self.mode = match self.mode {
            BodyMode::Decoded => BodyMode::Raw,
            BodyMode::Raw => BodyMode::Decoded,
        };
        self.rebuild(cx);
        cx.notify();
    }

    /// Build (or fetch from cache) the document the body view shows.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let ui = Ui::of(cx);
        let Some(call) = self.full.clone() else {
            if !self.hydrating {
                self.body.update(cx, |body, cx| {
                    body.set_doc(Arc::new(BodyDoc::default()), cx);
                    body.set_placeholder("", cx);
                });
            }
            return;
        };
        let (side_code, placeholder) = match (self.tab, self.side) {
            (Tab::Headers, _) => (9u8, ui.t("No headers", "没有 Headers")),
            (_, Side::Request) => (0, ui.t("No request body", "没有请求体")),
            (_, Side::Response) if call.pending => {
                (1, ui.t("Waiting for the response…", "等待响应…"))
            }
            (_, Side::Response) => (1, ui.t("No response body", "没有响应体")),
            (_, Side::Replay) => (2, ui.t("Replay has no body", "重放没有响应体")),
            (_, Side::Diff) => (3, ui.t("No diff available", "暂无差异")),
        };
        if self.tab == Tab::Access || (self.tab == Tab::Headers && !self.headers_raw) {
            return;
        }
        let key = DocKey {
            call: call.id.clone(),
            side: side_code,
            mode: self.mode,
            query: self.tokens.join(" "),
            look: (ui.dark(), ui.accent as u8),
        };
        self.body
            .update(cx, |body, cx| body.set_placeholder(placeholder, cx));
        if let Some(doc) = self.docs.get(&key) {
            self.body.update(cx, |body, cx| body.set_doc(doc, cx));
            return;
        }
        let source = DocSource::of(&call, self.tab, self.side, self.replays.get(&call.id));
        let tokens = self.tokens.clone();
        let mode = self.mode;
        let palette = ui.p;
        if source.len() <= SYNC_BUILD_BYTES {
            let doc = Arc::new(source.build(mode, &tokens, &palette));
            self.docs.insert(key, doc.clone());
            self.body.update(cx, |body, cx| body.set_doc(doc, cx));
            return;
        }
        self.body.update(cx, |body, cx| body.set_loading(cx));
        let generation = self.generation;
        self._build = Some(cx.spawn(async move |this, cx| {
            let doc = cx
                .background_executor()
                .spawn(async move { Arc::new(source.build(mode, &tokens, &palette)) })
                .await;
            this.update(cx, |inspector, cx| {
                inspector.docs.insert(key.clone(), doc.clone());
                if inspector.generation == generation {
                    inspector.body.update(cx, |body, cx| body.set_doc(doc, cx));
                }
            })
            .ok();
        }));
    }

    // -- Replay & Composer -----------------------------------------------------

    /// Through the live proxy when the run is live, else straight to the emulator.
    pub fn replay_target(&self, cx: &App) -> Option<ReplayTarget> {
        let session = self.session.read(cx);
        if session.demo {
            return None;
        }
        let header = &session.run.as_ref()?.header;
        if session.is_live() && !header.endpoint.is_empty() {
            return Some(ReplayTarget {
                url: header.endpoint.clone(),
                via_proxy: true,
            });
        }
        (!header.upstream.is_empty()).then(|| ReplayTarget {
            url: header.upstream.clone(),
            via_proxy: false,
        })
    }

    pub fn can_replay(&self, cx: &App) -> bool {
        match (self.full.as_ref(), self.replay_target(cx)) {
            (Some(call), Some(target)) => replay::can_replay(call, Some(&target.url)),
            _ => false,
        }
    }

    pub fn open_composer(&mut self, cx: &mut Context<Self>) {
        if let (Some(call), Some(target)) = (self.full.clone(), self.replay_target(cx))
            && replay::can_replay(&call, Some(&target.url))
        {
            cx.emit(InspectorEvent::OpenComposer(call, target));
        }
    }

    pub fn apply_composed_replay(
        &mut self,
        call_id: String,
        state: ReplayState,
        cx: &mut Context<Self>,
    ) {
        let done = matches!(state, ReplayState::Done(_));
        self.replays.insert(call_id.clone(), state);
        self.tab = Tab::Exchange;
        if done && self.call.as_ref().is_some_and(|c| c.id == call_id) {
            self.side = Side::Replay;
        }
        self.docs.clear();
        self.rebuild(cx);
        cx.notify();
    }

    pub fn replay(&mut self, cx: &mut Context<Self>) {
        let (Some(call), Some(target)) = (self.full.clone(), self.replay_target(cx)) else {
            return;
        };
        if !replay::can_replay(&call, Some(&target.url)) {
            return;
        }
        let id = call.id.clone();
        self.replays.insert(id.clone(), ReplayState::Running);
        self.tab = Tab::Exchange;
        cx.notify();
        let home = self.home.clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { replay::replay_call(&call, &target, &home) })
                .await;
            this.update(cx, |inspector, cx| {
                let state = match outcome {
                    Ok(done) => ReplayState::Done(done),
                    Err(error) => ReplayState::Failed(error),
                };
                inspector.apply_composed_replay(id, state, cx);
            })
            .ok();
        })
        .detach();
    }

    pub fn curl(&self, cx: &App) -> Option<String> {
        let call = self.full.as_ref()?;
        let session = self.session.read(cx);
        to_curl(call, session.header_endpoint())
    }

    // -- Render ------------------------------------------------------------------

    fn render_header(&self, ui: &Ui, call: &Trace, cx: &mut Context<Self>) -> Div {
        let t = ui.p;
        let lang = ui.lang;
        let color = ui.service_color(call.service_name());
        let mut top = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(6.0))
                    .h(px(20.0))
                    .px(px(6.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(theme::tint(color, 0.35))
                    .child(glyph::dot(color, 6.0))
                    .child(
                        div()
                            .text_size(px(text::SM))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(color)
                            .child(call.service_name().to_string()),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_size(px(text::XL))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(t.text))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(operation_label(call).to_string()),
            );
        if self.can_replay(cx) {
            let running = matches!(self.replays.get(&call.id), Some(ReplayState::Running));
            let live = self
                .replay_target(cx)
                .is_some_and(|target| target.via_proxy);
            top = top
                .child(
                    widgets::primary_button(
                        ui,
                        "replay",
                        if running {
                            ui.t("Replaying…", "重放中…")
                        } else {
                            ui.t("Replay", "重放")
                        },
                    )
                    .tooltip(widgets::tip(
                        if live {
                            ui.t(
                                "Send again through the live proxy (recorded in this run)",
                                "通过运行中的代理重新发送（会记录到本次运行）",
                            )
                        } else {
                            ui.t(
                                "Send again to the emulator (shown here, not recorded)",
                                "直接重新发送到模拟器（仅在此显示，不记录）",
                            )
                        },
                        Some(i18n::shortcut("⏎")),
                    ))
                    .on_click(cx.listener(|inspector, _, _, cx| inspector.replay(cx))),
                )
                .child(
                    widgets::button(ui, "edit-replay", ui.t("Edit", "编辑"))
                        .tooltip(widgets::tip(
                            ui.t(
                                "Edit payload/headers & auto re-sign SigV4",
                                "编辑请求参数与报文并自动重签 SigV4",
                            ),
                            Some(i18n::shortcut("⇧⏎")),
                        ))
                        .on_click(cx.listener(|inspector, _, _, cx| inspector.open_composer(cx))),
                );
        }
        if let Some(curl) = self.curl(cx) {
            top = top.child(
                widgets::button(ui, "copy-curl", "curl")
                    .tooltip(widgets::tip(
                        ui.t("Copy as curl", "复制为 curl"),
                        Some(i18n::shortcut("⇧C")),
                    ))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(curl.clone()));
                        cx.emit(InspectorEvent::Toast(i18n::copied("curl", lang)));
                    })),
            );
        }

        let path = call.path.clone();
        let semantic = call.semantic_target();
        let path_row = div()
            .id("inspector-path")
            .flex()
            .items_center()
            .gap(px(8.0))
            .mt(px(6.0))
            .cursor_pointer()
            .tooltip(widgets::tip(
                format!("{} {}", call.method.to_ascii_uppercase(), call.path),
                Some(ui.t("click to copy", "点击复制").into()),
            ))
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
                cx.emit(InspectorEvent::Toast(i18n::copied("path", lang)));
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .font(fonts::mono_font())
                    .text_size(px(text::SM))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(t.accent))
                    .child(call.method.to_ascii_uppercase()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .font(fonts::mono_font())
                    .text_size(px(text::SM))
                    .text_color(rgb(t.text_dim))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(if !semantic.is_empty() {
                        semantic
                    } else {
                        display_path(call).to_string()
                    }),
            );

        let status_tint = if call.pending {
            rgb(t.amber)
        } else {
            ui.status_color(call.status)
        };
        let mut meta = div()
            .flex()
            .items_center()
            .gap(px(12.0))
            .mt(px(6.0))
            .font(fonts::mono_font())
            .text_size(px(text::SM))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(glyph::dot(status_tint, 6.0))
                    .child(div().text_color(status_tint).child(if call.pending {
                        "…".to_string()
                    } else {
                        format!("{} {}", status_label(call.status), status_text(call.status))
                    })),
            )
            .child(
                div()
                    .text_color(if call.pending {
                        rgb(t.amber)
                    } else {
                        ui.latency_color(call.ms)
                    })
                    .child(if call.pending {
                        ui.t("running", "进行中").to_string()
                    } else {
                        format_latency(call.ms)
                    }),
            )
            .child(
                div()
                    .text_color(rgb(t.text_faint))
                    .child(short_clock(&call.at)),
            );
        if call.blocked {
            meta = meta.child(widgets::badge(ui, "BLOCKED", t.purple));
        }
        if let Some(retry) = call.retry_badge() {
            meta = meta.child(widgets::badge(ui, retry, t.accent));
        }
        meta = meta.child(div().flex_1()).child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(glyph::auth_mark(&t, call.authorization || call.presigned()))
                .child(
                    div()
                        .font_family(fonts::sans())
                        .text_color(rgb(t.text_muted))
                        .child(if call.authorization {
                            "SigV4"
                        } else if call.presigned() {
                            "Presigned"
                        } else {
                            "Unsigned"
                        }),
                ),
        );

        div()
            .flex()
            .flex_col()
            .px(px(16.0))
            .pt(px(12.0))
            .child(top)
            .child(path_row)
            .child(meta)
            .child(self.render_tabs(ui, call, cx))
    }

    fn render_tabs(&self, ui: &Ui, call: &Trace, cx: &mut Context<Self>) -> Div {
        let t = ui.p;
        let headers = call.request.as_ref().map_or(0, |r| r.headers.len())
            + call.response.as_ref().map_or(0, |r| r.headers.len());
        let permissions = call.access.len() + call.implied.len();
        let tabs = [
            (Tab::Exchange, ui.t("Exchange", "报文"), None),
            (Tab::Headers, "Headers", Some(headers)),
            (Tab::Access, ui.t("Access", "权限"), Some(permissions)),
        ];
        let mut row = div()
            .flex()
            .items_end()
            .gap(px(16.0))
            .h(px(36.0))
            .mt(px(4.0))
            .border_b_1()
            .border_color(rgb(t.border_subtle));
        for (index, (tab, label, count)) in tabs.into_iter().enumerate() {
            let active = self.tab == tab;
            row = row.child(
                div()
                    .id(ElementId::named_usize("tab", index))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .h(px(32.0))
                    .border_b_2()
                    .border_color(if active {
                        theme::h(t.accent, 1.0)
                    } else {
                        theme::h(t.accent, 0.0)
                    })
                    .text_size(px(text::MD))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if active { rgb(t.text) } else { rgb(t.text_dim) })
                    .cursor_pointer()
                    .hover(move |s| s.text_color(rgb(t.text)))
                    .on_click(cx.listener(move |inspector, _, _, cx| inspector.set_tab(tab, cx)))
                    .child(label)
                    .when_some(count.filter(|n| *n > 0), |el, count| {
                        el.child(
                            div()
                                .font(fonts::mono_font())
                                .text_size(px(text::XS))
                                .text_color(rgb(t.text_muted))
                                .child(count.to_string()),
                        )
                    }),
            );
        }
        row
    }

    fn render_problem(&self, ui: &Ui, call: &Trace) -> Option<Div> {
        let t = ui.p;
        let problem = call.problem.as_ref()?;
        let mut panel = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .p(px(12.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(theme::h(t.red, 0.35))
            .bg(theme::h(t.red, 0.06))
            .child(widgets::label(
                ui,
                if call.blocked {
                    ui.t("BLOCKED BY LENSLET", "已被 LENSLET 拦截")
                } else {
                    ui.t("WHY IT FAILED", "失败原因")
                },
            ))
            .child(
                div()
                    .font(fonts::mono_font())
                    .text_size(px(text::LG))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(t.red))
                    .child(problem.code.clone()),
            );
        if let Some(message) = problem.message.as_deref().filter(|m| !m.is_empty()) {
            panel = panel.child(
                div()
                    .text_size(px(text::MD))
                    .line_height(px(17.0))
                    .text_color(rgb(t.text_soft))
                    .child(message.to_string()),
            );
        }
        if let Some(hint) = problem.hint.as_deref() {
            panel = panel.child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .text_size(px(text::MD))
                    .line_height(px(17.0))
                    .child(div().text_color(rgb(t.accent)).child("→"))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_color(rgb(t.text_body))
                            .child(hint.to_string()),
                    ),
            );
        }
        if let Some(error) = call.error.as_deref() {
            panel = panel.child(widgets::mono(ui, error.to_string()));
        }
        Some(panel)
    }

    fn render_replay(&self, ui: &Ui, call: &Trace, cx: &mut Context<Self>) -> Option<Div> {
        let t = ui.p;
        let state = self.replays.get(&call.id)?;
        let id = call.id.clone();
        let dismiss = widgets::button(ui, "dismiss-replay", ui.t("Dismiss", "关闭")).on_click(
            cx.listener(move |inspector, _, _, cx| {
                inspector.replays.remove(&id);
                if matches!(inspector.side, Side::Replay | Side::Diff) {
                    inspector.side = Side::Response;
                    inspector.rebuild(cx);
                }
                cx.notify();
            }),
        );
        let card = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .p(px(12.0))
            .rounded(px(8.0))
            .border_1();
        Some(match state {
            ReplayState::Running => card
                .border_color(theme::h(t.accent, 0.4))
                .bg(theme::h(t.accent, 0.06))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .text_size(px(text::MD))
                        .text_color(rgb(t.accent))
                        .child(glyph::replay_icon(theme::h(t.accent, 1.0)))
                        .child(ui.t("Replaying…", "重放中…")),
                ),
            ReplayState::Failed(error) => card
                .border_color(theme::h(t.red, 0.4))
                .bg(theme::h(t.red, 0.06))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .child(widgets::label(ui, ui.t("REPLAY FAILED", "重放失败")))
                        .child(div().flex_1())
                        .child(dismiss),
                )
                .child(widgets::mono(ui, error.clone())),
            ReplayState::Done(out) => {
                let ok = out.status < 400;
                let color = if ok { t.green } else { t.red };
                let diff_btn = widgets::button(ui, "show-replay-diff", ui.t("Diff", "对比差异"))
                    .on_click(cx.listener(|inspector, _, _, cx| {
                        inspector.set_side(Side::Diff, cx);
                    }));
                card.border_color(theme::h(color, 0.4))
                    .bg(theme::h(color, 0.05))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(widgets::label(ui, ui.t("REPLAY", "重放结果")))
                            .child(div().flex_1())
                            .child(diff_btn)
                            .child(dismiss),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(8.0))
                            .child(compare_column(
                                ui,
                                ui.t("Recorded", "原始"),
                                call.status,
                                call.ms,
                                call.response
                                    .as_ref()
                                    .and_then(|r| r.body.as_ref())
                                    .map_or(0, Body::size),
                            ))
                            .child(compare_column(
                                ui,
                                ui.t("Replay", "重放"),
                                out.status,
                                out.ms,
                                out.body_bytes,
                            )),
                    )
            }
        })
    }

    fn render_retries(&self, ui: &Ui, call: &Trace, cx: &mut Context<Self>) -> Option<Div> {
        if self.peers.len() < 2 {
            return None;
        }
        let t = ui.p;
        let mut chain = div().flex().flex_wrap().items_center().gap(px(6.0));
        for (index, peer) in self.peers.iter().enumerate() {
            if index > 0 {
                chain = chain.child(
                    div()
                        .text_size(px(text::XS))
                        .text_color(rgb(t.text_faint))
                        .child("→"),
                );
            }
            let current = peer.id == call.id;
            let color = if peer.blocked {
                t.purple
            } else if peer.is_error() {
                t.red
            } else {
                t.green
            };
            let id = peer.id.clone();
            chain = chain.child(
                div()
                    .id(ElementId::named_usize("retry", index))
                    .flex()
                    .items_center()
                    .h(px(24.0))
                    .px(px(8.0))
                    .rounded(px(5.0))
                    .border_1()
                    .border_color(if current {
                        theme::h(t.accent, 1.0)
                    } else {
                        theme::h(color, 0.4)
                    })
                    .bg(theme::h(color, 0.08))
                    .font(fonts::mono_font())
                    .text_size(px(text::SM))
                    .text_color(rgb(color))
                    .cursor_pointer()
                    .hover(move |s| s.bg(theme::h(color, 0.16)))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(InspectorEvent::Select(id.clone()));
                    }))
                    .child(format!(
                        "#{} {} · {}",
                        peer.attempt.unwrap_or(index as u32 + 1),
                        if peer.pending {
                            "…".to_string()
                        } else {
                            status_label(peer.status)
                        },
                        format_latency(peer.ms)
                    )),
            );
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(widgets::label(ui, ui.t("SDK RETRIES", "SDK 重试")))
                .child(chain),
        )
    }

    fn render_exchange(&self, ui: &Ui, call: &Trace, cx: &mut Context<Self>) -> Div {
        let t = ui.p;
        let replay_done = matches!(self.replays.get(&call.id), Some(ReplayState::Done(_)));
        let mut segments = vec![
            widgets::Segment {
                id: "side-req",
                label: ui.t("Request", "请求").into(),
                active: self.side == Side::Request,
                tooltip: None,
            },
            widgets::Segment {
                id: "side-res",
                label: ui.t("Response", "响应").into(),
                active: self.side == Side::Response,
                tooltip: None,
            },
        ];
        if replay_done {
            segments.push(widgets::Segment {
                id: "side-replay",
                label: ui.t("Replay", "重放").into(),
                active: self.side == Side::Replay,
                tooltip: None,
            });
            segments.push(widgets::Segment {
                id: "side-diff",
                label: ui.t("Diff", "差异").into(),
                active: self.side == Side::Diff,
                tooltip: None,
            });
        }
        let entity = cx.entity();
        let sides = widgets::segmented(ui, segments, move |index, _, cx| {
            let side = [Side::Request, Side::Response, Side::Replay, Side::Diff][index];
            entity.update(cx, |inspector, cx| inspector.set_side(side, cx));
        });

        let body_ref = self.body.read(cx);
        let doc = body_ref.doc().clone();
        let match_pos = body_ref.match_status();
        let meta = self.body_meta(call, ui);
        let mut toolbar = div().flex().items_center().gap(px(6.0)).child(sides).child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .font(fonts::mono_font())
                .text_size(px(text::XS))
                .text_color(rgb(t.text_muted))
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(meta),
        );
        if let Some((cur_m, total_m)) = match_pos {
            let body_prev = self.body.clone();
            let body_next = self.body.clone();
            toolbar = toolbar.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(widgets::badge(ui, format!("{cur_m}/{total_m}"), t.accent))
                    .child(
                        widgets::button(ui, "match-prev", "▲")
                            .px(px(5.0))
                            .tooltip(widgets::tip(
                                i18n::hits_count(doc.matches, ui.lang),
                                Some(i18n::shortcut("⇧G")),
                            ))
                            .on_click(move |_, _, cx| {
                                body_prev.update(cx, |b, cx| b.step_match(-1, cx))
                            }),
                    )
                    .child(
                        widgets::button(ui, "match-next", "▼")
                            .px(px(5.0))
                            .tooltip(widgets::tip(
                                i18n::hits_count(doc.matches, ui.lang),
                                Some(i18n::shortcut("G")),
                            ))
                            .on_click(move |_, _, cx| {
                                body_next.update(cx, |b, cx| b.step_match(1, cx))
                            }),
                    ),
            );
        }
        if doc.decodable && self.side != Side::Diff {
            let decoded = self.mode == BodyMode::Decoded;
            let label = match (decoded, doc.decoder) {
                (true, Some(decoder)) => format!("{} · {decoder}", ui.t("Decoded", "已解码")),
                (true, None) => ui.t("Decoded", "已解码").to_string(),
                (false, _) => ui.t("Raw", "原始").to_string(),
            };
            toolbar = toolbar.child(
                widgets::button(ui, "mode", label)
                    .tooltip(widgets::tip(
                        if decoded {
                            ui.t("Show the exact wire text", "显示原始报文")
                        } else {
                            ui.t(
                                "Pretty-print and decode AWS encodings",
                                "格式化并解码 AWS 编码",
                            )
                        },
                        None,
                    ))
                    .on_click(cx.listener(|inspector, _, _, cx| inspector.toggle_mode(cx))),
            );
        }
        if !doc.is_empty() {
            let body = self.body.clone();
            toolbar = toolbar.child(
                widgets::button(ui, "copy-body", ui.t("Copy", "复制"))
                    .tooltip(widgets::tip(
                        ui.t(
                            "Copy the selection, or the whole body",
                            "复制选中字符或整个报文",
                        ),
                        Some(i18n::shortcut("C")),
                    ))
                    .on_click(move |_, _, cx| body.update(cx, |body, cx| body.copy(cx))),
            );
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .gap(px(12.0))
            .p(px(16.0))
            .children(self.render_problem(ui, call))
            .children(self.render_replay(ui, call, cx))
            .children(self.render_retries(ui, call, cx))
            .child(toolbar)
            .child(super::cached(&self.body))
    }

    fn body_meta(&self, call: &Trace, ui: &Ui) -> String {
        if self.side == Side::Diff {
            return ui
                .t(
                    "Unified diff (Recorded → Replay)",
                    "统一差异对比（原始 → 重放）",
                )
                .into();
        }
        let exchange: Option<&Exchange> = match self.side {
            Side::Request => call.request.as_ref(),
            Side::Response => call.response.as_ref(),
            Side::Replay | Side::Diff => None,
        };
        if self.side == Side::Replay {
            if let Some(ReplayState::Done(out)) = self.replays.get(&call.id) {
                return format!(
                    "{} · {} headers · {}",
                    format_bytes(out.body_bytes),
                    out.headers.len(),
                    out.content_type.clone().unwrap_or_default()
                );
            }
            return String::new();
        }
        let Some(exchange) = exchange else {
            return String::new();
        };
        let mut parts = Vec::new();
        if let Some(body) = exchange.body.as_ref() {
            parts.push(format_bytes(body.size()));
            if let Some(encoding) = body.encoding.as_deref() {
                parts.push(encoding.to_string());
            }
            if body.truncated {
                parts.push(ui.t("truncated", "已截断").into());
            }
            if body.binary {
                parts.push(ui.t("binary", "二进制").into());
            }
        }
        if let Some(ct) = exchange.content_type.as_deref() {
            parts.push(ct.to_string());
        }
        parts.join(" · ")
    }

    fn render_headers(&self, ui: &Ui, call: &Trace, cx: &mut Context<Self>) -> Div {
        let t = ui.p;
        let lang = ui.lang;
        let entity = cx.entity();
        let mode_seg = widgets::segmented(
            ui,
            vec![
                widgets::Segment {
                    id: "hdr-table",
                    label: ui.t("Structured", "结构化").into(),
                    active: !self.headers_raw,
                    tooltip: None,
                },
                widgets::Segment {
                    id: "hdr-raw",
                    label: ui.t("Raw", "原始文本").into(),
                    active: self.headers_raw,
                    tooltip: None,
                },
            ],
            move |ix, _, cx| {
                entity.update(cx, |insp, cx| {
                    insp.headers_raw = ix == 1;
                    insp.rebuild(cx);
                    cx.notify();
                });
            },
        );

        let body = self.body.clone();
        let top_bar = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(mode_seg)
            .child(div().flex_1())
            .when(self.headers_raw, |bar| {
                bar.child(
                    widgets::button(ui, "copy-headers", ui.t("Copy", "复制"))
                        .on_click(move |_, _, cx| body.update(cx, |body, cx| body.copy(cx))),
                )
            });

        if self.headers_raw {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .gap(px(12.0))
                .p(px(16.0))
                .child(top_bar)
                .child(super::cached(&self.body));
        }

        let mut scroll = div()
            .id("headers-structured-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .gap(px(14.0));

        // 1. SigV4 Authorization Scope breakdown
        if let Some(auth_val) = call
            .request
            .as_ref()
            .and_then(|r| r.header("authorization"))
            .and_then(decode_sigv4_header)
        {
            let mut sig_box = div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .p(px(10.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(theme::h(t.accent, 0.30))
                .bg(theme::h(t.accent, 0.05))
                .child(widgets::label(
                    ui,
                    ui.t("SIGV4 CREDENTIAL SCOPE", "SIGV4 签名凭证域"),
                ));
            for (k, v) in [
                ("Algorithm", "AWS4-HMAC-SHA256".to_string()),
                ("AccessKeyId", auth_val.access_key_id),
                (
                    "Scope",
                    format!("{}/{}/{}", auth_val.date, auth_val.region, auth_val.service),
                ),
                ("SignedHeaders", auth_val.signed_headers),
            ] {
                sig_box = sig_box.child(widgets::meta_row(ui, k, widgets::mono(ui, v)));
            }
            scroll = scroll.child(sig_box);
        }

        // 2. Decoded Query Parameters
        let query_params = decoded_query_params(&call.path);
        if !query_params.is_empty() {
            scroll = scroll.child(
                self.render_kv_section(
                    ui,
                    ui.t("QUERY PARAMETERS", "URL 查询参数"),
                    "qp",
                    &query_params
                        .into_iter()
                        .map(|(name, value)| Header { name, value })
                        .collect::<Vec<_>>(),
                    lang,
                    cx,
                ),
            );
        }

        // 3. Request & Response Headers
        if let Some(req) = call.request.as_ref()
            && !req.headers.is_empty()
        {
            scroll = scroll.child(self.render_kv_section(
                ui,
                ui.t("REQUEST HEADERS", "请求 Headers"),
                "req-h",
                &req.headers,
                lang,
                cx,
            ));
        }
        if let Some(res) = call.response.as_ref()
            && !res.headers.is_empty()
        {
            scroll = scroll.child(self.render_kv_section(
                ui,
                ui.t("RESPONSE HEADERS", "响应 Headers"),
                "res-h",
                &res.headers,
                lang,
                cx,
            ));
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .gap(px(12.0))
            .p(px(16.0))
            .child(top_bar)
            .child(scroll)
    }

    fn render_kv_section(
        &self,
        ui: &Ui,
        title: &str,
        prefix: &'static str,
        items: &[Header],
        lang: crate::prefs::Lang,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = ui.p;
        let mut table = div()
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(t.border_subtle))
            .bg(rgb(t.code))
            .overflow_hidden();
        for (ix, h) in items.iter().enumerate() {
            let line = format!("{}: {}", h.name, h.value);
            let name_label = h.name.clone();
            table = table.child(
                div()
                    .id(ElementId::Name(format!("{prefix}-{ix}").into()))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .min_h(px(26.0))
                    .px(px(10.0))
                    .py(px(3.0))
                    .when(ix > 0, |el| {
                        el.border_t_1().border_color(rgb(t.border_subtle))
                    })
                    .cursor_pointer()
                    .hover(move |s| s.bg(rgb(t.row_hover)))
                    .tooltip(widgets::tip(
                        line.clone(),
                        Some(ui.t("click to copy", "点击复制").into()),
                    ))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(line.clone()));
                        cx.emit(InspectorEvent::Toast(i18n::copied(&name_label, lang)));
                    }))
                    .child(
                        div()
                            .w(px(148.0))
                            .flex_shrink_0()
                            .font(fonts::mono_font())
                            .text_size(px(text::XS))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(t.accent))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(h.name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .font(fonts::mono_font())
                            .text_size(px(text::XS))
                            .text_color(rgb(t.text_body))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(h.value.clone()),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(widgets::label(ui, title.to_string()))
            .child(table)
    }

    fn render_access(&self, ui: &Ui, call: &Trace, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = ui.p;
        let mut list = div().flex().flex_col();
        let entries: Vec<_> = call.call_access().collect();
        if entries.is_empty() {
            list = list.child(
                div()
                    .text_size(px(text::MD))
                    .text_color(rgb(t.text_muted))
                    .child(ui.t(
                        "This call needs no IAM permission Lenslet knows of.",
                        "Lenslet 未识别到此调用需要的 IAM 权限。",
                    )),
            );
        }
        for (index, (access, implied)) in entries.into_iter().enumerate() {
            let key = access.key();
            let mut name = div().flex().items_center().gap(px(6.0)).child(
                div()
                    .font(fonts::mono_font())
                    .text_size(px(text::MD))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(t.text))
                    .child(access.action.clone()),
            );
            if implied {
                name = name.child(widgets::badge(ui, "IMPLIED", t.text_muted));
            }
            if call.is_access_new(access) {
                name = name.child(if call.blocked {
                    widgets::badge(ui, "DENIED", t.purple)
                } else {
                    widgets::badge(ui, "NOT IN LOCK", t.amber)
                });
            }
            list = list.child(
                div()
                    .id(ElementId::named_usize("call-access", index))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .py(px(6.0))
                    .px(px(8.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .hover(move |s| s.bg(rgb(t.row_hover)))
                    .tooltip(widgets::tip(
                        ui.t("Show in Access", "在权限视图中查看"),
                        None,
                    ))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(InspectorEvent::OpenAccess(key.clone()));
                    }))
                    .child(name)
                    .child(widgets::mono(ui, access.resource.clone())),
            );
        }
        let request_id = call
            .response
            .as_ref()
            .and_then(|r| {
                r.header("x-amz-request-id")
                    .or_else(|| r.header("x-amzn-requestid"))
            })
            .unwrap_or("—")
            .to_string();
        let mut meta = div().flex().flex_col();
        let semantic = call.semantic_target();
        if !semantic.is_empty() {
            meta = meta.child(widgets::meta_row(
                ui,
                ui.t("Target", "目标资源"),
                widgets::mono(ui, semantic),
            ));
        }
        if let Some(role) = call.caller_role.as_deref() {
            meta = meta.child(widgets::meta_row(
                ui,
                ui.t("Assumed Role", "调用角色"),
                widgets::mono(ui, role.to_string()),
            ));
        }
        meta = meta
            .child(widgets::meta_row(
                ui,
                "Started",
                widgets::mono(ui, full_time(&call.at)),
            ))
            .child(widgets::meta_row(
                ui,
                "Host",
                widgets::mono(ui, call.host.clone()),
            ))
            .child(widgets::meta_row(
                ui,
                "Region",
                widgets::mono(ui, call.region.clone().unwrap_or_else(|| "—".into())),
            ))
            .child(widgets::meta_row(
                ui,
                "Request ID",
                widgets::mono(ui, request_id),
            ))
            .child(widgets::meta_row(
                ui,
                "Call ID",
                widgets::mono(ui, call.id.clone()),
            ));
        if let Some(invocation) = call.invocation_id.as_deref() {
            meta = meta.child(widgets::meta_row(
                ui,
                "Invocation",
                widgets::mono(ui, invocation.to_string()),
            ));
        }
        for (side, exchange) in [
            ("Request", call.request.as_ref()),
            ("Response", call.response.as_ref()),
        ] {
            if let Some(sha) = exchange
                .and_then(|e| e.body.as_ref())
                .and_then(|b| b.sha256.as_deref())
                .filter(|sha| sha.len() > 8)
            {
                meta = meta.child(widgets::meta_row(
                    ui,
                    format!("{side} SHA-256"),
                    widgets::mono(ui, sha.to_string()),
                ));
            }
        }
        if let Some(of) = call.replay_of.as_deref() {
            meta = meta.child(widgets::meta_row(
                ui,
                "Replay of",
                widgets::mono(ui, of.to_string()),
            ));
        }
        div()
            .id("inspector-access")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .gap(px(16.0))
            .p(px(16.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(widgets::label(ui, ui.t("IAM PERMISSIONS", "IAM 权限")))
                    .child(list),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(widgets::label(ui, ui.t("CALL", "调用")))
                    .child(meta),
            )
    }

    fn render_empty(&self, ui: &Ui) -> AnyElement {
        let t = ui.p;
        let key = |k: &str, label: &str| {
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .min_w(px(56.0))
                        .text_right()
                        .font(fonts::mono_font())
                        .text_size(px(text::SM))
                        .text_color(rgb(t.text_dim))
                        .child(k.to_string()),
                )
                .child(
                    div()
                        .text_size(px(text::SM))
                        .text_color(rgb(t.text_muted))
                        .child(label.to_string()),
                )
        };
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .child(
                div()
                    .text_size(px(text::LG))
                    .text_color(rgb(t.text_dim))
                    .child(ui.t("Select a call", "选择一个请求")),
            )
            .child(key("j / k", ui.t("next / previous", "下一个 / 上一个")))
            .child(key("/", ui.t("search", "搜索")))
            .child(key(&i18n::shortcut("K"), ui.t("commands", "命令面板")))
            .into_any_element()
    }
}

fn compare_column(ui: &Ui, title: &str, status: u16, ms: u64, bytes: usize) -> Div {
    let t = ui.p;
    div()
        .flex_1()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .p(px(8.0))
        .rounded(px(6.0))
        .bg(theme::h(t.text, 0.03))
        .child(widgets::label(ui, title.to_string()))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .font(fonts::mono_font())
                .text_size(px(text::MD))
                .child(
                    div()
                        .text_color(ui.status_color(status))
                        .child(status_label(status)),
                )
                .child(div().text_color(rgb(t.text_dim)).child(format_latency(ms)))
                .child(
                    div()
                        .text_color(rgb(t.text_muted))
                        .child(format_bytes(bytes)),
                ),
        )
}

fn default_side(call: Option<&Trace>) -> Side {
    let Some(call) = call else {
        return Side::Response;
    };
    let has = |exchange: Option<&Exchange>| {
        exchange
            .and_then(|e| e.body.as_ref())
            .is_some_and(|body| !body.is_empty())
    };
    if call.is_error() || has(call.response.as_ref()) || !has(call.request.as_ref()) {
        Side::Response
    } else {
        Side::Request
    }
}

fn format_for_diff(body: Option<&Body>, content_type: &str, mode: BodyMode) -> String {
    let Some(body) = body else {
        return String::new();
    };
    let Some(text) = body.text.as_deref() else {
        return String::new();
    };
    if mode == BodyMode::Decoded {
        if let Some((_, decoded, _)) =
            decode_aws_semantic(text, content_type, body.encoding.as_deref())
        {
            return decoded;
        }
        if let Some(pretty) = pretty_body(text, content_type) {
            return pretty;
        }
    }
    text.to_string()
}

/// What a document is built from (owned, so it can move to a background task).
enum DocSource {
    Body {
        body: Option<Body>,
        content_type: String,
    },
    Diff {
        rec_body: Option<Body>,
        rec_ct: String,
        rep_body: Option<Body>,
        rep_ct: String,
    },
    Text(String),
}

impl DocSource {
    fn of(call: &Trace, tab: Tab, side: Side, replay: Option<&ReplayState>) -> DocSource {
        let message = |exchange: Option<&Exchange>| DocSource::Body {
            body: exchange.and_then(|e| e.body.clone()),
            content_type: exchange
                .and_then(|e| e.content_type.clone())
                .unwrap_or_default(),
        };
        if tab == Tab::Headers {
            let mut text = String::new();
            for (title, exchange) in [
                ("Request", call.request.as_ref()),
                ("Response", call.response.as_ref()),
            ] {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("# {title}\n"));
                if let Some(exchange) = exchange {
                    for header in &exchange.headers {
                        text.push_str(&format!("{}: {}\n", header.name, header.value));
                    }
                }
            }
            return DocSource::Text(text.trim_end().to_string());
        }
        match side {
            Side::Request => message(call.request.as_ref()),
            Side::Response => message(call.response.as_ref()),
            Side::Replay => match replay {
                Some(ReplayState::Done(ReplayOutcome {
                    body_text,
                    content_type,
                    body_bytes,
                    ..
                })) => DocSource::Body {
                    body: body_text.clone().map(|text| Body {
                        text: Some(text),
                        bytes: *body_bytes,
                        ..Body::default()
                    }),
                    content_type: content_type.clone().unwrap_or_default(),
                },
                _ => DocSource::Text(String::new()),
            },
            Side::Diff => match replay {
                Some(ReplayState::Done(ReplayOutcome {
                    body_text,
                    content_type,
                    body_bytes,
                    ..
                })) => DocSource::Diff {
                    rec_body: call.response.as_ref().and_then(|e| e.body.clone()),
                    rec_ct: call
                        .response
                        .as_ref()
                        .and_then(|e| e.content_type.clone())
                        .unwrap_or_default(),
                    rep_body: body_text.clone().map(|text| Body {
                        text: Some(text),
                        bytes: *body_bytes,
                        ..Body::default()
                    }),
                    rep_ct: content_type.clone().unwrap_or_default(),
                },
                _ => DocSource::Text(String::new()),
            },
        }
    }

    fn len(&self) -> usize {
        match self {
            DocSource::Body { body, .. } => body.as_ref().map_or(0, |body| {
                body.text.as_ref().map_or(0, String::len) + body.hex.as_ref().map_or(0, String::len)
            }),
            DocSource::Diff {
                rec_body, rep_body, ..
            } => {
                rec_body
                    .as_ref()
                    .and_then(|b| b.text.as_ref())
                    .map_or(0, String::len)
                    + rep_body
                        .as_ref()
                        .and_then(|b| b.text.as_ref())
                        .map_or(0, String::len)
            }
            DocSource::Text(text) => text.len(),
        }
    }

    fn build(&self, mode: BodyMode, tokens: &[String], palette: &theme::Palette) -> BodyDoc {
        match self {
            DocSource::Body { body, content_type } => {
                body::build(body.as_ref(), content_type, mode, tokens, palette)
            }
            DocSource::Diff {
                rec_body,
                rec_ct,
                rep_body,
                rep_ct,
            } => {
                let a = format_for_diff(rec_body.as_ref(), rec_ct, mode);
                let b = format_for_diff(rep_body.as_ref(), rep_ct, mode);
                let unified = body::diff_text(&a, &b);
                BodyDoc::from_text(unified, SyntaxKind::Diff, tokens, palette)
            }
            DocSource::Text(text) => {
                BodyDoc::from_text(text.clone(), SyntaxKind::Plain, tokens, palette)
            }
        }
    }
}

impl Render for Inspector {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scope = crate::fsguard::RenderScope::enter();
        let ui = Ui::of(cx);
        let t = ui.p;
        let container = div()
            .id("inspector")
            .key_context("Inspector")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(0.0))
            .bg(rgb(t.panel_raised));
        let Some(call) = self.call().cloned() else {
            return container.child(self.render_empty(&ui));
        };
        let content: AnyElement = match self.tab {
            Tab::Exchange => self.render_exchange(&ui, &call, cx).into_any_element(),
            Tab::Headers => self.render_headers(&ui, &call, cx).into_any_element(),
            Tab::Access => self.render_access(&ui, &call, cx).into_any_element(),
        };
        container
            .child(self.render_header(&ui, &call, cx))
            .child(content)
    }
}
