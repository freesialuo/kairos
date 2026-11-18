pub mod calendar {
    pub mod events;
    pub mod plan;
    pub mod reconcile;
    pub mod model;
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
    pub mod sync;
    pub mod model;
}
pub mod sched {
    pub mod queue;
    pub mod alarm;
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
        self.clock.now_ms()
    }
    pub fn migrate(&self) -> Result<(), CoreError> {
        db::schema::migrate(&self.db)
    }
}

pub mod ics {
    pub mod expand;
    pub mod parse;
}
