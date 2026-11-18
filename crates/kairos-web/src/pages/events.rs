use crate::api::JsonFields as _;
use jiff::{
    SignedDuration, Span,
    civil::{Date, DateTime},
};
use kairos_types::{
    admin::{ManualEventInput, ReminderChoice},
    limits::{MAX_REMINDER_MINUTES, MAX_REMINDERS},
    repeat::{End, Frequency, RepeatPreset, from_rrule},
};
use leptos::prelude::*;
use serde_json::Value;
use web_sys::FormData;

use crate::{
    api::{ApiError, call},
    app::{Notice, array, confirm, form, optional, run, text, value},
};

const EVENTS: &str = "/api/admin/events";
const DAYS: [(&str, &str); 7] = [
    ("MO", "Mon"),
    ("TU", "Tue"),
    ("WE", "Wed"),
    ("TH", "Thu"),
    ("FR", "Fri"),
    ("SA", "Sat"),
    ("SU", "Sun"),
];

#[component]
pub fn Events(notice: Notice) -> impl IntoView {
    let events = RwSignal::new(Value::Null);
    let editor = RwSignal::new(None::<Value>);
    let busy = RwSignal::new(false);
    let requested = linked_event();
    run(notice, async move {
        let rows = call("GET", EVENTS, None).await?;
        if let Some(id) = requested {
            if let Some(event) = array(&rows)
                .into_iter()
                .find(|event| event.field("id").as_i64() == Some(id))
            {
                editor.set(Some(event));
            } else {
                notice.set("This event is no longer available.".into());
            }
        }
        events.set(rows);
        Ok(())
    });

    view! {
        <section>
            <div class="page-head">
                <h1>"Events"</h1>
                <button type="button" class="primary" disabled=move || busy.get()
                    on:click=move |_| editor.set(Some(Value::Null))>"Add event"</button>
            </div>
            {move || editor.get().map(|entry| view! {
                <EventEditor entry=entry events=events editor=editor busy=busy notice=notice/>
            })}
            {move || {
                let rows = events.get();
                if rows.is_null() {
                    return view! { <p class="loading">"Loading events…"</p> }.into_any();
                }
                if rows.as_array().is_some_and(Vec::is_empty) {
                    return view! { <p class="panel">"Events you add here appear on your calendar."</p> }.into_any();
                }
                view! {
                    <ul class="row-list">
                        {array(&rows).into_iter().map(|entry| view! {
                            <EventItem entry=entry events=events editor=editor busy=busy notice=notice/>
                        }).collect_view()}
                    </ul>
                }.into_any()
            }}
        </section>
    }
}

fn linked_event() -> Option<i64> {
    let search = web_sys::window()?.location().search().ok()?;
    web_sys::UrlSearchParams::new_with_str(&search)
        .ok()?
        .get("event")?
        .parse()
        .ok()
}

fn close_editor(editor: RwSignal<Option<Value>>) {
    editor.set(None);
    if linked_event().is_some()
        && let Some(history) = web_sys::window().and_then(|window| window.history().ok())
    {
        let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some("/events"));
    }
}

#[component]
fn EventItem(
    entry: Value,
    events: RwSignal<Value>,
    editor: RwSignal<Option<Value>>,
    busy: RwSignal<bool>,
    notice: Notice,
) -> impl IntoView {
    let event = entry.field("event");
    let title = text(event, "title");
    let start = text(event, "start_local").replace('T', " ");
    let repeating = event.field("rrule").is_string();
    let id = entry.field("id").as_i64().unwrap_or_default();
    let prompt = if repeating {
        format!("Delete the entire repeating event {title}?")
    } else {
        format!("Delete {title}?")
    };
    view! {
        <li class="panel split">
            <div>
                <h2>{title}</h2>
                <p>{start}</p>
                {repeating.then(||view! { <p>"Repeats"</p> })}
            </div>
            <div class="actions">
                <button type="button" class="quiet" disabled=move || busy.get()
                    on:click=move |_| editor.set(Some(entry.clone()))>
                    {if repeating { "Edit series" } else { "Edit" }}
                </button>
                <button type="button" class="quiet" disabled=move || busy.get() on:click=move |_| {
                    if busy.get_untracked() || !confirm(&prompt) { return; }
                    busy.set(true);
                    run(notice, async move {
                        let result = async {
                            call("DELETE", &format!("{EVENTS}/{id}"), None).await?;
                            if editor.get_untracked().is_some_and(|row| row.field("id").as_i64() == Some(id)) {
                                close_editor(editor);
                            }
                            events.set(call("GET", EVENTS, None).await?);
                            Ok(())
                        }.await;
                        busy.set(false);
                        result
                    });
                }>{if repeating { "Delete series" } else { "Delete" }}</button>
            </div>
        </li>
    }
}

#[component]
fn EventEditor(
    entry: Value,
    events: RwSignal<Value>,
    editor: RwSignal<Option<Value>>,
    busy: RwSignal<bool>,
    notice: Notice,
) -> impl IntoView {
    let id = entry.field("id").as_i64();
    let event = entry.field("event");
    let start = text(event, "start_local");
    let timed_end = entry
        .field("end_local")
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| fallback_end(&start, event.field("duration_sec").as_i64().unwrap_or(0)));
    let preset = event
        .field("rrule")
        .as_str()
        .and_then(|rule| from_rrule(rule, &start));
    let unsupported = event.field("rrule").is_string() && preset.is_none();
    let all_day = RwSignal::new(event.field("all_day").as_bool().unwrap_or(false));
    let start = RwSignal::new(start);
    let end = RwSignal::new(timed_end);
    let reminder_leads = event.field("lead_minutes").as_array().map(|leads| {
        leads
            .iter()
            .filter_map(Value::as_u64)
            .map(|lead| lead.to_string())
            .collect::<Vec<_>>()
    });

    let submit = move |event| {
        let Some(data) = form(event) else { return };
        if busy.get_untracked() || unsupported {
            return;
        }
        let body = match event_payload(&data) {
            Ok(body) => body,
            Err(error) => {
                notice.set(error);
                return;
            }
        };
        let method = if id.is_some() { "PATCH" } else { "POST" };
        let path = id.map_or_else(|| EVENTS.to_owned(), |id| format!("{EVENTS}/{id}"));
        busy.set(true);
        run(notice, async move {
            let result = async {
                let body = serde_json::to_value(body)
                    .map_err(|error| ApiError::Other(error.to_string()))?;
                call(method, &path, Some(body)).await?;
                close_editor(editor);
                events.set(call("GET", EVENTS, None).await?);
                Ok(())
            }
            .await;
            busy.set(false);
            result
        });
    };

    view! {
        <form class="panel" on:submit=submit>
            <h2>{if id.is_some() { "Edit event" } else { "Add event" }}</h2>
            {unsupported.then(||view! { <p class="error">"This repeat rule cannot be edited here."</p> })}
            <div class="form-grid">
                <label>"Title"<input required name="title" maxlength=200 value=text(event, "title")/></label>
                <label class="check"><input type="checkbox" name="all_day" prop:checked=move || all_day.get()
                    on:change=move |ev| all_day.set(event_target_checked(&ev))/>"All day"</label>
                <label>"Starts"
                    <input required name="start" type=move || if all_day.get() { "date" } else { "datetime-local" }
                        step=1
                        prop:value=move || input_wall(&start.get(), all_day.get())
                        on:input=move |ev| start.set(event_target_value(&ev))/>
                </label>
                <label>{move || if all_day.get() { "Ends (exclusive)" } else { "Ends" }}
                    <input name="end" type=move || if all_day.get() { "date" } else { "datetime-local" }
                        step=1 prop:value=move || input_wall(&end.get(), all_day.get())
                        on:input=move |ev| end.set(event_target_value(&ev))/>
                </label>
                <label>"Time zone"<input name="timezone" placeholder="Use calendar time zone" value=text(event, "tzid")/></label>
                <label>"Location"<input name="location" maxlength=2000 value=text(event, "location")/></label>
                <label>"Description"<textarea name="description" maxlength=2000 prop:value=text(event, "description")></textarea></label>
            </div>
            <RepeatEditor preset=preset/>
            <ReminderEditor leads=reminder_leads/>
            <div class="actions">
                <button type="submit" class="primary" disabled=move || busy.get() || unsupported>"Save event"</button>
                <button type="button" class="quiet" disabled=move || busy.get() on:click=move |_| close_editor(editor)>"Cancel"</button>
            </div>
        </form>
    }
}

fn fallback_end(start: &str, duration: i64) -> String {
    if let Ok(date) = start.parse::<Date>()
        && start.len() == 10
    {
        return Span::new()
            .try_days(duration / 86_400)
            .ok()
            .and_then(|days| date.checked_add(days).ok())
            .map_or_else(String::new, |end| end.to_string());
    }
    start
        .parse::<DateTime>()
        .ok()
        .and_then(|wall| wall.checked_add(SignedDuration::from_secs(duration)).ok())
        .map_or_else(String::new, |end| end.to_string())
}

fn input_wall(wall: &str, all_day: bool) -> String {
    if all_day {
        return wall.get(..10).unwrap_or(wall).into();
    }
    if wall.len() == 10 {
        format!("{wall}T00:00:00")
    } else {
        wall.into()
    }
}

fn canonical_wall(wall: String, all_day: bool) -> String {
    if !all_day && wall.len() == 16 {
        format!("{wall}:00")
    } else {
        wall
    }
}

fn event_payload(form: &FormData) -> Result<ManualEventInput, String> {
    let all_day = form.has("all_day");
    let reminders = match value(form, "reminders").as_str() {
        "none" => ReminderChoice::None,
        "custom" => {
            let raw = value(form, "leads");
            let leads = raw
                .split(',')
                .map(|item| item.trim().parse::<u32>())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "Enter reminder minutes separated by commas.".to_owned())?;
            if leads.len() > MAX_REMINDERS || leads.iter().any(|lead| *lead > MAX_REMINDER_MINUTES)
            {
                return Err("Use up to five reminder leads from 0 to 10,080 minutes.".into());
            }
            ReminderChoice::Custom(leads)
        }
        _ => ReminderChoice::Default,
    };
    Ok(ManualEventInput {
        title: value(form, "title").trim().into(),
        start: canonical_wall(value(form, "start"), all_day),
        end: optional(form, "end").map(|end| canonical_wall(end, all_day)),
        all_day,
        timezone: optional(form, "timezone")
            .map(|zone| zone.trim().to_owned())
            .filter(|zone| !zone.is_empty()),
        location: optional(form, "location"),
        description: optional(form, "description"),
        repeat: repeat_preset(form)?,
        reminders,
    })
}

fn repeat_preset(form: &FormData) -> Result<Option<RepeatPreset>, String> {
    let freq = match value(form, "repeat").as_str() {
        "daily" => Frequency::Daily,
        "weekly" => {
            let days = DAYS
                .iter()
                .filter(|(day, _)| form.has(day))
                .map(|(day, _)| (*day).into())
                .collect::<Vec<_>>();
            if days.is_empty() {
                return Err("Choose at least one weekday.".into());
            }
            Frequency::Weekly { days }
        }
        "monthly" => Frequency::Monthly,
        "yearly" => Frequency::Yearly,
        _ => return Ok(None),
    };
    let interval = value(form, "interval")
        .parse::<u8>()
        .ok()
        .filter(|n| (1..=99).contains(n))
        .ok_or_else(|| "Choose an interval from 1 to 99.".to_owned())?;
    let end = match value(form, "repeat_end").as_str() {
        "until" => {
            End::Until(optional(form, "until").ok_or_else(|| "Choose an end date.".to_owned())?)
        }
        "count" => End::Count(
            value(form, "count")
                .parse::<u16>()
                .ok()
                .filter(|n| (1..=999).contains(n))
                .ok_or_else(|| "Choose a repeat count from 1 to 999.".to_owned())?,
        ),
        _ => End::Never,
    };
    Ok(Some(RepeatPreset {
        freq,
        interval,
        end,
    }))
}

#[component]
fn RepeatEditor(preset: Option<RepeatPreset>) -> impl IntoView {
    let (frequency, days) = match preset.as_ref().map(|p| &p.freq) {
        Some(Frequency::Daily) => ("daily", &[][..]),
        Some(Frequency::Weekly { days }) => ("weekly", days.as_slice()),
        Some(Frequency::Monthly) => ("monthly", &[][..]),
        Some(Frequency::Yearly) => ("yearly", &[][..]),
        None => ("none", &[][..]),
    };
    let (ending, date, count) = match preset.as_ref().map(|p| &p.end) {
        Some(End::Count(n)) => ("count", "", n.to_string()),
        Some(End::Until(date)) => ("until", date.as_str(), "10".into()),
        _ => ("never", "", "10".into()),
    };
    let repeat = RwSignal::new(frequency.to_owned());
    let end = RwSignal::new(ending.to_owned());
    let interval = preset.as_ref().map_or(1, |p| p.interval);
    let date = date.to_owned();
    view! {
        <fieldset>
            <legend>"Repeat"</legend>
            <div class="form-grid">
                <label>"Frequency"
                    <select name="repeat" on:change=move |ev| repeat.set(event_target_value(&ev))>
                        <option value="none" selected=frequency == "none">"Does not repeat"</option>
                        <option value="daily" selected=frequency == "daily">"Daily"</option>
                        <option value="weekly" selected=frequency == "weekly">"Weekly"</option>
                        <option value="monthly" selected=frequency == "monthly">"Monthly"</option>
                        <option value="yearly" selected=frequency == "yearly">"Yearly"</option>
                    </select>
                </label>
                <label hidden=move || repeat.get() == "none">"Repeat every"
                    <input name="interval" min=1 max=99 type="number" value=interval required
                        disabled=move || repeat.get() == "none"/>
                </label>
                <label hidden=move || repeat.get() == "none">"Ends"
                    <select name="repeat_end" on:change=move |ev| end.set(event_target_value(&ev))>
                        <option value="never" selected=ending == "never">"Never"</option>
                        <option value="until" selected=ending == "until">"On a date"</option>
                        <option value="count" selected=ending == "count">"After a number of times"</option>
                    </select>
                </label>
                <label hidden=move || repeat.get() == "none" || end.get() != "until">"Last day"
                    <input name="until" type="date" value=date required
                        disabled=move || repeat.get() == "none" || end.get() != "until"/>
                </label>
                <label hidden=move || repeat.get() == "none" || end.get() != "count">"Number of occurrences"
                    <input name="count" type="number" min=1 max=999 value=count required
                        disabled=move || repeat.get() == "none" || end.get() != "count"/>
                </label>
            </div>
            <fieldset hidden=move || repeat.get() != "weekly">
                <legend>"Days of the week"</legend>
                <div class="actions">
                    {DAYS.into_iter().map(|(day, label)| view! {
                        <label class="check"><input name=day type="checkbox" prop:checked=days.iter().any(|selected| selected == day)/>{label}</label>
                    }).collect_view()}
                </div>
            </fieldset>
        </fieldset>
    }
}

#[component]
fn ReminderEditor(leads: Option<Vec<String>>) -> impl IntoView {
    let kind = match leads.as_ref() {
        None => "default",
        Some(leads) if leads.is_empty() => "none",
        Some(_) => "custom",
    };
    let mode = RwSignal::new(kind.to_owned());
    let minutes = leads.map_or_else(String::new, |leads| leads.join(", "));
    view! {
        <fieldset>
            <legend>"Reminders"</legend>
            <div class="form-grid">
                <label>"Reminder schedule"
                    <select name="reminders" on:change=move |ev| mode.set(event_target_value(&ev))>
                        <option value="default" selected=kind == "default">"Default"</option>
                        <option value="custom" selected=kind == "custom">"Custom"</option>
                        <option value="none" selected=kind == "none">"No reminders"</option>
                    </select>
                </label>
                <label hidden=move || mode.get() != "custom">"Minutes before the event"
                    <input name="leads" value=minutes placeholder="30, 10" required=move || mode.get() == "custom"/>
                </label>
            </div>
        </fieldset>
    }
}
