pub mod auth;
pub mod calendar {
    pub mod events;
    pub mod model;
    pub mod plan;
    pub mod reconcile;
}
pub mod config;
pub mod time;
pub mod db {
    pub mod schema;
    pub mod settings;
}
pub mod error;
pub mod http;
pub mod notify;
pub mod ports;
pub mod sources {
    pub mod model;
    pub mod sync;
}
pub mod sched {
    pub mod alarm;
    pub mod dispatch;
    pub mod queue;
}

pub use config::Config;
pub use error::{CoreError, DbError, NetError};
pub use ports::{Clock, Db, Ports};

pub struct Hub<P: Ports> {
    pub db: P::Db,
    pub clock: P::Clock,
    pub entropy: P::Entropy,
    pub http: P::Http,
    pub alarm: P::Alarm,
    pub commit: P::Commit,
    pub config: Config,
}
impl<P: Ports> Hub<P> {
    pub fn now(&self) -> i64 {
        let now = self.clock.now_ms();
        #[cfg(feature = "test-hooks")]
        if self.config.allow_http_urls {
            let offset = crate::db::settings::get_meta(&self.db, "test_clock_offset_ms")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            return now.saturating_add(offset);
        }
        now
    }
    pub fn migrate(&self) -> Result<(), CoreError> {
        db::schema::migrate(&self.db)
    }
}

pub mod ics {
    pub mod expand;
    pub mod parse;
}
pub mod webauthn;
