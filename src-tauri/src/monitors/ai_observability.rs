use axum::{
    extract::{Request, State},
    response::Response,
    body::Body,
    routing::any,
    Router,
};
use reqwest::Client;
use std::sync::Arc;
use crate::database::Database;

#[derive(Clone)]
pub struct ProxyState {
    pub client: Client,
    pub db: Arc<Database>,
}

pub fn ai_router(db: Arc<Database>) -> Router {
    let client = Client::new();
    let state = ProxyState { client, db };

    Router::new()
        // Axum 0.8 (matchit 0.8) requires the wildcard capture to be brace-wrapped
        // as `{*path}`; the pre-0.8 bare `*path` form panics at router construction.
        .route("/ollama/{*path}", any(ollama_proxy))
        .route("/lmstudio/{*path}", any(lmstudio_proxy))
        .with_state(state)
}

async fn ollama_proxy(
    State(_state): State<ProxyState>,
    req: Request,
) -> Result<Response, (axum::http::StatusCode, String)> {
    // Intercept and log logic would parse `eval_count` and TPS here
    Ok(axum::response::Response::builder()
        .status(200)
        .body(Body::from(format!("Intercepted Ollama Request to: {}", req.uri())))
        .unwrap())
}

async fn lmstudio_proxy(
    State(_state): State<ProxyState>,
    req: Request,
) -> Result<Response, (axum::http::StatusCode, String)> {
    Ok(axum::response::Response::builder()
        .status(200)
        .body(Body::from(format!("Intercepted LM Studio Request to: {}", req.uri())))
        .unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_router_builds_without_panicking() {
        // Regression test for the axum 0.8 wildcard route syntax (`{*path}`).
        // The pre-0.8 bare `*path` form panics inside `Router::route`, so simply
        // constructing the router IS the assertion here.
        let db = std::sync::Arc::new(
            crate::database::Database::new(std::path::PathBuf::from(":memory:"))
                .expect("in-memory db should init"),
        );
        let _router = ai_router(db);
    }
}
