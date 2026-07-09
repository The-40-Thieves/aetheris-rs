use axum::Router;
use std::sync::Arc;
use crate::database::Database;
use crate::monitors::ai_observability::ai_router;

/// Address the embedded AI-observability proxy listens on.
///
/// Bound to loopback only. This port fronts local LLM inference engines and
/// performs no authentication, so exposing it on `0.0.0.0` would hand the whole
/// LAN / Tailscale mesh an open, unauthenticated proxy into the user's machine.
/// Remote access (e.g. from a phone on the mesh) must be added deliberately with
/// a bearer token or mTLS check — never by widening this bind.
const PROXY_BIND_ADDR: &str = "127.0.0.1:3030";

pub async fn start_server(db: Arc<Database>) {
    let app = Router::new()
        .merge(ai_router(db.clone()));

    match tokio::net::TcpListener::bind(PROXY_BIND_ADDR).await {
        Ok(listener) => {
            println!("Aetheris AI proxy listening on http://{PROXY_BIND_ADDR}");
            if let Err(e) = axum::serve(listener, app).await {
                eprintln!("Aetheris AI proxy server error: {e}");
            }
        }
        Err(e) => {
            // Do not fail silently: a port clash or permission error here means
            // the AI-observability feature is dead, and the user should know.
            eprintln!("Aetheris AI proxy failed to bind {PROXY_BIND_ADDR}: {e}");
        }
    }
}
