//! Native integration harness for the same Hub used by the Worker.
pub mod authenticator;
mod db;
mod fakes;
pub use db::SqliteDb;
pub use fakes::{FakeAlarm, FakeClock, FakeCommit, FakeHttp, Faults, SeededEntropy};
use kairos_core::{
    Config, Db, Hub, Ports,
    http::{Request, Response},
    ports::Row,
};
use sha2::{Digest, Sha256};
use std::{cell::Cell, ops::Deref};

pub const NOW: &str = "2026-11-20T08:00:00Z";
pub fn ms(iso: &str) -> i64 {
    iso.parse::<jiff::Timestamp>()
        .expect("valid RFC 3339 test instant")
        .as_millisecond()
}

pub struct TestPorts;
impl Ports for TestPorts {
    type Db = SqliteDb;
    type Clock = FakeClock;
    type Entropy = SeededEntropy;
    type Http = FakeHttp;
    type Alarm = FakeAlarm;
    type Commit = FakeCommit;
}
pub struct TestHub {
    pub hub: Hub<TestPorts>,
}
impl Deref for TestHub {
    type Target = Hub<TestPorts>;
    fn deref(&self) -> &Self::Target {
        &self.hub
    }
}
impl Default for TestHub {
    fn default() -> Self {
        Self::new()
    }
}
impl TestHub {
    pub fn new() -> Self {
        let faults = Faults::default();
        let hub = Hub {
            db: SqliteDb::new(faults.clone()),
            clock: FakeClock(Cell::new(ms(NOW))),
            entropy: SeededEntropy(Cell::new(12345)),
            http: FakeHttp::new(faults.clone()),
            alarm: FakeAlarm::default(),
            commit: FakeCommit(faults),
            config: Config {
                rp_id: "kairos.test".into(),
                origin: "https://kairos.test".into(),
                recovery_digest: Sha256::digest(b"correct horse").into(),
                session_secret: vec![42; 32],
                allow_http_urls: true,
            },
        };
        let test = Self { hub };
        test.migrate().expect("migrate test database");
        test
    }
    pub fn faults(&self) -> &Faults {
        &self.db.faults
    }
    pub fn set_now(&self, iso: &str) {
        self.clock.0.set(ms(iso));
    }
    pub fn advance_min(&self, minutes: i64) {
        self.clock.0.set(self.now() + minutes * 60_000);
    }
    pub fn http(&self) -> &FakeHttp {
        &self.hub.http
    }
    pub fn alarm_at(&self) -> Option<i64> {
        self.alarm.at.get()
    }
    pub fn set_alarm_calls(&self) -> usize {
        self.alarm.calls.get()
    }
    pub fn sql(&self, q: &str) -> Vec<Row> {
        self.db.exec(q, &[]).expect("test SQL")
    }
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
        cookies: &[&str],
    ) -> TestResponse {
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let mut headers = vec![("Origin".into(), self.config.origin.clone())];
        if !cookies.is_empty() {
            headers.push(("Cookie".into(), cookies.join("; ")));
        }
        let bytes = body
            .map(|value| {
                headers.push(("Content-Type".into(), "application/json".into()));
                serde_json::to_vec(&value).expect("test JSON")
            })
            .unwrap_or_default();
        let response = self
            .handle(Request {
                method: method.into(),
                path: path.into(),
                query: form_urlencoded::parse(query.as_bytes())
                    .into_owned()
                    .collect(),
                headers,
                body: bytes,
            })
            .await;
        TestResponse(response)
    }
}
pub struct TestResponse(Response);
impl Deref for TestResponse {
    type Target = Response;
    fn deref(&self) -> &Response {
        &self.0
    }
}
impl TestResponse {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("JSON response")
    }
}
