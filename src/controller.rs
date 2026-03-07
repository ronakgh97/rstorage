use anyhow::Result;
use axum::Router;
use axum::routing::{get, post};
use dashmap::DashMap;
use lazy_static::lazy_static;

lazy_static! {
    pub static ref ON_GOINGS: DashMap<String, String> = DashMap::new();
}

async fn create_router() -> Router {
    Router::new()
        .route("/status/:id", get(status_handler))
        .route("/upload", post(upload_handler))
        .route("/download/:id", get(download_handler))
}

async fn start_server() -> Result<()> {
    let port = 3000;

    let router = create_router().await;

    let addr = format!("0.0.0.0:{}", port);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router).await?;
    Ok(())
}

async fn status_handler() -> String {
    "Status handler".to_string()
}

async fn upload_handler() -> String {
    "Upload handler".to_string()
}

async fn download_handler() -> String {
    "Download handler".to_string()
}
