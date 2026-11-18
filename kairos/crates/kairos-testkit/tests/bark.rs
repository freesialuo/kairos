use kairos_core::{
    Db,
    notify::{
        bark::{BarkConfig, ChannelError, load_channel, send_bark},
        message::Message,
    },
    ports::Value,
};
use kairos_testkit::{FakeHttp, TestHub};

const SERVER: &str = "https://bark.test";

fn config() -> BarkConfig {
    BarkConfig {
        server: format!("{SERVER}/"),
        device_key: "fake-key".into(),
    }
}

fn message() -> Message {
    Message {
        title: "⏰ Standup".into(),
        body: "09:00 · now".into(),
        url: Some("https://kairos.test/calendar?event=42".into()),
    }
}

#[tokio::test]
async fn bark_posts_expected_json() {
    let http = FakeHttp::default();
    http.respond(SERVER, 200, &[], br#"{"code":200}"#);
    send_bark(&http, &config(), &message()).await.unwrap();
    let requests = http.requests();
    assert_eq!(requests.len(), 1);
    let request = requests.first().unwrap();
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, format!("{SERVER}/push"));
    assert_eq!(
        request.headers,
        [("Content-Type".into(), "application/json".into())]
    );
    assert_eq!(request.max_body_bytes, 65_536);
    let posted: serde_json::Value =
        serde_json::from_slice(request.body.as_deref().unwrap()).unwrap();
    assert_eq!(
        posted,
        serde_json::json!({
            "device_key": "fake-key",
            "title": "⏰ Standup",
            "body": "09:00 · now",
            "url": "https://kairos.test/calendar?event=42",
            "group": "reminders",
            "level": "timeSensitive",
        })
    );
}

#[tokio::test]
async fn bark_http_500_is_error() {
    let http = FakeHttp::default();
    http.respond(SERVER, 500, &[], br#"{"code":200}"#);
    assert_eq!(
        send_bark(&http, &config(), &message()).await,
        Err(ChannelError::Http(500)),
    );
}

#[tokio::test]
async fn bark_code_400_in_200_response_is_error() {
    let http = FakeHttp::default();
    http.respond(SERVER, 200, &[], br#"{"code":400,"message":"bad key"}"#);
    assert_eq!(
        send_bark(&http, &config(), &message()).await,
        Err(ChannelError::Api(400, "bad key".into())),
    );
}

#[tokio::test]
async fn bark_rejects_invalid_and_oversized_responses() {
    let http = FakeHttp::default();
    http.respond(SERVER, 200, &[], b"{\"code\":\"200\"}");
    http.respond(SERVER, 200, &[], &vec![b'x'; 65_537]);
    assert_eq!(
        send_bark(&http, &config(), &message()).await,
        Err(ChannelError::Net("invalid Bark response".into())),
    );
    assert_eq!(
        send_bark(&http, &config(), &message()).await,
        Err(ChannelError::Net("response body too large".into())),
    );
}

#[test]
fn load_channel_requires_enabled_bark_and_defaults_server() {
    let hub = TestHub::new();
    assert_eq!(load_channel(&hub.db).unwrap(), None);

    hub.db
        .exec(
            "INSERT INTO channels(kind, config, enabled) VALUES ('bark', ?1, 0)",
            &[Value::Text(r#"{"device_key":"fake-key"}"#.into())],
        )
        .unwrap();
    assert_eq!(load_channel(&hub.db).unwrap(), None);

    hub.db
        .exec("UPDATE channels SET enabled=1 WHERE kind='bark'", &[])
        .unwrap();
    assert_eq!(
        load_channel(&hub.db).unwrap(),
        Some(BarkConfig {
            server: "https://api.day.app".into(),
            device_key: "fake-key".into(),
        })
    );
}
