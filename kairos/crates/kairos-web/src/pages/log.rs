use crate::api::JsonFields as _;
use crate::{api::call, app::*};
use leptos::prelude::*;
use serde_json::Value;

#[component]
pub fn Log(notice: Notice) -> impl IntoView {
    let data = RwSignal::new(Value::Null);
    let refresh = move || {
        run(notice, async move {
            data.set(call("GET", "/api/admin/log", None).await?);
            Ok(())
        })
    };
    refresh();
    view! {<div class="page-head"><h1>"Delivery log"</h1><button on:click=move|_|refresh()>"Refresh"</button></div>
        {move||{
            let d=data.get(); let areas=[("failed_reminders","Failed reminders"),("deliveries","Recent deliveries"),("failing_sources","Failing sources"),("expansion_warnings","Recurrence warnings")];
            areas.into_iter().map(|(key,label)|{
                let items=array(d.field(key));view!{<section class="panel"><h2>{label}</h2>
                    {if items.is_empty(){view!{<p class="muted-text">"No entries."</p>}.into_any()}else{
                        items.into_iter().map(|i|{
                            let label=["title","name","reminder_key","key","kind"].iter().find_map(|k|i.field(k).as_str()).unwrap_or_default().to_string();
                            let detail=["error","last_error","expand_warning","summary"].iter().find_map(|k|i.field(k).as_str()).unwrap_or_default().to_string();
                            let msg=if *i.field("kind") =="reminder" && *i.field("ok") ==1 {format!("Sent · {detail}")}else{detail};
                            let time=i.field("at").as_i64().and_then(|t|jiff::Timestamp::from_millisecond(t).ok()).map(|t|t.strftime("%Y-%m-%d %H:%M UTC").to_string());
                            view!{<div class="row"><div><strong>{label}</strong><p>{msg}</p></div><small>{time}</small></div>}
                        }).collect_view().into_any()
                    }}</section>}
            }).collect_view()
        }}
        <section class="panel"><h2>"Background activity"</h2>{move||{
            let d=data.get();["last_auth_error","last_alarm_error","last_maintenance_error","alarm_error_streak","last_watchdog_at","last_maintenance_at"].into_iter().filter_map(|key|{
                let val=d.field("background").field(key).as_str()?;let v=if key.ends_with("_at"){val.parse().ok().and_then(|t|jiff::Timestamp::from_millisecond(t).ok()).map(|t|t.strftime("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_else(||val.into())}else{val.into()};
                Some(view!{<p>{key.replace('_'," ")}" · "{v}</p>})}).collect_view()
        }}</section>
    }
}
