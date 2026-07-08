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
        .route("/ollama/*path", any(ollama_proxy))
        .route("/lmstudio/*path", any(lmstudio_proxy))
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
