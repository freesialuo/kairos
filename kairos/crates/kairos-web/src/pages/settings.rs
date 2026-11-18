use crate::api::JsonFields as _;
use crate::{
    api::{ApiError, call},
    app::*,
    passkey,
};
use leptos::prelude::*;
use serde_json::{Value, json};

#[component]
pub fn Settings(notice: Notice) -> impl IntoView {
    let settings = RwSignal::new(Value::Null);
    let bark = RwSignal::new(Value::Null);
    let keys = RwSignal::new(Value::Null);
    let tokens = RwSignal::new(Value::Null);
    let shown = RwSignal::new(String::new());
    let queued = RwSignal::new(0_i64);
    reload("/api/admin/settings", settings, notice);
    reload("/api/admin/channel", bark, notice);
    reload("/api/admin/passkeys", keys, notice);
    reload("/api/admin/tokens", tokens, notice);
    let refresh = move || {
        run(notice, async move {
            loop {
                let q = call("GET", "/api/admin/queue", None).await?;
                queued.set(q.field("length").as_i64().unwrap_or(0));
                if queued.get_untracked() == 0 {
                    break;
                }
                gloo_timers::future::TimeoutFuture::new(2000).await;
                if !web_sys::window()
                    .is_some_and(|w| w.location().pathname().ok().as_deref() == Some("/settings"))
                {
                    break;
                }
            }
            Ok(())
        })
    };
    refresh();
    view! {<div class="page-head"><h1>"Settings"</h1></div>
        <div class="settings-grid">
        <section class="panel"><h2>"Reminder defaults"</h2>
        {move||(!settings.get().is_null()).then(||{let s=settings.get();view!{<form class="form-grid" on:submit=move|ev|{
            let Some(d)=form(ev)else{return;};run(notice,async move{
                let body=json!({"timezone":value(&d,"timezone"),"default_lead_min":value(&d,"lead").parse::<u32>().unwrap_or(15),"all_day_time":value(&d,"time")});
                call("PUT","/api/admin/settings",Some(body.clone())).await?;settings.set(body);refresh();notice.set("Defaults saved.".into());Ok(())
            });
        }><label>"Time zone"<input name="timezone" required list="zones" value=text(&s,"timezone")/></label>
        <datalist id="zones">{["UTC","America/Vancouver","America/New_York","Europe/Berlin","Asia/Tokyo","Asia/Shanghai"].into_iter().map(|z|view!{<option value=z/>}).collect_view()}</datalist>
        <label>"Default lead (minutes)"<input name="lead" type="number" min=0 max=10080 required value=s.field("default_lead_min").to_string()/></label>
        <label>"All-day reminder time"<input name="time" type="time" required value=text(&s,"all_day_time")/></label>
        <button class="primary">"Save defaults"</button></form>}})}
        {move||(queued.get()>0).then(||view!{<p role="status">{format!("Recomputing reminders… {} left",queued.get())}</p>})}</section>
        <section class="panel"><h2>"Bark notifications"</h2>
        {move||(!bark.get().is_null()).then(||{let b=bark.get();view!{<form class="form-grid" on:submit=move|ev|{
            let Some(d)=form(ev)else{return;};run(notice,async move{
                bark.set(call("PUT","/api/admin/channel",Some(json!({"server":value(&d,"server"),"device_key":optional(&d,"device_key"),"enabled":d.has("enabled")}))).await?);notice.set("Notification settings saved.".into());Ok(())
            });
        }><label>"Bark server"<input type="url" name="server" required value=text(&b,"server")/></label>
        <label>"Device key"<input type="password" autocomplete="off" name="device_key" placeholder=text(&b,"device_key")/></label>
        <label class="check"><input type="checkbox" name="enabled" checked=*b.field("enabled") ==true/>"Enable Bark"</label>
        <div class="actions"><button class="primary">"Save channel"</button>
        <button type="button" on:click=move|_|run(notice,async move{call("POST","/api/admin/channel/test",Some(json!({}))).await?;notice.set("Test notification sent.".into());Ok(())})>"Send test notification"</button></div></form>}})}</section>
        <section class="panel"><h2>"Passkeys"</h2><div class="row-list">{move||array(&keys.get()).into_iter().map(|k|{
            let id=text(&k,"id");let del=id.clone();let name=text(&k,"name");
            view!{<div class="row"><form class="inline" on:submit=move|ev|{
                let Some(d)=form(ev)else{return;};let id=id.clone();run(notice,async move{call("PATCH",&format!("/api/admin/passkeys/{id}"),Some(json!({"name":value(&d,"name")}))).await?;reload("/api/admin/passkeys",keys,notice);Ok(())});
            }><input aria-label="Passkey name" name="name" value=name required/><button>"Rename"</button></form>
            {(*k.field("stale") ==true).then(||view!{<small>"Previous domain"</small>})}
            <button class="quiet danger" on:click=move|_|{let del=del.clone();run(notice,async move{
                let url=format!("/api/admin/passkeys/{del}");match call("DELETE",&url,None).await{
                    Err(ApiError::Confirm(warning))=>{if confirm(&warning){call("DELETE",&format!("{url}?confirm=1"),None).await?;}},
                    Err(e)=>return Err(e),Ok(_)=>(),
                };reload("/api/admin/passkeys",keys,notice);Ok(())
            })}>"Revoke"</button></div>}
        }).collect_view()}</div>
        <button on:click=move|_|run(notice,async move{passkey::ceremony(true,None).await?;reload("/api/admin/passkeys",keys,notice);Ok(())})>"Add passkey"</button>
        </section>
        <section class="panel"><h2>"API tokens"</h2>
        {move||array(&tokens.get()).into_iter().map(|t|{
            let id=t.field("id").as_i64().unwrap_or_default();view!{<div class="row"><span>{text(&t,"name")}<small>{text(&t,"prefix")}</small></span><button class="quiet danger" on:click=move|_|{
                if confirm("Revoke this API token?"){run(notice,async move{call("DELETE",&format!("/api/admin/tokens/{id}"),None).await?;reload("/api/admin/tokens",tokens,notice);Ok(())})}
            }>"Revoke"</button></div>}
        }).collect_view()}
        <form class="inline" on:submit=move|ev|{let Some(d)=form(ev)else{return;};run(notice,async move{let result=call("POST","/api/admin/tokens",Some(json!({"name":value(&d,"name")}))).await?;
            shown.set(text(&result,"token"));reload("/api/admin/tokens",tokens,notice);Ok(())});
        }><input name="name" aria-label="Token name" placeholder="Token name" required/><button>"Create token"</button></form>
        {move||(!shown.get().is_empty()).then(||view!{<label>"Copy this token now"<input readonly value=shown.get()/></label>})}
        </section>
        <section class="panel"><h2>"Calendar export"</h2><div class="actions"><a download="kairos.ics" href="/api/admin/export.ics">"Download ICS"</a><a download="kairos.json" href="/api/admin/export.json">"Download JSON"</a></div></section>
        </div>
    }
}
