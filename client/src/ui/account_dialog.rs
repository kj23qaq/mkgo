use core_protocol::dto::AccountRole;
use serde::Deserialize;
use stylist::yew::styled_component;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    window, Headers, HtmlInputElement, InputEvent, Request, RequestInit, RequestMode, Response,
};
use yew::prelude::*;

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct AccountInfo {
    account_name: String,
    role: AccountRole,
}

#[derive(Clone, Copy, PartialEq)]
enum PanelMode {
    Login,
    Register,
    Password,
    Delete,
}

#[styled_component(AccountDialog)]
pub fn account_dialog() -> Html {
    let account = use_state(|| None::<AccountInfo>);
    let opened = use_state(|| false);
    let mode = use_state(|| PanelMode::Login);
    let account_name = use_state(String::new);
    let password = use_state(String::new);
    let current_password = use_state(String::new);
    let new_password = use_state(String::new);
    let delete_verified = use_state(|| false);
    let device_ready = use_state(|| false);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    {
        let account = account.clone();
        let error = error.clone();
        let device_ready = device_ready.clone();
        use_effect_with_deps(
            move |_| {
                spawn_local(async move {
                    if let Err(message) =
                        post_json::<serde_json::Value>("/account/device", "{}").await
                    {
                        error.set(Some(message));
                        return;
                    }
                    device_ready.set(true);
                    match get_json::<Option<AccountInfo>>("/account/me").await {
                        Ok(info) => account.set(info),
                        Err(message) => error.set(Some(message)),
                    }
                });
                || ()
            },
            (),
        );
    }

    let on_account_name = {
        let account_name = account_name.clone();
        Callback::from(move |event: InputEvent| {
            account_name.set(input_value(&event));
        })
    };
    let on_password = {
        let password = password.clone();
        Callback::from(move |event: InputEvent| {
            password.set(input_value(&event));
        })
    };
    let on_current_password = {
        let current_password = current_password.clone();
        Callback::from(move |event: InputEvent| {
            current_password.set(input_value(&event));
        })
    };
    let on_new_password = {
        let new_password = new_password.clone();
        Callback::from(move |event: InputEvent| {
            new_password.set(input_value(&event));
        })
    };

    let on_submit = {
        let mode = mode.clone();
        let account_name = account_name.clone();
        let password = password.clone();
        let account = account.clone();
        let busy = busy.clone();
        let error = error.clone();
        let opened = opened.clone();
        Callback::from(move |_| {
            let endpoint = if *mode == PanelMode::Register {
                "/account/register"
            } else {
                "/account/login"
            };
            let body = serde_json::json!({
                "account_name": (*account_name).clone(),
                "password": (*password).clone(),
            })
            .to_string();
            let account = account.clone();
            let busy = busy.clone();
            let error = error.clone();
            let opened = opened.clone();
            busy.set(true);
            error.set(None);
            spawn_local(async move {
                match post_json::<AccountInfo>(endpoint, &body).await {
                    Ok(info) => {
                        account.set(Some(info));
                        opened.set(false);
                        if let Err(message) = reload_page() {
                            error.set(Some(message));
                        }
                    }
                    Err(message) => error.set(Some(message)),
                }
                busy.set(false);
            });
        })
    };

    let on_logout = {
        let account = account.clone();
        let busy = busy.clone();
        let error = error.clone();
        Callback::from(move |_| {
            let account = account.clone();
            let busy = busy.clone();
            let error = error.clone();
            busy.set(true);
            spawn_local(async move {
                match post_json::<serde_json::Value>("/account/logout", "{}").await {
                    Ok(_) => {
                        account.set(None);
                        if let Err(message) = reload_page() {
                            error.set(Some(message));
                        }
                    }
                    Err(message) => error.set(Some(message)),
                }
                busy.set(false);
            });
        })
    };

    let on_change_password = {
        let current_password = current_password.clone();
        let new_password = new_password.clone();
        let busy = busy.clone();
        let error = error.clone();
        Callback::from(move |_| {
            let body = serde_json::json!({
                "current_password": (*current_password).clone(),
                "new_password": (*new_password).clone(),
            })
            .to_string();
            let current_password = current_password.clone();
            let new_password = new_password.clone();
            let busy = busy.clone();
            let error = error.clone();
            busy.set(true);
            error.set(None);
            spawn_local(async move {
                match post_json::<serde_json::Value>("/account/password", &body).await {
                    Ok(_) => {
                        current_password.set(String::new());
                        new_password.set(String::new());
                        error.set(Some("密码已更改。但没改的是你的心❤".into()));
                        if let Err(message) = reload_page() {
                            error.set(Some(message));
                        }
                    }
                    Err(message) => error.set(Some(message)),
                }
                busy.set(false);
            });
        })
    };

    let on_verify_delete = {
        let password = password.clone();
        let delete_verified = delete_verified.clone();
        let busy = busy.clone();
        let error = error.clone();
        Callback::from(move |_| {
            let body = serde_json::json!({"password": (*password).clone()}).to_string();
            let delete_verified = delete_verified.clone();
            let busy = busy.clone();
            let error = error.clone();
            busy.set(true);
            error.set(None);
            spawn_local(async move {
                match post_json::<serde_json::Value>("/account/delete/verify", &body).await {
                    Ok(_) => delete_verified.set(true),
                    Err(message) => error.set(Some(message)),
                }
                busy.set(false);
            });
        })
    };

    let on_delete = {
        let password = password.clone();
        let account = account.clone();
        let delete_verified = delete_verified.clone();
        let busy = busy.clone();
        let error = error.clone();
        Callback::from(move |_| {
            if !*delete_verified {
                error.set(Some("请先验证账户密码。".into()));
                return;
            }
            let body = serde_json::json!({"password": (*password).clone()}).to_string();
            let account = account.clone();
            let delete_verified = delete_verified.clone();
            let busy = busy.clone();
            let error = error.clone();
            busy.set(true);
            error.set(None);
            spawn_local(async move {
                match post_json::<serde_json::Value>("/account/delete", &body).await {
                    Ok(_) => {
                        account.set(None);
                        delete_verified.set(false);
                        if let Err(message) = reload_page() {
                            error.set(Some(message));
                        }
                    }
                    Err(message) => error.set(Some(message)),
                }
                busy.set(false);
            });
        })
    };

    let button_style = "border:1px solid #ffffff77;border-radius:0.4rem;background:#34495e;color:white;cursor:pointer;font-size:1rem;padding:0.5rem 0.75rem;";
    let input_style = "box-sizing:border-box;width:100%;margin:0.35rem 0;padding:0.5rem;border:1px solid #ffffff77;border-radius:0.3rem;background:#ffffff12;color:white;font-size:1rem;";

    html! {
        <>
            <button
                style="position:fixed;z-index:999;right:10.5rem;bottom:1rem;"
                onclick={{
                    let opened = opened.clone();
                    Callback::from(move |_| opened.set(!*opened))
                }}
            >
                {if account.is_some() { "是的！你已登陆！" } else { "登陆" }}
            </button>
            if *opened {
                <section style="position:fixed;z-index:1001;right:1rem;bottom:4.5rem;width:min(24rem,calc(100vw - 2rem));box-sizing:border-box;padding:1rem;border:1px solid #ffffff88;border-radius:0.75rem;background:#18232eee;color:white;font:1rem sans-serif;box-shadow:0 0.3rem 1.5rem #0009;">
                    <div style="display:flex;justify-content:space-between;align-items:center;">
                        <strong>{"账户"}</strong>
                        <button style={button_style} onclick={{
                            let opened = opened.clone();
                            Callback::from(move |_| opened.set(false))
                        }}>{"关闭"}</button>
                    </div>
                    if !*device_ready && error.is_none() {
                        <p>{"正在准备安全设备标识..."}</p>
                    }
                    if let Some(info) = &*account {
                        <p>{format!("账户名：{}", info.account_name)}</p>
                        <p>{format!("账户身份：{}", info.role.label_zh())}</p>
                        if *mode == PanelMode::Password {
                            <input style={input_style} type="password" placeholder="当前密码" value={(*current_password).clone()} oninput={on_current_password.clone()} />
                            <input style={input_style} type="password" placeholder="新密码（6–128 位）" value={(*new_password).clone()} oninput={on_new_password.clone()} />
                            <button style={button_style} onclick={on_change_password.clone()} disabled={*busy}>{"更改密码"}</button>
                        } else if *mode == PanelMode::Delete {
                            <input style={input_style} type="password" placeholder="输入密码以验证注销" value={(*password).clone()} oninput={on_password.clone()} />
                            if *delete_verified {
                                <button style="border:1px solid #ff8f8f;border-radius:0.4rem;background:#a93226;color:white;cursor:pointer;font-size:1rem;padding:0.5rem 0.75rem;" onclick={on_delete.clone()} disabled={*busy}>{"注销账户"}</button>
                            } else {
                                <p style="font-size:0.9rem;">{"awa"}</p>
                                <button style={button_style} onclick={on_verify_delete.clone()} disabled={*busy}>{"验证密码"}</button>
                            }
                        } else {
                            <div style="display:flex;flex-wrap:wrap;gap:0.5rem;margin-top:1rem;">
                                <button style={button_style} onclick={{
                                    let mode = mode.clone();
                                    let error = error.clone();
                                    Callback::from(move |_| {
                                        mode.set(PanelMode::Password);
                                        error.set(None);
                                    })
                                }}>{"更改密码"}</button>
                                <button style={button_style} onclick={on_logout.clone()} disabled={*busy}>{"退出登陆"}</button>
                                <button style="border:1px solid #ff8f8f;border-radius:0.4rem;background:#a93226;color:white;cursor:pointer;font-size:1rem;padding:0.5rem 0.75rem;" onclick={{
                                    let mode = mode.clone();
                                    let delete_verified = delete_verified.clone();
                                    let error = error.clone();
                                    Callback::from(move |_| {
                                        mode.set(PanelMode::Delete);
                                        delete_verified.set(false);
                                        error.set(None);
                                    })
                                }}>{"注销账户(亲亲求你了别注销)"}</button>
                            </div>
                        }
                    } else {
                        <div style="display:flex;gap:0.5rem;margin:1rem 0;">
                            <button style={button_style} onclick={{
                                let mode = mode.clone();
                                let error = error.clone();
                                Callback::from(move |_| {
                                    mode.set(PanelMode::Login);
                                    error.set(None);
                                })
                            }}>{"登陆"}</button>
                            <button style={button_style} onclick={{
                                let mode = mode.clone();
                                let error = error.clone();
                                Callback::from(move |_| {
                                    mode.set(PanelMode::Register);
                                    error.set(None);
                                })
                            }}>{"现在注册"}</button>
                        </div>
                        <input style={input_style} type="text" autocomplete="username" placeholder="账户名" value={(*account_name).clone()} oninput={on_account_name} />
                        <input style={input_style} type="password" autocomplete={if *mode == PanelMode::Register { "new-password" } else { "current-password" }} placeholder="密码" value={(*password).clone()} oninput={on_password} />
                        <button
                            style={button_style}
                            onclick={on_submit.clone()}
                            disabled={*busy || !*device_ready}
                        >
                            {if *mode == PanelMode::Register { "注册" } else { "登陆" }}
                        </button>
                        <p style="font-size:0.9rem;">{"忘记密码请联系超级管理员。"}</p>
                    }
                    if let Some(error) = &*error {
                        <p style="color:#ffb3a7;">{error}</p>
                    }
                </section>
            }
        </>
    }
}

fn input_value(event: &InputEvent) -> String {
    event
        .target()
        .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
        .map(|input| input.value())
        .unwrap_or_default()
}

fn reload_page() -> Result<(), String> {
    window()
        .ok_or_else(|| "浏览器窗口不可用。".to_string())?
        .location()
        .reload()
        .map_err(|error| format!("{error:?}"))
}

async fn get_json<T: for<'de> Deserialize<'de>>(url: &str) -> Result<T, String> {
    let mut options = RequestInit::new();
    options.method("GET");
    options.mode(RequestMode::SameOrigin);
    let request =
        Request::new_with_str_and_init(url, &options).map_err(|error| format!("{error:?}"))?;
    let response: Response = JsFuture::from(
        window()
            .ok_or_else(|| "浏览器窗口不可用。".to_string())?
            .fetch_with_request(&request),
    )
    .await
    .map_err(|error| format!("{error:?}"))?
    .dyn_into()
    .map_err(|error| format!("{error:?}"))?;
    let text = response_text(response).await?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
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
    let request =
        Request::new_with_str_and_init(url, &options).map_err(|error| format!("{error:?}"))?;
    let response: Response = JsFuture::from(
        window()
            .ok_or_else(|| "浏览器窗口不可用。".to_string())?
            .fetch_with_request(&request),
    )
    .await
    .map_err(|error| format!("{error:?}"))?
    .dyn_into()
    .map_err(|error| format!("{error:?}"))?;
    let status = response.status();
    let text = response_text(response).await?;
    if !(200..300).contains(&status) {
        return Err(if text.is_empty() {
            "请求失败，请稍后重试。".to_string()
        } else {
            text
        });
    }
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

async fn response_text(response: Response) -> Result<String, String> {
    JsFuture::from(response.text().map_err(|error| format!("{error:?}"))?)
        .await
        .map_err(|error| format!("{error:?}"))?
        .as_string()
        .ok_or_else(|| "服务器返回了无效响应。你只能受着".to_string())
}
