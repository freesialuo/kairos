mod db;
mod fetch;

use std::rc::Rc;

use kairos_core::{
    CoreError, DbError,
    ports::{Alarm, Clock, Commit, Entropy, Ports},
};
use worker::{ScheduledTime, Storage, js_sys};

pub(crate) use db::DoDb;
pub(crate) use fetch::FetchHttp;

pub(crate) struct WorkerPorts;
impl Ports for WorkerPorts {
    type Db = DoDb;
    type Clock = DateClock;
    type Entropy = CryptoEntropy;
    type Http = FetchHttp;
    type Alarm = DoAlarm;
    type Commit = DoCommit;
}

pub(crate) struct DateClock;
impl Clock for DateClock {
    fn now_ms(&self) -> i64 {
        js_sys::Date::now() as i64
    }
}

pub(crate) struct CryptoEntropy;
impl Entropy for CryptoEntropy {
    fn fill(&self, buf: &mut [u8]) {
        if getrandom::fill(buf).is_err() {
            wasm_bindgen::throw_str("Workers cryptographic randomness is unavailable");
        }
    }
}

pub(crate) struct DoAlarm(pub(crate) Rc<Storage>);
impl Alarm for DoAlarm {
    async fn get(&self) -> Result<Option<i64>, CoreError> {
        self.0
            .get_alarm()
            .await
            .map_err(|error| DbError(format!("read alarm: {error}")).into())
    }

    async fn set(&self, at_ms: i64) -> Result<(), CoreError> {
        let date = js_sys::Date::new(&js_sys::Number::from(at_ms as f64));
        // Passing an i64 directly to set_alarm would schedule a relative delay.
        self.0
            .set_alarm(ScheduledTime::new(date))
            .await
            .map_err(|error| DbError(format!("set alarm: {error}")).into())
    }
}

pub(crate) struct DoCommit(pub(crate) Rc<Storage>);
impl Commit for DoCommit {
    async fn commit_point(&self) -> Result<(), CoreError> {
        self.0
            .sync()
            .await
            .map_err(|error| DbError(format!("sync storage: {error}")).into())
    }
}
