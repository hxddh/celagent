//! Interactive Request Composer & Local SigV4 Re-signer (`Edit & Replay`, ⇧⌘⏎).
//!
//! Lets developers edit a captured AWS call's HTTP method, path/query, headers,
//! and payload body, automatically re-computes `x-amz-content-sha256` and
//! `Authorization: AWS4-HMAC-SHA256 ...` when SigV4 is enabled, and dispatches
//! strictly over local loopback so mutated responses land straight in the
//! Inspector's Replay & Diff tabs.

use std::path::PathBuf;

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FontWeight, Render, Window, div, prelude::*,
    px, rgb,
};

use super::widgets;
use crate::{
    fonts,
    model::{Header, Trace},
    replay::{self, ComposedRequest, ReplayState, ReplayTarget},
    search::SearchInput,
    theme::{self, Ui, text},
};

pub enum ComposerEvent {
    Close,
    Sent(String, ReplayState),
}

pub struct RequestComposer {
    call_id: String,
    host: String,
    service: String,
    region: String,
    base_headers: Vec<Header>,
    target: Option<ReplayTarget>,
    home: PathBuf,
    resign_sigv4: bool,
    sending: bool,
    method_input: Entity<SearchInput>,
    path_input: Entity<SearchInput>,
    header_input: Entity<SearchInput>,
    body_input: Entity<SearchInput>,
}

impl EventEmitter<ComposerEvent> for RequestComposer {}

impl RequestComposer {
    pub fn new(home: PathBuf, cx: &mut Context<Self>) -> Self {
        let method_input = cx.new(|cx| SearchInput::new_field("POST", "POST", true, cx));
        let path_input = cx.new(|cx| SearchInput::new_field("/", "/path?query=1", true, cx));
        let header_input = cx.new(|cx| {
            SearchInput::new_field("", "x-amz-target: DynamoDB_20120810.GetItem", true, cx)
        });
        let body_input = cx.new(|cx| SearchInput::new_field("", "{\"Key\": ...}", true, cx));
        Self {
            call_id: String::new(),
            host: String::new(),
            service: "s3".into(),
            region: "us-east-1".into(),
            base_headers: Vec::new(),
            target: None,
            home,
            resign_sigv4: true,
            sending: false,
            method_input,
            path_input,
            header_input,
            body_input,
        }
    }

    pub fn load(
        &mut self,
        call: &Trace,
        target: ReplayTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.call_id = call.id.clone();
        self.host = call.host.clone();
        self.service = call.service.clone();
        self.region = call
            .region
            .clone()
            .unwrap_or_else(|| "us-east-1".to_string());
        self.base_headers = call
            .request
            .as_ref()
            .map(|r| r.headers.clone())
            .unwrap_or_default();
        self.target = Some(target);
        self.resign_sigv4 = call.authorization || !call.presigned();
        self.sending = false;

        let method = call.method.to_ascii_uppercase();
        let path = call.path.clone();
        let target_hdr = self
            .base_headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("x-amz-target"))
            .map(|h| format!("{}: {}", h.name, h.value))
            .unwrap_or_default();
        let raw_body = call
            .request
            .as_ref()
            .and_then(|r| r.body.as_ref())
            .and_then(|b| b.text.as_deref())
            .unwrap_or("");
        let compact_body = serde_json::from_str::<serde_json::Value>(raw_body)
            .ok()
            .and_then(|v| serde_json::to_string(&v).ok())
            .unwrap_or_else(|| raw_body.replace(['\r', '\n'], " "));

        self.method_input.update(cx, |i, cx| i.set_text(method, cx));
        self.path_input.update(cx, |i, cx| i.set_text(path, cx));
        self.header_input
            .update(cx, |i, cx| i.set_text(target_hdr, cx));
        self.body_input
            .update(cx, |i, cx| i.set_text(compact_body, cx));
        let focus = self.body_input.read(cx).handle();
        window.focus(&focus);
        cx.notify();
    }

    pub fn send(&mut self, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        let Some(target) = self.target.clone() else {
            return;
        };
        let method = self.method_input.read(cx).query().trim().to_string();
        let path = self.path_input.read(cx).query().trim().to_string();
        let extra_hdr = self.header_input.read(cx).query().trim().to_string();
        let body = self.body_input.read(cx).query().to_string();

        let mut headers = self.base_headers.clone();
        if let Some((name, val)) = extra_hdr.split_once(':') {
            let name = name.trim();
            let val = val.trim();
            if !name.is_empty() {
                headers.retain(|h| !h.name.eq_ignore_ascii_case(name));
                headers.push(Header {
                    name: name.to_string(),
                    value: val.to_string(),
                });
            }
        }

        let req = ComposedRequest {
            call_id: (!self.call_id.is_empty()).then(|| self.call_id.clone()),
            method,
            path,
            host: self.host.clone(),
            service: self.service.clone(),
            region: self.region.clone(),
            headers,
            body,
            resign_sigv4: self.resign_sigv4,
        };
        let call_id = self.call_id.clone();
        let home = self.home.clone();
        self.sending = true;
        cx.emit(ComposerEvent::Sent(call_id.clone(), ReplayState::Running));
        cx.notify();

        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { replay::send_composed(&req, &target, &home) })
                .await;
            this.update(cx, |composer, cx| {
                composer.sending = false;
                let state = match outcome {
                    Ok(done) => ReplayState::Done(done),
                    Err(err) => ReplayState::Failed(err),
                };
                cx.emit(ComposerEvent::Sent(call_id, state));
                cx.emit(ComposerEvent::Close);
            })
            .ok();
        })
        .detach();
    }

    fn set_method(&mut self, method: &'static str, cx: &mut Context<Self>) {
        self.method_input.update(cx, |i, cx| i.set_text(method, cx));
        cx.notify();
    }
}

impl Render for RequestComposer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scope = crate::fsguard::RenderScope::enter();
        let ui = Ui::of(cx);
        let t = ui.p;
        let cur_method = self.method_input.read(cx).query().to_ascii_uppercase();

        let mut method_chips = div().flex().items_center().gap(px(4.0));
        for m in ["POST", "GET", "PUT", "DELETE", "HEAD"] {
            let active = cur_method == m;
            method_chips = method_chips.child(
                widgets::chip(&ui, m, m, None, active, t.accent)
                    .on_click(cx.listener(move |comp, _, _, cx| comp.set_method(m, cx))),
            );
        }

        let target_label = self
            .target
            .as_ref()
            .map(|tgt| {
                if tgt.via_proxy {
                    format!("{} ({})", tgt.url, ui.t("via live proxy", "经由实时代理"))
                } else {
                    format!(
                        "{} ({})",
                        tgt.url,
                        ui.t("direct emulator", "直连本地模拟器")
                    )
                }
            })
            .unwrap_or_default();

        let card = div()
            .id("composer-modal")
            .w(px(660.0))
            .flex()
            .flex_col()
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(t.border))
            .bg(rgb(t.panel_raised))
            .shadow(ui.elevation())
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(ComposerEvent::Close)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .h(px(46.0))
                    .px(px(18.0))
                    .border_b_1()
                    .border_color(rgb(t.border_subtle))
                    .child(
                        div()
                            .text_size(px(text::XL))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(t.text))
                            .child(ui.t("Edit & Replay Request", "编辑并重放请求")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .font(fonts::mono_font())
                            .text_size(px(text::XS))
                            .text_color(rgb(t.text_muted))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(target_label),
                    )
                    .child(
                        widgets::button(&ui, "close-composer", ui.t("Cancel", "取消"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ComposerEvent::Close))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .p(px(18.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(div().w(px(88.0)).child(self.method_input.clone()))
                            .child(method_chips),
                    )
                    .child(field_block(
                        &ui,
                        ui.t("PATH & QUERY", "请求路径与查询参数"),
                        self.path_input.clone(),
                    ))
                    .child(field_block(
                        &ui,
                        ui.t("HEADER OVERRIDE (OPTIONAL)", "覆盖/新增 Header（可选）"),
                        self.header_input.clone(),
                    ))
                    .child(field_block(
                        &ui,
                        ui.t("REQUEST BODY", "请求报文体 (JSON / XML / Form)"),
                        self.body_input.clone(),
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .pt(px(4.0))
                            .child(
                                widgets::chip(
                                    &ui,
                                    "toggle-sigv4",
                                    format!(
                                        "{} ({}/{})",
                                        ui.t("Auto re-sign AWS SigV4", "自动重签 AWS SigV4"),
                                        self.service,
                                        self.region
                                    ),
                                    None,
                                    self.resign_sigv4,
                                    t.green,
                                )
                                .on_click(cx.listener(
                                    |comp, _, _, cx| {
                                        comp.resign_sigv4 = !comp.resign_sigv4;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(div().flex_1())
                            .child(
                                widgets::primary_button(
                                    &ui,
                                    "composer-send",
                                    if self.sending {
                                        ui.t("Sending…", "发送中…")
                                    } else {
                                        ui.t("Send to Emulator", "发送到本地模拟器")
                                    },
                                )
                                .on_click(cx.listener(|comp, _, _, cx| comp.send(cx))),
                            ),
                    ),
            );

        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme::h(0x000000, if ui.dark() { 0.44 } else { 0.20 }))
            .child(widgets::appear(&ui, "composer-anim", card))
    }
}

fn field_block(ui: &Ui, title: &str, input: Entity<SearchInput>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(widgets::label(ui, title.to_string()))
        .child(input)
}

pub fn render_composer_overlay(composer: &Entity<RequestComposer>, _cx: &App) -> AnyElement {
    composer.clone().into_any_element()
}
