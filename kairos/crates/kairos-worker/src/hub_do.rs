use std::{cell::Cell, panic::AssertUnwindSafe, rc::Rc};

use kairos_core::{Config, CoreError, Hub as CoreHub, http};
use sha2::{Digest, Sha256};
use worker::{
    DurableObject, Env, Error, Request, Response, Result, State, console_error, durable_object,
};

use crate::{
    adapters::{CryptoEntropy, DateClock, DoAlarm, DoCommit, DoDb, FetchHttp, WorkerPorts},
    convert,
};

#[durable_object]
pub struct Hub {
    core: Result<CoreHub<WorkerPorts>, String>,
    // A failed or panicking migration leaves the flag false and can be retried.
    migrated: AssertUnwindSafe<Cell<bool>>,
}

impl Hub {
    fn ready(&self) -> Result<&CoreHub<WorkerPorts>, CoreError> {
        let core = self
            .core
            .as_ref()
            .map_err(|error| CoreError::Internal(format!("invalid configuration: {error}")))?;
        if !self.migrated.get() {
            core.migrate()?;
            self.migrated.set(true);
        }
        Ok(core)
    }
}

impl DurableObject for Hub {
    fn new(state: State, env: Env) -> Self {
        let core = config(&env)
            .map(|config| {
                let storage = Rc::new(state.storage());
                CoreHub {
                    db: DoDb::new(&storage),
                    clock: DateClock,
                    entropy: CryptoEntropy,
                    http: FetchHttp,
                    alarm: DoAlarm(Rc::clone(&storage)),
                    commit: DoCommit(storage),
                    config,
                }
            })
            .map_err(|error| error.to_string());
        Self {
            core,
            migrated: AssertUnwindSafe(Cell::new(false)),
        }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        match self.ready() {
            Ok(core) => convert::response(core.handle(convert::request(req).await?).await),
            Err(error) => {
                console_error!("Kairos initialization failed: {error}");
                convert::response(http::Response::from_error(&error))
            }
        }
    }

    async fn alarm(&self) -> Result<Response> {
        match self.ready() {
            Ok(core) => {
                let _ = core.run_alarm().await;
            }
            Err(error) => console_error!("Kairos initialization failed: {error}"),
        }
        Response::empty()
    }
}

fn config(env: &Env) -> Result<Config> {
    let recovery = env.secret("RECOVERY_CODE")?.to_string();
    let secret = env.secret("SESSION_SECRET")?.to_string();
    let mut session_secret = [0; 32];
    hex::decode_to_slice(secret, &mut session_secret)
        .map_err(|_| Error::RustError("SESSION_SECRET must contain 64 hex characters".into()))?;
    Ok(Config {
        rp_id: env.var("RP_ID")?.to_string(),
        origin: env.var("ORIGIN")?.to_string(),
        recovery_digest: Sha256::digest(recovery.as_bytes()).into(),
        session_secret: session_secret.to_vec(),
        allow_http_urls: cfg!(feature = "test-hooks"),
    })
}
