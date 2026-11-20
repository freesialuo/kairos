use kairos_core::{
    auth::tokens::{create_token, list_tokens, revoke_token},
    db::settings::{Settings, put_settings},
    http::{Method, Request, Response},
    sched::queue::{SeriesFilter, enqueue_where, process_queue},
};
use kairos_testkit::{TestHub, ms};
use serde_json::{Value, json};

async fn request(h: &TestHub, token: &str, method: Method, target: &str, body: Value) -> Response {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    h.handle(Request {
        method,
        path: path.into(),
        query: form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect(),
        headers: vec![("aUtHoRiZaTiOn".into(), format!("Bearer {token}"))],
        body: serde_json::to_vec(&body).unwrap(),
    })
    .await
}

fn body(response: &Response, status: u16) -> Value {
    assert_eq!(
        response.status,
        status,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    serde_json::from_slice(&response.body).unwrap()
}

fn utc(s: &str) -> Value {
    json!({"title": "Dentist", "start": s, "reminders": [0]})
}

#[tokio::test]
async fn put_creates_then_unchanged_put_is_noop() {
    let h = TestHub::new();
    let (_, token) = create_token(&h, "Script").unwrap();
    let input = utc("2026-11-21T09:00:00Z");
    let first = request(
        &h,
        &token,
        Method::Put,
        "/api/v1/events/room%3A2",
        input.clone(),
    )
    .await;
    let first = body(&first, 200);
    assert_eq!(first["created"], true);
    assert_eq!(first["reminders_scheduled"], 1);
    assert_eq!(
        h.sql("SELECT external_id FROM events")[0]
            .text("external_id")
            .unwrap(),
        "room:2"
    );
    let before = h.sql("SELECT total_changes() AS n");
    let second = request(&h, &token, Method::Put, "/api/v1/events/room:2", input).await;
    let second = body(&second, 200);
    assert_eq!(
        second,
        json!({"id": first["id"], "created": false, "reminders_scheduled": 1})
    );
    assert_eq!(h.sql("SELECT total_changes() AS n"), before);
}

#[tokio::test]
async fn put_and_delete_report_committed_changes_when_alarm_rearm_fails() {
    let h = TestHub::new();
    let (_, token) = create_token(&h, "Script").unwrap();
    let path = "/api/v1/events/checkup";
    h.alarm.fail_next_set();
    let response = request(&h, &token, Method::Put, path, utc("2026-11-21T09:00:00Z")).await;
    let response = body(&response, 200);
    assert_eq!(response["created"], true);
    assert_eq!(response["reminders_scheduled"], 1);
    assert_eq!(
        h.sql("SELECT id FROM events")[0].i64("id").unwrap(),
        response["id"].as_i64().unwrap()
    );

    h.alarm.fail_next_set();
    let response = request(&h, &token, Method::Delete, path, Value::Null).await;
    assert_eq!(response.status, 204);
    assert!(h.sql("SELECT id FROM events").is_empty());
    assert!(h.sql("SELECT key FROM reminders").is_empty());
}

#[tokio::test]
async fn put_is_full_replacement_and_empty_reminders_withdraw() {
    let h = TestHub::new();
    let (_, token) = create_token(&h, "Script").unwrap();
    let path = "/api/v1/events/replacement";
    let first = request(
        &h,
        &token,
        Method::Put,
        path,
        json!({
            "title": "Dentist", "start": "2026-11-21T09:00:00", "location": "Clinic"
        }),
    )
    .await;
    assert_eq!(body(&first, 200)["reminders_scheduled"], 1);
    let second = request(
        &h,
        &token,
        Method::Put,
        path,
        json!({
            "title": "Dentist", "start": "2026-11-21T09:00:00", "reminders": []
        }),
    )
    .await;
    assert_eq!(body(&second, 200)["reminders_scheduled"], 0);
    let rows = h.sql("SELECT location,lead_minutes FROM events");
    assert_eq!(rows[0].opt_text("location").unwrap(), None);
    assert_eq!(rows[0].text("lead_minutes").unwrap(), "[]");
    assert!(
        h.sql("SELECT key FROM reminders WHERE status='pending'")
            .is_empty()
    );
}

#[tokio::test]
async fn explicit_offset_survives_user_zone_change() {
    let h = TestHub::new();
    h.set_now("2026-11-20T00:00:00Z");
    let (_, token) = create_token(&h, "Script").unwrap();
    put_settings(
        &h.db,
        &Settings {
            timezone: "Europe/Berlin".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let response = request(
        &h,
        &token,
        Method::Put,
        "/api/v1/events/offset",
        utc("2026-11-20T09:00:00+08:00"),
    )
    .await;
    assert_eq!(body(&response, 200)["reminders_scheduled"], 1);
    let rows = h.sql("SELECT start_local,tzid FROM events");
    assert_eq!(rows[0].text("start_local").unwrap(), "2026-11-20T02:00:00");
    assert_eq!(rows[0].text("tzid").unwrap(), "Europe/Berlin");
    let at = ms("2026-11-20T01:00:00Z");
    assert_eq!(
        h.sql("SELECT fire_at FROM reminders")[0]
            .i64("fire_at")
            .unwrap(),
        at
    );
    put_settings(
        &h.db,
        &Settings {
            timezone: "Asia/Tokyo".into(),
            ..Default::default()
        },
    )
    .unwrap();
    enqueue_where(&h.db, SeriesFilter::All, "settings", h.now()).unwrap();
    assert!(process_queue(&h, h.now()).await.step_error.is_none());
    assert_eq!(
        h.sql("SELECT fire_at FROM reminders")[0]
            .i64("fire_at")
            .unwrap(),
        at
    );
}

#[tokio::test]
async fn explicit_later_overlap_keeps_initial_start_and_later_slots_use_zone() {
    let h = TestHub::new();
    h.set_now("2026-11-01T05:00:00Z");
    let (_, token) = create_token(&h, "Script").unwrap();
    let response = request(
        &h,
        &token,
        Method::Put,
        "/api/v1/events/fold",
        json!({
            "title": "Fold", "start": "2026-11-01T01:30:00-05:00", "timezone": "America/New_York",
            "rrule": "FREQ=MINUTELY;COUNT=2", "reminders": [0]
        }),
    )
    .await;
    assert_eq!(body(&response, 200)["reminders_scheduled"], 2);
    let rows = h.sql("SELECT start_local,tzid,initial_start_utc FROM events");
    assert_eq!(rows[0].text("start_local").unwrap(), "2026-11-01T01:30:00");
    assert_eq!(rows[0].text("tzid").unwrap(), "America/New_York");
    assert_eq!(
        rows[0].i64("initial_start_utc").unwrap(),
        ms("2026-11-01T06:30:00Z")
    );
    let fires: Vec<_> = h
        .sql("SELECT fire_at FROM reminders ORDER BY fire_at")
        .iter()
        .map(|row| row.i64("fire_at").unwrap())
        .collect();
    assert_eq!(
        fires,
        [ms("2026-11-01T05:31:00Z"), ms("2026-11-01T06:30:00Z")]
    );
    let response = request(
        &h,
        &token,
        Method::Get,
        "/api/v1/events?from=2026-11-01T05:00:00Z&to=2026-11-01T06:00:00Z",
        Value::Null,
    )
    .await;
    let response = body(&response, 200);
    let items = response["occurrences"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["start_utc"], fires[0]);
}

#[tokio::test]
async fn validation_fields_and_range_limits() {
    let h = TestHub::new();
    let (_, token) = create_token(&h, "Script").unwrap();
    let path = "/api/v1/events/probe";
    for (field, value) in [
        ("exdates", json!(["2026-11-21T09:00:00Z"])),
        ("title", json!("é".repeat(201))),
        ("location", json!("a".repeat(2_001))),
        ("description", json!("b".repeat(2_001))),
        ("reminders", json!([0, 1, 2, 3, 4, 5])),
        ("reminders", json!([10_081])),
        ("timezone", json!("Not a zone")),
        ("start", json!("2026-02-30")),
        ("end", json!("2026-11-20T00:00:00Z")),
    ] {
        let mut input = utc("2026-11-21T09:00:00Z");
        input[field] = value;
        let result = request(&h, &token, Method::Put, path, input).await;
        assert_eq!(
            body(&result, 400),
            json!({"error": "invalid", "field": field})
        );
    }
    let mut input = utc("2026-11-21T09:00:00Z");
    input["unknown"] = json!(true);
    let unknown = request(&h, &token, Method::Put, path, input).await;
    assert_eq!(body(&unknown, 400)["field"], "body");
    let bad_id = request(
        &h,
        &token,
        Method::Put,
        "/api/v1/events/a%2Fb",
        utc("2026-11-21T09:00:00Z"),
    )
    .await;
    assert_eq!(body(&bad_id, 400)["field"], "external_id");
    let wide = request(
        &h,
        &token,
        Method::Get,
        "/api/v1/events?from=2026-11-01&to=2027-02-04",
        Value::Null,
    )
    .await;
    assert_eq!(body(&wide, 400)["field"], "to");
    assert!(h.sql("SELECT id FROM events").is_empty());
}

#[tokio::test]
async fn list_expands_in_order_reports_truncation_and_hides_disabled_sources() {
    let h = TestHub::new();
    let (_, token) = create_token(&h, "Script").unwrap();
    for (key, input) in [
        ("late", utc("2026-11-24T09:00:00Z")),
        (
            "dense",
            json!({"title": "Dense", "start": "2026-11-21T09:00:00Z", "rrule": "FREQ=MINUTELY;COUNT=2001", "reminders": []}),
        ),
        ("early", utc("2026-11-20T09:00:00Z")),
    ] {
        let response = request(
            &h,
            &token,
            Method::Put,
            &format!("/api/v1/events/{key}"),
            input,
        )
        .await;
        assert_eq!(response.status, 200);
    }
    h.sql("INSERT INTO sources(id,name,url,color,enabled) VALUES(72,'Disabled','https://test.invalid','#ffffff',0)");
    h.sql("UPDATE events SET origin='ics',source_id=72,external_id=NULL,series_key='ics:72:late' WHERE external_id='late'");
    let response = request(
        &h,
        &token,
        Method::Get,
        "/api/v1/events?from=2026-11-20&to=2026-11-25",
        Value::Null,
    )
    .await;
    let result = body(&response, 200);
    assert_eq!(result["truncated"], json!(["api:dense"]));
    let items = result["occurrences"].as_array().unwrap();
    assert_eq!(items.first().unwrap()["external_id"], "early");
    assert_eq!(items.len(), 2_001);
    assert!(
        items
            .windows(2)
            .all(|pair| pair[0]["start_utc"].as_i64() <= pair[1]["start_utc"].as_i64())
    );
    assert!(items.iter().all(|item| item["origin"] == "api"));
}

#[tokio::test]
async fn delete_withdraws_api_reminders_and_returns_404_for_missing() {
    let h = TestHub::new();
    let (_, token) = create_token(&h, "Script").unwrap();
    let path = "/api/v1/events/remove";
    assert_eq!(
        request(&h, &token, Method::Put, path, utc("2026-11-21T09:00:00Z"))
            .await
            .status,
        200
    );
    assert_eq!(
        h.sql("SELECT key FROM reminders WHERE status='pending'")
            .len(),
        1
    );
    let response = request(&h, &token, Method::Delete, path, Value::Null).await;
    assert_eq!(response.status, 204);
    assert!(response.body.is_empty());
    assert!(
        h.sql("SELECT key FROM reminders WHERE status='pending'")
            .is_empty()
    );
    assert_eq!(
        request(&h, &token, Method::Delete, path, Value::Null)
            .await
            .status,
        404
    );
}

#[tokio::test]
async fn token_use_is_throttled_and_revocation_is_immediate() {
    let h = TestHub::new();
    let (id, token) = create_token(&h, "Script").unwrap();
    let row = &h.sql("SELECT name,hash,prefix FROM api_tokens")[0];
    assert_eq!(row.text("prefix").unwrap(), token[..10]);
    assert_ne!(row.text("hash").unwrap(), token);
    assert_eq!(list_tokens(&h.db).unwrap()[0].last_used_at, None);
    let target = "/api/v1/events?from=2026-11-20&to=2026-11-25";
    let first = h.now();
    assert_eq!(
        request(&h, &token, Method::Get, target, Value::Null)
            .await
            .status,
        200
    );
    h.advance_min(59);
    assert_eq!(
        request(&h, &token, Method::Get, target, Value::Null)
            .await
            .status,
        200
    );
    assert_eq!(list_tokens(&h.db).unwrap()[0].last_used_at, Some(first));
    h.advance_min(1);
    assert_eq!(
        request(&h, &token, Method::Get, target, Value::Null)
            .await
            .status,
        200
    );
    assert_eq!(list_tokens(&h.db).unwrap()[0].last_used_at, Some(h.now()));
    revoke_token(&h.db, id).unwrap();
    assert_eq!(
        request(&h, &token, Method::Get, target, Value::Null)
            .await
            .status,
        401
    );
}

#[tokio::test]
async fn all_day_occurrences_follow_the_user_zone_and_honor_exdates() {
    let h = TestHub::new();
    let (_, token) = create_token(&h, "Script").unwrap();
    put_settings(
        &h.db,
        &Settings {
            timezone: "Asia/Tokyo".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let put = request(
        &h,
        &token,
        Method::Put,
        "/api/v1/events/holiday",
        json!({
            "title": "Holiday", "start": "2026-11-21", "end": "2026-11-23",
            "timezone": "America/New_York", "rrule": "FREQ=DAILY;COUNT=3",
            "exdates": ["2026-11-22"], "reminders": []
        }),
    )
    .await;
    body(&put, 200);
    let list = request(
        &h,
        &token,
        Method::Get,
        "/api/v1/events?from=2026-11-21&to=2026-11-25",
        Value::Null,
    )
    .await;
    let list = body(&list, 200);
    let items = list["occurrences"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["start_local"], "2026-11-21");
    assert_eq!(items[0]["end_local"], "2026-11-23");
    assert_eq!(items[0]["start_utc"], ms("2026-11-20T15:00:00Z"));
    assert_eq!(items[1]["start_local"], "2026-11-23");
    assert_eq!(items[1]["all_day"], true);
}
