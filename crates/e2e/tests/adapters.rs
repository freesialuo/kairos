use e2e::{MockServer, WranglerDev};
use serde_json::json;

#[allow(
    clippy::disallowed_methods,
    reason = "the local Worker uses the real time for alarms"
)]
#[tokio::test]
async fn worker_and_sqlite_adapter_lifecycle() {
    let mut worker = WranglerDev::start().await;
    assert_eq!(worker.get("/api/health").await, json!({"ok": true}));
    assert_eq!(worker.get("/__test/adapter/schema").await["version"], "3");

    let public = worker
        .request(reqwest::Method::POST, "/internal/rearm")
        .send()
        .await
        .unwrap();
    assert!(matches!(
        public.status(),
        reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::METHOD_NOT_ALLOWED
    ));
    assert_eq!(
        worker.get("/__test/adapter/watchdog").await["at"],
        json!(null)
    );
    let scheduled = worker
        .request(reqwest::Method::GET, "/cdn-cgi/local/scheduled")
        .send()
        .await
        .unwrap();
    assert!(scheduled.status().is_success());
    assert!(worker.get("/__test/adapter/watchdog").await["at"].is_string());

    let panic_response = worker
        .post_response("tx-rollback", json!({"panic": true}))
        .await;
    assert_eq!(
        panic_response.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        panic_response.json::<serde_json::Value>().await.unwrap(),
        json!({"error": "internal"}),
    );
    // A second transaction checks that the first write was rolled back and the object still serves requests.
    assert_eq!(
        worker.post("tx-rollback", json!({})).await["rolled_back"],
        true
    );
    assert_eq!(worker.post("commit", json!({})).await["ok"], true);

    // The unique future alarm also acts as a persistence marker across a cold restart.
    let at = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
        + 3_600_000;
    assert_eq!(worker.post("alarm", json!({"at": at})).await["at"], at);
    let invalid_alarm = worker.post_response("alarm", json!({"at": i64::MAX})).await;
    assert_eq!(
        invalid_alarm.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );
    worker.restart().await;
    assert_eq!(worker.get("/__test/adapter/schema").await["version"], "3");
    assert_eq!(worker.get("/__test/adapter/alarm").await["at"], at);

    let mock = MockServer::start().await;
    let url = mock.url("/big/6");
    let cap = 5 * 1024 * 1024;
    assert_eq!(
        worker.post("fetch", json!({"url": url, "max": cap})).await,
        json!({"error": "too_large"}),
    );
    assert_eq!(
        worker
            .post("fetch", json!({"url": mock.url("/big/5"), "max": cap}))
            .await,
        json!({"len": cap, "status": 200}),
    );
}
