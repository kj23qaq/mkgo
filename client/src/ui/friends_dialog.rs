// SPDX-FileCopyrightText: 2021 Softbear, Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::Deserialize;
use stylist::yew::styled_component;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{window, Headers, HtmlInputElement, InputEvent, RequestInit, RequestMode, Response};
use yew::prelude::*;

const PRIVATE_BOT_LIMIT: u8 = 8;

#[derive(Clone, Copy, PartialEq)]
struct PrivateRoom {
    invitation_id: u32,
    code: u32,
    secret: u64,
}

#[derive(Deserialize)]
struct CreatePrivateRoomResponse {
    invitation_id: u32,
    code: u32,
    secret: u64,
}

#[derive(Properties, PartialEq)]
pub struct FriendsDialogProps {
    pub on_start_public: Callback<()>,
}

#[styled_component(FriendsDialog)]
pub fn friends_dialog(props: &FriendsDialogProps) -> Html {
    let opened = use_state(|| false);
    let private_selected = use_state(|| false);
    let has_selection = use_state(|| false);
    let room = use_state(|| None::<PrivateRoom>);
    let bot_count = use_state(|| PRIVATE_BOT_LIMIT);
    let copied = use_state(|| false);
    let busy = use_state(|| false);
    let creating = use_state(|| false);
    let error = use_state(|| None::<String>);

    let panel_style = css!(
        r#"
        position: fixed;
        z-index: 1000;
        right: 1rem;
        bottom: 4.5rem;
        width: min(24rem, calc(100vw - 2rem));
        box-sizing: border-box;
        padding: 1rem;
        border: 1px solid #ffffff88;
        border-radius: 0.75rem;
        background: #18232eee;
        color: white;
        font: 1rem sans-serif;
        box-shadow: 0 0.3rem 1.5rem #0009;

        button {
            border: 1px solid #ffffff77;
            border-radius: 0.4rem;
            background: #34495e;
            color: white;
            cursor: pointer;
            font-size: 1rem;
            padding: 0.5rem 0.75rem;
        }

        button:hover:not(:disabled) { filter: brightness(1.2); }
        button:disabled { cursor: default; opacity: 0.6; }
        input[type="range"] { width: 100%; }
        "#
    );

    let on_open = {
        let opened = opened.clone();
        Callback::from(move |_| opened.set(true))
    };
    let on_public = {
        let private_selected = private_selected.clone();
        let has_selection = has_selection.clone();
        let copied = copied.clone();
        Callback::from(move |_| {
            private_selected.set(false);
            has_selection.set(true);
            copied.set(false);
        })
    };
    let on_private = {
        let private_selected = private_selected.clone();
        let has_selection = has_selection.clone();
        let room = room.clone();
        let creating = creating.clone();
        let error = error.clone();
        Callback::from(move |_| {
            private_selected.set(true);
            has_selection.set(true);
            error.set(None);
            if room.is_some() || *creating {
                return;
            }
            creating.set(true);
            let room = room.clone();
            let creating = creating.clone();
            let error = error.clone();
            spawn_local(async move {
                match post_json::<CreatePrivateRoomResponse>(
                    "/private-room",
                    &format!(r#"{{"bot_count":{PRIVATE_BOT_LIMIT}}}"#),
                )
                .await
                {
                    Ok(created) => room.set(Some(PrivateRoom {
                        invitation_id: created.invitation_id,
                        code: created.code,
                        secret: created.secret,
                    })),
                    Err(message) => error.set(Some(message)),
                }
                creating.set(false);
            });
        })
    };
    let on_copy = {
        let private_selected = private_selected.clone();
        let room = room.clone();
        let copied = copied.clone();
        let error = error.clone();
        Callback::from(move |_| {
            let target = if *private_selected {
                room.as_ref().map(|room| {
                    format!(
                        "{}/invite/{}/",
                        window().unwrap().location().origin().unwrap(),
                        room.invitation_id
                    )
                })
            } else {
                Some(format!(
                    "{}/",
                    window().unwrap().location().origin().unwrap()
                ))
            };
            let target = match target {
                Some(target) => target,
                None => return,
            };
            let copied = copied.clone();
            let error = error.clone();
            spawn_local(async move {
                match window().unwrap().navigator().clipboard() {
                    Some(clipboard) => match JsFuture::from(clipboard.write_text(&target)).await {
                        Ok(_) => copied.set(true),
                        Err(_) => error.set(Some("无法复制链接，请检查浏览器剪贴板权限。awa".into())),
                    },
                    None => error.set(Some("当前浏览器不支持复制链接awa。".into())),
                }
            });
        })
    };
    let on_bot_count = {
        let bot_count = bot_count.clone();
        Callback::from(move |event: InputEvent| {
            if let Some(input) = event.target_dyn_into::<HtmlInputElement>() {
                if let Ok(value) = input.value().parse::<u8>() {
                    bot_count.set(value.min(PRIVATE_BOT_LIMIT));
                }
            }
        })
    };
    let on_start_public = {
        let opened = opened.clone();
        let on_start_public = props.on_start_public.clone();
        Callback::from(move |_| {
            opened.set(false);
            on_start_public.emit(());
        })
    };
    let on_start_private = {
        let room = room.clone();
        let bot_count = bot_count.clone();
        let busy = busy.clone();
        let error = error.clone();
        Callback::from(move |_| {
            let room = match *room {
                Some(room) => room,
                None => return,
            };
            busy.set(true);
            error.set(None);
            let bot_count = *bot_count;
            let busy = busy.clone();
            let error = error.clone();
            spawn_local(async move {
                let body = format!(r#"{{"bot_count":{},"secret":{}}}"#, bot_count, room.secret);
                match post_json::<serde_json::Value>(
                    &format!("/private-room/{}/bots", room.invitation_id),
                    &body,
                )
                .await
                {
                    Ok(_) => {
                        let url = format!("/invite/{}/?private_start=1", room.invitation_id);
                        if let Err(_) = window().unwrap().location().set_href(&url) {
                            busy.set(false);
                            error.set(Some("无法打开私人服务器链接。".into()));
                        }
                    }
                    Err(message) => {
                        busy.set(false);
                        error.set(Some(message));
                    }
                }
            });
        })
    };

    html! {
        <>
            <button
                style="position:fixed;z-index:999;right:1rem;bottom:1rem;"
                onclick={on_open}
            >
                {"和朋友一起玩"}
            </button>
            if *opened {
                <section class={panel_style}>
                    <div style="display:flex;justify-content:space-between;align-items:center;">
                        <strong>{"和朋友一起玩"}</strong>
                        <button onclick={{
                            let opened = opened.clone();
                            Callback::from(move |_| opened.set(false))
                        }}>{"关闭"}</button>
                    </div>
                    <div style="display:flex;gap:0.5rem;margin:1rem 0;">
                        <button onclick={on_public.clone()}>{"邀请朋友到主服务器"}</button>
                        <button onclick={on_private.clone()}>{"邀请朋友到私人服务器"}</button>
                    </div>
                    if !*has_selection {
                        <p>{"请选择要邀请朋友前往的服务器。"}</p>
                    } else if *private_selected {
                        if let Some(room) = *room {
                            <p>{format!("六位邀请码：{:06}", room.code)}</p>
                            <label for="private_bot_count">
                                {format!("机器人数量：{}", *bot_count)}
                            </label>
                            <input
                                id="private_bot_count"
                                type="range"
                                min="0"
                                max={PRIVATE_BOT_LIMIT.to_string()}
                                value={bot_count.to_string()}
                                oninput={on_bot_count}
                            />
                            <div style="display:flex;justify-content:space-between;gap:0.5rem;">
                                <button onclick={on_copy}>
                                    {if *copied { "已复制邀请链接" } else { "复制邀请链接" }}
                                </button>
                                <button onclick={on_start_private} disabled={*busy}>
                                    {if *busy { "正在进入...(轰！擦擦！魔法变身！)" } else { "开始" }}
                                </button>
                            </div>
                        } else if error.is_none() {
                            <p>{"正在生成私人服务器和邀请码..."}</p>
                        }
                    } else {
                        <p>{"邀请链接将带朋友进入主服务器。"}</p>
                        <div style="display:flex;justify-content:space-between;gap:0.5rem;">
                            <button onclick={on_copy}>
                                {if *copied { "已复制主服务器链接" } else { "复制主服务器链接" }}
                            </button>
                            <button onclick={on_start_public}>{"开始"}</button>
                        </div>
                    }
                    if let Some(error) = &*error {
                        <p style="color:#ffb3a7;">{error}</p>
                    }
                </section>
            }
        </>
    }
}

async fn post_json<T: for<'de> Deserialize<'de>>(url: &str, body: &str) -> Result<T, String> {
    let headers = Headers::new().map_err(|error| format!("{error:?}"))?;
    headers
        .set("Content-Type", "application/json")
        .map_err(|error| format!("{error:?}"))?;
    let mut options = RequestInit::new();
    options.method("POST");
    options.mode(RequestMode::SameOrigin);
    options.headers(&headers);
    options.body(Some(&JsValue::from_str(body)));
    let request = web_sys::Request::new_with_str_and_init(url, &options)
        .map_err(|error| format!("{error:?}"))?;
    let response: Response = JsFuture::from(window().unwrap().fetch_with_request(&request))
        .await
        .map_err(|error| format!("{error:?}"))?
        .dyn_into()
        .map_err(|error| format!("{error:?}"))?;
    let text = JsFuture::from(response.text().map_err(|error| format!("{error:?}"))?)
        .await
        .map_err(|error| format!("{error:?}"))?
        .as_string()
        .ok_or_else(|| "服务器返回了无效响应。你只能受着".to_string())?;
    if !response.ok() {
        return Err(if text.is_empty() {
            "请求失败，请稍后重试。".to_string()
        } else {
            text
        });
    }
    serde_json::from_str(&text).map_err(|error| error.to_string())
}
