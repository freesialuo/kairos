use kairos_core::http::{Method, Request};
use kairos_testkit::{
    TestHub, TestResponse,
    authenticator::{Alg, SoftAuthenticator},
};
use kairos_types::auth::{CreationOptionsJson, RequestOptionsJson};
use serde_json::{Value, json};

async fn recover(hub: &TestHub) -> String {
    let response = hub
        .request(
            "POST",
            "/auth/recovery",
            Some(json!({"code": "correct horse"})),
            &[],
        )
        .await;
    assert_eq!(response.status, 200);
    response.cookie("__Host-sid").unwrap().into()
}

async fn register(hub: &TestHub) -> (String, SoftAuthenticator, String) {
    let session = recover(hub).await;
    let options = hub
        .request("POST", "/auth/passkey/register/options", None, &[&session])
        .await;
    assert_eq!(options.status, 200);
    let challenge = options.cookie("__Host-chal").unwrap();
    let payload: CreationOptionsJson = serde_json::from_slice(&options.body).unwrap();
    let mut authenticator = SoftAuthenticator::new(Alg::Es256, hub.entropy.0.get());
    let attestation = authenticator.register(&payload, &hub.config.origin);
    let response = hub
        .request(
            "POST",
            "/auth/passkey/register/verify",
            Some(json!({"response": attestation})),
            &[&session, challenge],
        )
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.cookie("__Host-chal"), Some("__Host-chal="));
    let id = response
        .json()
        .get("id")
        .and_then(Value::as_str)
        .unwrap()
        .to_owned();
    (session, authenticator, id)
}

async fn login_request(hub: &TestHub, authenticator: &mut SoftAuthenticator) -> (Value, String) {
    let response = hub
        .request("POST", "/auth/passkey/login/options", None, &[])
        .await;
    assert_eq!(response.status, 200);
    let challenge = response.cookie("__Host-chal").unwrap().to_owned();
    let opts: RequestOptionsJson = serde_json::from_slice(&response.body).unwrap();
    let assertion = authenticator.login(&opts, &hub.config.origin);
    (json!({"response": assertion}), challenge)
}

async fn login(hub: &TestHub, authenticator: &mut SoftAuthenticator) -> TestResponse {
    let (body, challenge) = login_request(hub, authenticator).await;
    hub.request(
        "POST",
        "/auth/passkey/login/verify",
        Some(body),
        &[&challenge],
    )
    .await
}

#[tokio::test]
async fn fresh_deployment_requires_recovery_for_first_key() {
    let hub = TestHub::new();
    let response = hub.request("GET", "/auth/state", None, &[]).await;
    assert_eq!(
        response.json(),
        json!({"setup_required": true, "session": null, "passkeys": 0})
    );
    let session = hub.admin_session().await;
    let response = hub.request("GET", "/auth/state", None, &[&session]).await;
    assert_eq!(
        response.json(),
        json!({"setup_required": false, "session": "full", "passkeys": 1})
    );
    let rows = hub.sql("SELECT name FROM passkeys");
    assert_eq!(rows.first().unwrap().text("name").unwrap(), "Passkey 1");
}

#[tokio::test]
async fn registration_without_session_is_401() {
    let hub = TestHub::new();
    let options = hub
        .request("POST", "/auth/passkey/register/options", None, &[])
        .await;
    assert_eq!(options.status, 401);
    let verify = hub
        .request("POST", "/auth/passkey/register/verify", None, &[])
        .await;
    assert_eq!(verify.status, 401);
}

#[tokio::test]
async fn recovery_session_can_only_register() {
    let hub = TestHub::new();
    let cookie = recover(&hub).await;
    let state = hub.request("GET", "/auth/state", None, &[&cookie]).await;
    assert_eq!(state.json().get("session"), Some(&json!("recovery")));
    let settings = hub
        .request("GET", "/api/admin/settings", None, &[&cookie])
        .await;
    assert_eq!(settings.status, 403);
    let logout_all = hub
        .request("POST", "/auth/logout-all", None, &[&cookie])
        .await;
    assert_eq!(logout_all.status, 403);
    let options = hub
        .request("POST", "/auth/passkey/register/options", None, &[&cookie])
        .await;
    assert_eq!(options.status, 200);
}

#[tokio::test]
async fn register_then_login_with_passkey() {
    let hub = TestHub::new();
    let (_, mut authenticator, _) = register(&hub).await;
    let response = login(&hub, &mut authenticator).await;
    assert_eq!(response.status, 200);
    let cookie = response.cookie("__Host-sid").unwrap();
    assert_eq!(response.cookie("__Host-chal"), Some("__Host-chal="));
    let status = hub.request("GET", "/auth/state", None, &[cookie]).await;
    assert_eq!(status.json().get("session"), Some(&json!("full")));
    let used = hub.sql("SELECT last_used_at, counter FROM passkeys");
    let used = used.first().unwrap();
    assert_eq!(used.i64("last_used_at").unwrap(), hub.now());
    assert_eq!(used.i64("counter").unwrap(), 1);
}

#[tokio::test]
async fn rejected_counter_regression_is_visible_in_diagnostics() {
    let hub = TestHub::new();
    let (_, mut authenticator, _) = register(&hub).await;
    assert_eq!(login(&hub, &mut authenticator).await.status, 200);
    authenticator.tamper().counter = Some(1);
    let response = login(&hub, &mut authenticator).await;
    assert_eq!(response.status, 400);
    assert_eq!(response.json(), json!({"error":"counter_regression"}));
    assert_eq!(
        kairos_core::db::settings::get_meta(&hub.db, "last_auth_error").as_deref(),
        Some("possible cloned authenticator")
    );
}

#[tokio::test]
async fn stale_rp_id_key_not_counted_and_cannot_log_in() {
    let hub = TestHub::new();
    let (_, mut authenticator, _) = register(&hub).await;
    hub.sql("UPDATE passkeys SET rp_id='old.test'");
    let state = hub.request("GET", "/auth/state", None, &[]).await;
    assert_eq!(
        state.json(),
        json!({"setup_required": true, "session": null, "passkeys": 0})
    );
    let response = login(&hub, &mut authenticator).await;
    assert_eq!(response.status, 400);
    assert_eq!(response.json(), json!({"error": "unknown_credential"}));
}

#[tokio::test]
async fn post_without_origin_is_403() {
    let hub = TestHub::new();
    let response = hub
        .handle(Request {
            method: Method::Post,
            path: "/auth/passkey/login/options".into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        })
        .await;
    assert_eq!(response.status, 403);
}

#[tokio::test]
async fn logout_all_revokes_every_session() {
    let hub = TestHub::new();
    let (registration_session, mut authenticator, _) = register(&hub).await;
    let second_session = login(&hub, &mut authenticator).await;
    let second_session = second_session.cookie("__Host-sid").unwrap();
    let response = hub
        .request("POST", "/auth/logout-all", None, &[second_session])
        .await;
    assert_eq!(response.status, 204);
    assert_eq!(response.cookie("__Host-sid"), Some("__Host-sid="));
    for cookie in [&registration_session, second_session] {
        let response = hub.request("GET", "/auth/state", None, &[cookie]).await;
        assert_eq!(response.json().get("session"), Some(&Value::Null));
    }
}

#[tokio::test]
async fn challenge_replay_rejected() {
    let hub = TestHub::new();
    let (_, mut authenticator, _) = register(&hub).await;
    let (body, challenge) = login_request(&hub, &mut authenticator).await;
    let first = hub
        .request(
            "POST",
            "/auth/passkey/login/verify",
            Some(body.clone()),
            &[&challenge],
        )
        .await;
    assert_eq!(first.status, 200);
    let replay = hub
        .request(
            "POST",
            "/auth/passkey/login/verify",
            Some(body),
            &[&challenge],
        )
        .await;
    assert_eq!(replay.status, 401);
    assert_eq!(replay.cookie("__Host-chal"), Some("__Host-chal="));
}

#[tokio::test]
async fn successful_recovery_arms_background_maintenance() {
    let hub = TestHub::new();
    assert_eq!(hub.alarm_at(), None);
    let response = hub
        .request(
            "POST",
            "/auth/recovery",
            Some(json!({"code":"correct horse"})),
            &[],
        )
        .await;
    assert_eq!(response.status, 200);
    assert!(hub.alarm_at().is_some());
}
