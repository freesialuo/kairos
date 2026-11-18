use super::{Method, Request, Response};
use crate::{CoreError, Hub, Ports};
use futures::FutureExt;
use kairos_types::HealthResponse;
use matchit::Router;
use std::{io::Write, panic::AssertUnwindSafe, sync::LazyLock};

static ROUTES: LazyLock<Result<Router<Method>, matchit::InsertError>> = LazyLock::new(|| {
    let mut router = Router::new();
    router.insert("/api/health", Method::Get)?;
    Ok(router)
});

impl<P: Ports> Hub<P> {
    pub async fn handle(&self, req: Request) -> Response {
        let outcome = AssertUnwindSafe(self.route(req)).catch_unwind().await;
        match outcome {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => Response::from_error(&error),
            Err(_) => {
                let _ = writeln!(std::io::stderr(), "panic in Hub::handle");
                Response::from_error(&CoreError::Internal("router panic".into()))
            }
        }
    }
    async fn route(&self, req: Request) -> Result<Response, CoreError> {
        if req.path.starts_with("/auth/") {
            return super::auth_routes::route(self, &req);
        }
        if req.path.starts_with("/api/admin/") {
            crate::auth::session::require_full_session(self, &req)?;
        }
        if req.method == Method::Post && req.path == "/internal/rearm" {
            return Ok(Response::json(
                200,
                &crate::sched::alarm::watchdog(self).await?,
            ));
        }
        #[cfg(feature = "test-hooks")]
        if self.config.allow_http_urls && req.path.starts_with("/__test/adapter/") {
            return super::internal::adapter(self, &req).await;
        }
        let router = ROUTES
            .as_ref()
            .map_err(|error| CoreError::Internal(error.to_string()))?;
        let route = router.at(&req.path).map_err(|_| CoreError::NotFound)?;
        if req.method != *route.value {
            return Err(CoreError::NotFound);
        }
        Ok(Response::json(200, &HealthResponse { ok: true }))
    }
}
