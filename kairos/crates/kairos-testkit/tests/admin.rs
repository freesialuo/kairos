use kairos_core::{
    Db,
    calendar::events::upsert_event,
    db::settings::{get_settings, put_settings},
    ics::parse::parse_ics,
    ports::Value,
    sched::queue::process_queue,
};
use kairos_testkit::TestHub;
use serde_json::{Value as Json, json};

async fn send(
    h: &TestHub,
    c: &str,
    m: &str,
    path: &str,
    body: Option<Json>,
) -> kairos_testkit::TestResponse {
    h.request(m, &format!("/api/admin/{path}"), body, &[c])
        .await
}

#[tokio::test]
async fn management_requires_a_full_session_and_export_excludes_credentials() {
    let h = TestHub::new();
    for route in ["settings", "log", "sources", "export.json", "export.ics"] {
        assert_eq!(
            h.request("GET", &format!("/api/admin/{route}"), None, &[])
                .await
                .status,
            401
        );
    }
    let c = h.admin_session().await;
    let channel =
        json!({"server":"https://bark.test","device_key":"private-device-key","enabled":true});
    assert_eq!(
        send(&h, &c, "PUT", "channel", Some(channel)).await.status,
        200
    );
    h.http()
        .respond("https://bark.test/push", 200, &[], br#"{"code":200}"#);
    assert_eq!(
        send(&h, &c, "POST", "channel/test", Some(json!({})))
            .await
            .status,
        200
    );
    let token = send(&h, &c, "POST", "tokens", Some(json!({"name":"script"})))
        .await
        .json();
    let secret = token["token"].as_str().unwrap();
    assert!(secret.starts_with("kr_"));
    assert!(
        !send(&h, &c, "GET", "tokens", None)
            .await
            .json()
            .to_string()
            .contains(secret)
    );
    let export = send(&h, &c, "GET", "export.json", None).await.json();
    let serialized = export.to_string();
    assert!(!serialized.contains("private-device-key"));
    assert!(!serialized.contains(secret));
    for k in ["passkeys", "sessions", "tokens"] {
        assert!(export.get(k).is_none());
    }
    let log = send(&h, &c, "GET", "log", None).await.json();
    assert_eq!(log["deliveries"][0]["kind"], "test");
    assert!(log["background"].is_object());
    let keys = send(&h, &c, "GET", "passkeys", None).await.json();
    let path = format!("passkeys/{}", keys[0]["id"].as_str().unwrap());
    assert_eq!(
        send(&h, &c, "PATCH", &path, Some(json!({"name":"Laptop"})))
            .await
            .status,
        204
    );
    assert_eq!(
        send(&h, &c, "DELETE", &path, None).await.json()["error"],
        "confirm_required"
    );
    assert_eq!(
        send(&h, &c, "DELETE", &format!("{path}?confirm=1"), None)
            .await
            .status,
        204
    );
}
#[tokio::test]
async fn manual_series_skip_calendar_and_ics_round_trip() {
    let h = TestHub::new();
    let c = h.admin_session().await;
    let input = json!({"title":"Standup, <b>test</b>","start":"2026-11-23T09:00:00","timezone":"UTC",
        "repeat":{"freq":{"weekly":{"days":["MO"]}},"interval":1,"end":{"count":3}},"reminders":{"custom":[10]}});
    let created = send(&h, &c, "POST", "events", Some(input)).await;
    assert_eq!(created.status, 200, "{:?}", created.json());
    let id = created.json()["id"].as_i64().unwrap();
    assert_eq!(
        send(
            &h,
            &c,
            "POST",
            &format!("events/{id}/skip"),
            Some(json!({"slot":"2026-11-30T09:00:00"}))
        )
        .await
        .status,
        204
    );
    let calendar = send(
        &h,
        &c,
        "GET",
        "calendar?from=2026-11-20T00:00:00Z&to=2026-12-10T00:00:00Z",
        None,
    )
    .await
    .json();
    let occ = calendar["occurrences"].as_array().unwrap();
    assert_eq!(occ.len(), 2);
    assert_eq!(occ[0]["editable"], true);
    assert_eq!(occ[0]["deletable"], true);
    assert_eq!(occ[0]["reminders"][0]["status"], "pending");
    let response = send(&h, &c, "GET", "export.ics", None).await;
    let parsed = parse_ics(&response.body, 1).unwrap();
    assert_eq!(parsed.rows[0].title, "Standup, <b>test</b>");
    assert_eq!(parsed.rows[0].exdates, ["2026-11-30T09:00:00"]);
    assert_eq!(
        send(&h, &c, "GET", "events", None).await.json()[0]["end_local"],
        "2026-11-23T09:00:00"
    );
}
#[tokio::test]
async fn source_switch_withdraws_and_calendar_zone_changes_across_dst() {
    let h = TestHub::new();
    let c = h.admin_session().await;
    let mut s = get_settings(&h.db).unwrap();
    s.timezone = "Asia/Shanghai".into();
    put_settings(&h.db, &s).unwrap();
    let source = send(
        &h,
        &c,
        "POST",
        "sources",
        Some(json!({"name":"Classes","url":"https://feeds.test/work"})),
    )
    .await
    .json()["id"]
        .as_i64()
        .unwrap();
    let feed=b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:work\r\nSUMMARY:Class\r\nDTSTART;TZID=America/New_York:20261019T090000\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let row = parse_ics(feed, source).unwrap().rows.remove(0);
    upsert_event(&h.db, &row, h.now()).unwrap();
    let url = "calendar?from=2026-10-19T00:00:00Z&to=2026-11-15T23:59:59Z";
    let list = send(&h, &c, "GET", url, None).await.json();
    let times: Vec<_> = list["occurrences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| &v["start_local"].as_str().unwrap()[11..16])
        .collect();
    assert_eq!(times, ["21:00", "21:00", "22:00", "22:00"]);
    let address = format!("sources/{source}");
    send(&h, &c, "PATCH", &address, Some(json!({"notify":true}))).await;
    process_queue(&h, h.now()).await;
    assert!(!h.sql("SELECT key FROM reminders").is_empty());
    send(&h, &c, "PATCH", &address, Some(json!({"notify":false}))).await;
    process_queue(&h, h.now()).await;
    assert!(h.sql("SELECT key FROM reminders").is_empty());
    send(&h, &c, "PATCH", &address, Some(json!({"enabled":false}))).await;
    assert_eq!(
        send(&h, &c, "GET", url, None).await.json()["occurrences"],
        json!([])
    );
    // Changing URL discards the old conditional request validators.
    h.db.exec(
        "UPDATE sources SET etag='old',last_modified='yesterday' WHERE id=?1",
        &[Value::Integer(source)],
    )
    .unwrap();
    send(
        &h,
        &c,
        "PATCH",
        &address,
        Some(json!({"url":"https://example.org/new"})),
    )
    .await;
    assert!(
        h.sql("SELECT * FROM sources")[0]
            .opt_text("etag")
            .unwrap()
            .is_none()
    );
}
