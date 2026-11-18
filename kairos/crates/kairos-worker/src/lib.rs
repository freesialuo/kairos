#![cfg(target_arch = "wasm32")]

mod adapters;
mod convert;
mod hub_do;

use kairos_core::{CoreError, http};
use worker::{Context, Env, Request, Response, Result, event};

pub use hub_do::Hub;

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let path = req.path();
    if path.starts_with("/api/")
        || path.starts_with("/auth/")
        || (cfg!(feature = "test-hooks") && path.starts_with("/__test/"))
    {
        let namespace = env.durable_object("HUB")?;
        let stub = namespace.id_from_name("main")?.get_stub()?;
        stub.fetch_with_request(req).await
    } else {
        convert::response(http::Response::from_error(&CoreError::NotFound))
    }
}
