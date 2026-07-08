use axum::Router;
use std::sync::Arc;
use crate::database::Database;
use crate::monitors::ai_observability::ai_router;

pub async fn start_server(db: Arc<Database>) {
    let app = Router::new()
        .merge(ai_router(db.clone()));
    
    if let Ok(listener) = tokio::net::TcpListener::bind("0.0.0.0:3030").await {
        println!("Aetheris Web Server listening on port 3030");
        let _ = axum::serve(listener, app).await;
    }
}
