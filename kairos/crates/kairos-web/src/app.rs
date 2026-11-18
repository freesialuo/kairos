use crate::api::JsonFields as _;
use crate::{
    api::{ApiError, call},
    pages, passkey, strings,
};
use leptos::prelude::*;
use leptos_router::{components::Router, hooks::use_location};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use web_sys::{FormData, HtmlFormElement};

pub type Notice = RwSignal<String>;
pub fn go(path: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.location().set_href(path);
    }
}
pub fn form(event: web_sys::SubmitEvent) -> Option<FormData> {
    event.prevent_default();
    let el = event.current_target()?.dyn_into::<HtmlFormElement>().ok()?;
    FormData::new_with_form(&el).ok()
}
pub fn value(data: &FormData, key: &str) -> String {
    data.get(key).as_string().unwrap_or_default()
}
pub fn optional(data: &FormData, key: &str) -> Option<String> {
    let s = value(data, key);
    (!s.is_empty()).then_some(s)
}
pub fn run<F>(notice: Notice, work: F)
where
    F: std::future::Future<Output = Result<(), ApiError>> + 'static,
{
    notice.set(String::new());
    leptos::task::spawn_local(async move {
        if let Err(error) = work.await {
            notice.set(error.to_string());
        }
    });
}
pub fn reload(path: &'static str, target: RwSignal<Value>, notice: Notice) {
    run(notice, async move {
        target.set(call("GET", path, None).await?);
        Ok(())
    });
}
pub fn array(value: &Value) -> Vec<Value> {
    value.as_array().cloned().unwrap_or_default()
}
pub fn text(value: &Value, key: &str) -> String {
    value.field(key).as_str().unwrap_or_default().into()
}
pub fn confirm(s: &str) -> bool {
    web_sys::window()
        .and_then(|w| w.confirm_with_message(s).ok())
        .unwrap_or(false)
}

#[component]
pub fn App() -> impl IntoView {
    view! {<Router><Workspace/></Router>}
}
#[component]
fn Workspace() -> impl IntoView {
    let path = use_location().pathname;
    let state = RwSignal::new(None::<Value>);
    let notice = RwSignal::new(String::new());
    let location = path.get_untracked();
    run(notice, async move {
        let value = call("GET", "/auth/state", None).await?;
        let scope = value.field("session").as_str();
        if scope == Some("full") && ["/", "/setup", "/login"].contains(&location.as_str()) {
            go("/calendar");
        } else if scope != Some("full") && location != "/setup" {
            let dest = if scope == Some("recovery") || *value.field("setup_required") == true {
                "/setup"
            } else {
                "/login"
            };
            if location != dest {
                go(dest);
            }
        }
        state.set(Some(value));
        Ok(())
    });
    view! {
        <header class="topbar"><a rel="external" class="brand" href="/calendar"><span class="dial" aria-hidden="true"></span>"Kairos"</a>
            {move || (state.get().is_some_and(|s|*s.field("session") =="full")).then(||view!{
            <nav aria-label="Main navigation">{strings::PAGES.into_iter().map(|(url,name)|view!{
                <a rel="external" href=url aria-current=move ||(path.get()==url).then_some("page")>{name}</a>
            }).collect_view()}</nav>
            <button class="quiet logout" on:click=move |_|run(notice,async move{call("POST","/auth/logout",Some(json!({}))).await?;go("/login");Ok(())})>"Log out"</button>
            })}
        </header>
        <main>
        <div role="alert" class="notice" hidden=move||notice.get().is_empty()>{move||notice.get()}</div>
        {move||match state.get(){
            None=>view!{<p class="loading">"Opening calendar…"</p>}.into_any(),
            Some(auth) if *auth.field("session") =="full"=>match path.get().as_str(){
                "/calendar"|"/"=>view!{<pages::calendar::Calendar notice=notice/>}.into_any(),
                "/sources"=>view!{<pages::sources::Sources notice=notice/>}.into_any(),
                "/events"=>view!{<pages::events::Events notice=notice/>}.into_any(),
                "/settings"=>view!{<pages::settings::Settings notice=notice/>}.into_any(),
                "/log"=>view!{<pages::log::Log notice=notice/>}.into_any(),
                _=>view!{<p>"Page unavailable. " <a rel="external" href="/calendar">"Open calendar"</a></p>}.into_any(),
            },
            Some(auth)=>view!{<SignIn setup=path.get()=="/setup" recovery=*auth.field("session") =="recovery" fresh=*auth.field("setup_required") ==true notice=notice/>}.into_any(),
        }}</main>
    }
}

#[component]
fn SignIn(setup: bool, recovery: bool, fresh: bool, notice: Notice) -> impl IntoView {
    let busy = RwSignal::new(false);
    let on_submit = move |ev| {
        let Some(data) = form(ev) else {
            return;
        };
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        run(notice, async move {
            let result=async{
                if recovery {
                    passkey::ceremony(true,optional(&data,"name")).await?;
                    if fresh{call("PUT","/api/admin/settings",Some(json!({"timezone":passkey::browser_zone(),"default_lead_min":15,"all_day_time":"09:00"}))).await?;}
                    go("/calendar");
                }else if setup{
                    call("POST","/auth/recovery",Some(json!({"code":value(&data,"code")}))).await?;go("/setup");
                }else{passkey::ceremony(false,None).await?;go("/calendar");}
                Ok(())
            }.await;
            busy.set(false);
            result
        });
    };
    view! {<section class="entry"><div class="entry-mark" aria-hidden="true"><span></span></div>
        <h1>{if recovery{"Add a passkey"}else if setup{"Open your calendar"}else{"Welcome back"}}</h1>
        <p>{if recovery{strings::REGISTER}else if setup{strings::RECOVERY}else{strings::SIGN_IN}}</p>
        <form on:submit=on_submit>
            {(!recovery && setup).then(||view!{<label>"Recovery code"<input required type="password" name="code" autocomplete="off"/></label>})}
            {recovery.then(||view!{<label>"Passkey name"<input name="name" placeholder="This device" maxlength=200/></label>})}
            <button class="primary" disabled=move||busy.get()>{if recovery{"Register passkey"}else if setup{"Continue"}else{"Sign in with passkey"}}</button>
        </form>
        {(!setup&&!recovery).then(||view!{<a rel="external" href="/setup">"Use recovery code"</a>})}
    </section>}
}
