use crate::api::JsonFields as _;
use crate::{
    api::call,
    app::*,
    calendar_grid::{month_grid, week_of},
};
use jiff::{ToSpan, civil::Date};
use leptos::prelude::*;
use serde_json::{Value, json};

fn today() -> Date {
    let today = js_sys::Date::new_0();
    Date::new(
        today.get_full_year() as i16,
        today.get_month() as i8 + 1,
        today.get_date() as i8,
    )
    .unwrap_or(Date::ZERO)
}
#[component]
pub fn Calendar(notice: Notice) -> impl IntoView {
    let day = RwSignal::new(today());
    let weekly = RwSignal::new(false);
    let data = RwSignal::new(json!({}));
    let selection = RwSignal::new(None::<Value>);
    let version = RwSignal::new(0u32);
    let grid = Memo::new(move |_| {
        if weekly.get() {
            vec![week_of(day.get())]
        } else {
            month_grid(day.get().year(), day.get().month())
        }
    });
    let target = web_sys::window()
        .and_then(|w| w.location().search().ok())
        .and_then(|s| web_sys::UrlSearchParams::new_with_str(&s).ok())
        .and_then(|s| s.get("event"))
        .and_then(|id| id.parse::<i64>().ok());
    let target_key = RwSignal::new(target);
    Effect::new(move |_| {
        version.get();
        let weeks = grid.get();
        let Some(first) = weeks.first().and_then(|w| w.first()).copied() else {
            return;
        };
        let Some(last) = weeks.last().and_then(|w| w.last()).copied() else {
            return;
        };
        // The API accepts instants, and filters by the user's display zone on the server.
        // Pad UTC dates to include the first and last cells in every supported offset.
        let from = first.saturating_sub(1.day());
        let to = last.saturating_add(2.days());
        run(notice, async move {
            let response = call(
                "GET",
                &format!("/api/admin/calendar?from={from}T00:00:00Z&to={to}T00:00:00Z"),
                None,
            )
            .await?;
            if grid.get_untracked() != weeks {
                return Ok(());
            }
            if let Some(id) = target_key.get_untracked()
                && let Some(event) = array(response.field("occurrences"))
                    .into_iter()
                    .find(|v| *v.field("event_id") == id)
            {
                selection.set(Some(event));
                target_key.set(None);
            }
            data.set(response);
            Ok(())
        });
    });
    let shift = move |direction: i64| {
        let d = day.get_untracked();
        let period = if weekly.get_untracked() {
            (direction * 7).days()
        } else {
            direction.months()
        };
        day.set(d.saturating_add(period))
    };
    view! {
        <div class="page-head"><div><h1>"Calendar"</h1></div><a rel="external" class="button primary" href="/events">"Add event"</a></div>
        <div class="calendar-bar"><div class="month-nav"><button class="quiet" aria-label="Previous period" on:click=move|_|shift(-1)>"‹"</button>
          <h2>{move||day.get().strftime("%B %Y").to_string()}</h2>
          <button class="quiet" aria-label="Next period" on:click=move|_|shift(1)>"›"</button>
          <button class="quiet" on:click=move|_|day.set(today())>"Today"</button></div>
          <div class="segmented"><button class:active=move||!weekly.get() on:click=move|_|weekly.set(false)>"Month"</button><button class:active=move||weekly.get() on:click=move|_|weekly.set(true)>"Week"</button></div>
        </div>
        {move||(!array(data.get().field("truncated")).is_empty()).then(||view!{<p class="notice">"Some repeating series show only the occurrences within their expansion limit."</p>})}
        <div class="calendar-grid">
        {["Mon","Tue","Wed","Thu","Fri","Sat","Sun"].into_iter().map(|d|view!{<div class="weekday">{d}</div>}).collect_view()}
        {move||{
            let occurrences=array(data.get().field("occurrences"));
            let typed=occurrences.iter().filter_map(|entry|serde_json::from_value(entry.clone()).ok()).collect::<Vec<_>>();
            let weeks=grid.get();
            let first=weeks.iter().flatten().next().copied().unwrap_or(Date::ZERO);
            let last=weeks.iter().flatten().next_back().copied().unwrap_or(first);
            let mut placement=crate::calendar_grid::place(&typed,first,last);
            let by_key=occurrences.iter().filter_map(|entry|entry.field("occ_key").as_str().map(|key|(key,entry))).collect::<std::collections::HashMap<_,_>>();
            weeks.into_iter().flatten().map(|date|{
                let other=date.month()!=day.get().month()&&!weekly.get();let is_today=date==today();
                let visible=placement.remove(&date).unwrap_or_default().into_iter().filter_map(|e|by_key.get(e.occ_key.as_str()).map(|v|(*v).clone())).collect::<Vec<_>>();
                view!{<div class="day" class:muted=other class:today=is_today>
                    <span class="day-number">{date.day()}</span>{visible.into_iter().map(|e|{
                        let title=text(&e,"title");let color=e.field("color").as_str().unwrap_or("#53745e").to_owned();
                        let time=if *e.field("all_day") ==true{String::new()}else{text(&e,"start_local").get(11..16).unwrap_or("").to_owned()};
                        view!{<button class="event-chip" style:border-left-color=color on:click=move|_|selection.set(Some(e.clone()))>
                            <small>{time}</small>{title}</button>}
                    }).collect_view()}</div>}
            }).collect_view()
        }}</div>
        {move||selection.get().map(|e|{
            let key=e.field("event_id").as_i64().unwrap_or_default();let slot=text(&e,"slot");let title=text(&e,"title");let edit=*e.field("editable") ==true;
            view!{<aside aria-label="Event details" class="drawer">
                <button class="quiet close" aria-label="Close details" on:click=move|_|selection.set(None)>"×"</button>
                <h2>{title}</h2><p>{text(&e,"start_local").replace('T'," ")}</p><p>{text(&e,"location")}</p><p>{text(&e,"source")}</p>
                <h3>"Reminders"</h3>
                {if array(e.field("reminders")).is_empty(){view!{<p>"No reminders scheduled."</p>}.into_any()}else{array(e.field("reminders")).into_iter().map(|r|view!{<p>{text(&r,"status")}</p>}).collect_view().into_any()}}
                <div class="actions">{edit.then(||view!{<a rel="external" class="button" href=format!("/events?event={key}")>"Edit series"</a>})}
                {(edit&&slot!="single").then(||view!{<button class="quiet" on:click=move|_|{
                    if !confirm("Delete this occurrence?"){return;} let slot=slot.clone();run(notice,async move{call("POST",&format!("/api/admin/events/{key}/skip"),Some(json!({"slot":slot}))).await?;selection.set(None);version.update(|n|*n+=1);Ok(())});
                }>"Delete this occurrence"</button>})}
                {(*e.field("deletable") ==true).then(||view!{<button class="quiet danger" on:click=move|_|{
                    if confirm("Delete this event and its scheduled reminders?"){run(notice,async move{call("DELETE",&format!("/api/admin/events/{key}"),None).await?;selection.set(None);version.update(|n|*n+=1);Ok(())});}
                }>"Delete event"</button>})}</div>
            </aside>}
        })}
    }
}
