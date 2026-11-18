use crate::api::JsonFields as _;
use leptos::prelude::*;
use serde_json::{Value, json};

use crate::{
    api::call,
    app::{Notice, array, confirm, form, optional, reload, run, text, value},
};

const SOURCES: &str = "/api/admin/sources";

#[component]
pub fn Sources(notice: Notice) -> impl IntoView {
    let sources = RwSignal::new(Value::Null);
    let editor = RwSignal::new(None::<Value>);
    let busy = RwSignal::new(false);
    reload(SOURCES, sources, notice);

    view! {
        <section>
            <div class="page-head">
                <h1>"Sources"</h1>
                <button type="button" class="primary" disabled=move || busy.get()
                    on:click=move |_| editor.set(Some(Value::Null))>"Add source"</button>
            </div>
            {move || editor.get().map(|source| view! {
                <SourceEditor source=source sources=sources editor=editor busy=busy notice=notice/>
            })}
            {move || {
                let rows = sources.get();
                if rows.is_null() {
                    return view! { <p class="loading">"Loading sources…"</p> }.into_any();
                }
                if rows.as_array().is_some_and(Vec::is_empty) {
                    return view! { <p class="panel">"Add a calendar feed to start importing events."</p> }.into_any();
                }
                view! {
                    <ul class="row-list">
                        {array(&rows).into_iter().map(|source| view! {
                            <SourceItem source=source sources=sources editor=editor busy=busy notice=notice/>
                        }).collect_view()}
                    </ul>
                }.into_any()
            }}
        </section>
    }
}

#[component]
fn SourceItem(
    source: Value,
    sources: RwSignal<Value>,
    editor: RwSignal<Option<Value>>,
    busy: RwSignal<bool>,
    notice: Notice,
) -> impl IntoView {
    let id = source.field("id").as_i64().unwrap_or_default();
    let name = text(&source, "name");
    let enabled = source.field("enabled").as_bool().unwrap_or(false);
    let notify = source.field("notify").as_bool().unwrap_or(false);
    let error = source.field("last_error").as_str().map(str::to_owned);
    let failed = error.is_some();
    let last_result = source
        .field("last_result")
        .as_str()
        .unwrap_or("Waiting for first sync.")
        .to_owned();
    let path = format!("{SOURCES}/{id}");
    let sync_path = format!("{path}/sync");
    let toggle_path = path.clone();
    let delete_prompt = format!("Delete {name} and its imported events?");
    view! {
        <li class="panel split" class:failed=failed>
            <div>
                <h2>{name}</h2>
                <p>{last_result}</p>
                {error.map(|error| view! { <p class="error">{error}</p> })}
                {(!enabled).then(|| view! { <p>"Disabled"</p> })}
                {(enabled && !notify).then(|| view! { <p>"Reminders off"</p> })}
            </div>
            <div class="actions">
                <button type="button" class="quiet" disabled=move || busy.get()
                    on:click=move |_| editor.set(Some(source.clone()))>"Edit"</button>
                <button type="button" class="quiet" disabled=move || busy.get() || !enabled
                    on:click=move |_| mutate("POST", sync_path.clone(), None, sources, busy, notice, || {})>
                    "Sync now"
                </button>
                <button type="button" class="quiet" disabled=move || busy.get()
                    on:click=move |_| mutate("PATCH", toggle_path.clone(), Some(json!({"enabled": !enabled})), sources, busy, notice, || {})>
                    {if enabled { "Disable" } else { "Enable" }}
                </button>
                <button type="button" class="quiet" disabled=move || busy.get() on:click=move |_| {
                    if confirm(&delete_prompt) {
                        mutate("DELETE", path.clone(), None, sources, busy, notice, move || {
                            if editor.get_untracked().is_some_and(|row| row.field("id").as_i64() == Some(id)) {
                                editor.set(None);
                            }
                        });
                    }
                }>"Delete"</button>
            </div>
        </li>
    }
}

fn mutate(
    method: &'static str,
    path: String,
    body: Option<Value>,
    sources: RwSignal<Value>,
    busy: RwSignal<bool>,
    notice: Notice,
    on_success: impl FnOnce() + 'static,
) {
    if busy.get_untracked() {
        return;
    }
    busy.set(true);
    run(notice, async move {
        let result = async {
            call(method, &path, body).await?;
            on_success();
            sources.set(call("GET", SOURCES, None).await?);
            Ok(())
        }
        .await;
        busy.set(false);
        result
    });
}

#[component]
fn SourceEditor(
    source: Value,
    sources: RwSignal<Value>,
    editor: RwSignal<Option<Value>>,
    busy: RwSignal<bool>,
    notice: Notice,
) -> impl IntoView {
    let id = source.field("id").as_i64();
    let label = if id.is_some() {
        "Edit source"
    } else {
        "Add source"
    };
    let color = source
        .field("color")
        .as_str()
        .unwrap_or("#45786b")
        .to_owned();
    let interval = source
        .field("interval_min")
        .as_u64()
        .unwrap_or(30)
        .to_string();
    let default_lead = source
        .field("default_lead_min")
        .as_u64()
        .map_or_else(String::new, |lead| lead.to_string());
    let enabled = source.field("enabled").as_bool().unwrap_or(true);
    let notify = source.field("notify").as_bool().unwrap_or(true);
    let honor = source.field("honor_valarm").as_bool().unwrap_or(true);

    let submit = move |event| {
        let Some(data) = form(event) else { return };
        if busy.get_untracked() {
            return;
        }
        let Some(interval) = value(&data, "interval_min")
            .parse::<u32>()
            .ok()
            .filter(|n| *n >= 15)
        else {
            notice.set("Enter a sync interval of at least 15 minutes.".into());
            return;
        };
        let lead = match optional(&data, "default_lead_min") {
            None => None,
            Some(lead) => {
                let Some(lead) = lead.parse::<u32>().ok().filter(|n| *n <= 10_080) else {
                    notice.set("Enter a reminder lead from 0 to 10,080 minutes.".into());
                    return;
                };
                Some(lead)
            }
        };
        let method = if id.is_some() { "PATCH" } else { "POST" };
        let path = id.map_or_else(|| SOURCES.to_owned(), |id| format!("{SOURCES}/{id}"));
        let body = json!({
            "name": value(&data, "name").trim(),
            "url": value(&data, "url").trim(),
            "color": value(&data, "color"),
            "enabled": data.has("enabled"),
            "notify": data.has("notify"),
            "honor_valarm": data.has("honor_valarm"),
            "interval_min": interval,
            "default_lead_min": lead,
        });
        mutate(method, path, Some(body), sources, busy, notice, move || {
            editor.set(None)
        });
    };

    view! {
        <form class="panel" on:submit=submit>
            <h2>{label}</h2>
            <div class="form-grid">
                <label>"Name"<input name="name" required maxlength=200 value=text(&source, "name")/></label>
                <label>"Calendar URL"<input name="url" type="url" required value=text(&source, "url")/></label>
                <label>"Color"<input name="color" type="color" value=color/></label>
                <label>"Sync interval (minutes)"<input name="interval_min" type="number" min=15 required value=interval/></label>
                <label>"Default reminder lead (minutes)"
                    <input name="default_lead_min" type="number" min=0 max=10080 value=default_lead placeholder="Use settings default"/>
                </label>
            </div>
            <div class="form-grid">
                <label class="check"><input name="enabled" type="checkbox" prop:checked=enabled/>"Import events"</label>
                <label class="check"><input name="notify" type="checkbox" prop:checked=notify/>"Send reminders"</label>
                <label class="check"><input name="honor_valarm" type="checkbox" prop:checked=honor/>"Use reminders from the feed"</label>
            </div>
            <div class="actions">
                <button type="submit" class="primary" disabled=move || busy.get()>"Save source"</button>
                <button type="button" class="quiet" disabled=move || busy.get() on:click=move |_| editor.set(None)>"Cancel"</button>
            </div>
        </form>
    }
}
