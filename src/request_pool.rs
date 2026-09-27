//! The composer-installed request pool, made visible to the verb services.
//!
//! ADR-0029's pool law: modules are tenant-agnostic and the composing
//! service owns routing to the right database. The composer's tenant router
//! already inserts the request's `PgPool` into the request extensions;
//! [`bind_request_pool`] copies it into a task-local for the duration of the
//! handler future, and the write services resolve their database through
//! [`current`] with their composed pool as the fallback — so a mount without
//! tenant routing behaves exactly as before, and a verb under a tenant mount
//! writes to that tenant's database (host-wired ports ride along untouched).

use axum::{body::Body, http::Request, middleware::Next, response::Response};
use sqlx::PgPool;

tokio::task_local! {
    static REQUEST_POOL: PgPool;
}

/// The pool this request's tenant router installed, if any.
pub fn current() -> Option<PgPool> {
    REQUEST_POOL.try_with(|pool| pool.clone()).ok()
}

/// Middleware that binds the composer-inserted pool for the whole handler
/// call. Requests without an inserted pool pass straight through.
pub async fn bind_request_pool(req: Request<Body>, next: Next) -> Response {
    match req.extensions().get::<PgPool>().cloned() {
        Some(pool) => REQUEST_POOL.scope(pool, next.run(req)).await,
        None => next.run(req).await,
    }
}

/// Run a future with a pool bound as this module's request pool — the host
/// verdict dispatcher and relay consumers use this so module-side service
/// calls resolve the caller's database through rpool() (ADR-0029 pool law).
pub async fn with_pool_scope<F: std::future::Future<Output = O>, O>(
    pool: sqlx::PgPool,
    fut: F,
) -> O {
    REQUEST_POOL.scope(pool, fut).await
}
