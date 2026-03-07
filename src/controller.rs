use crate::service::get_storage_path;
use anyhow::Result;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Request};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use dashmap::DashMap;
use futures_util::StreamExt;
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use std::time::Instant;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

static START_TIME: OnceLock<chrono::DateTime<chrono::Local>> = OnceLock::new();

lazy_static! {
    pub static ref ON_GOINGS: DashMap<String, String> = DashMap::new();
}

async fn create_router() -> Router {
    Router::new()
        .route("/status/:id", get(status_handler))
        .route("/upload", post(upload_handler))
        .route("/download/:id", get(download_handler))
        .layer(DefaultBodyLimit::max(1 * 1024 * 1024 * 1024)) // Set max body size to 1GB
}

pub async fn start_server(port: u16) -> Result<()> {
    let router = create_router().await;

    let now = chrono::Local::now();
    START_TIME.get_or_init(|| now);
    let addr = format!("0.0.0.0:{}", port);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router).await?;
    Ok(())
}

#[inline]
/// Get the server uptime in hours
pub fn get_uptime_hrs() -> f64 {
    if let Some(start_time) = START_TIME.get() {
        let now = chrono::Local::now();
        let duration = now.signed_duration_since(*start_time);
        duration.num_hours() as f64
    } else {
        0.0
    }
}

#[derive(Deserialize, Serialize)]
struct Status {
    timestamp: String,
    uptime_hrs: f64,
    on_goings_task: Vec<String>,
}

pub async fn status_handler() -> impl IntoResponse {
    let status = Status {
        timestamp: chrono::Local::now().to_rfc3339(),
        uptime_hrs: get_uptime_hrs(),
        on_goings_task: ON_GOINGS.iter().map(|entry| entry.key().clone()).collect(),
    };

    (StatusCode::OK, Json(status))
}

#[derive(Deserialize, Serialize)]
pub struct UploadStatus {
    pub file_id: String,
    pub time_took: f64,
}

#[derive(Deserialize, Serialize)]
struct Metadata {
    filename: String,
    file_size: u64,
    file_hash: String,
    file_key: String,
}

const CHUNK_BUF_SIZE: usize = 8 * 1024 * 1024; // 8MB

const WRITE_BUF_SIZE: usize = 16 * 1024 * 1024; // 16MB

pub async fn upload_handler(request: Request<Body>) -> impl IntoResponse {
    let headers: HeaderMap = request.headers().clone();

    let time_start = Instant::now();

    let filename = headers
        .get("x-file-name")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)
        .unwrap();

    // TODO: Implement size-limit later (=<1gb for now)
    // Size in bytes
    let file_size = headers
        .get("x-file-size")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)
        .unwrap()
        .parse::<u64>()
        .unwrap();

    let file_hash = headers
        .get("x-file-hash")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)
        .unwrap();

    // TODO: Implement file key validation and authentication later
    let file_key = headers
        .get("x-file-key")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)
        .unwrap();

    // Generate a unique file ID for this upload
    let file_id = Uuid::new_v4().to_string();

    // Register the ongoing upload task
    ON_GOINGS.insert(file_id.clone(), file_hash.to_string());

    println!(
        "Received upload request: filename={}, size={} bytes, hash={}, key={}, file_id={}",
        filename, file_size, file_hash, file_key, file_id
    );

    let mut stream = request.into_body().into_data_stream();

    let sanitized_filename = file_id
        .clone()
        .replace("-", "_")
        .replace("/", "_")
        .replace(".", "_")
        .replace("\\", "_");

    let path = get_storage_path().await.unwrap().join(&sanitized_filename);

    let file = tokio::fs::File::create(&path).await.unwrap();

    let mut file = tokio::io::BufWriter::with_capacity(WRITE_BUF_SIZE, file);

    // Buffer to hold incoming data until we have enough to write in chunks
    // TODO: Change this buffer to adjust to file-size later
    let mut buffer = Vec::with_capacity(CHUNK_BUF_SIZE);

    let mut hasher = Sha256::new();
    let mut received_bytes: u64 = 0;

    while let Some(chunk) = stream.next().await {
        let data = chunk.unwrap();

        received_bytes += data.len() as u64;
        hasher.update(&data);

        buffer.extend_from_slice(&data);

        while buffer.len() >= CHUNK_BUF_SIZE {
            file.write_all(&buffer[..CHUNK_BUF_SIZE]).await.unwrap(); // No-copy write
            buffer.drain(..CHUNK_BUF_SIZE);
        }
    }

    // Flush remaining buffer if it's the last chunk or whatever is left
    if !buffer.is_empty() {
        file.write_all(&buffer).await.unwrap();
        // buffer.clear();
    }

    file.flush().await.unwrap();

    // Post-Check
    let computed_hash = format!("{:x}", hasher.finalize());

    if computed_hash != file_hash {
        println!(
            "Hash mismatch for file_id={}: expected {}, got {}",
            file_id, file_hash, computed_hash
        );

        ON_GOINGS.remove(&file_id);
        // Clean up the uploaded file if hash verification fails
        tokio::fs::remove_file(&path).await.unwrap();
        return (
            StatusCode::BAD_REQUEST,
            Json(UploadStatus {
                file_id,
                time_took: 0.0,
            }),
        );
    }

    if received_bytes != file_size {
        println!(
            "Size mismatch for file_id={}: expected {} bytes, got {} bytes",
            file_id, file_size, received_bytes
        );

        ON_GOINGS.remove(&file_id);
        // Clean up the uploaded file if size verification fails
        tokio::fs::remove_file(&path).await.unwrap();
        return (
            StatusCode::BAD_REQUEST,
            Json(UploadStatus {
                file_id,
                time_took: 0.0,
            }),
        );
    }

    // Save Hash and key
    let metadata = Metadata {
        filename: filename.to_string(),
        file_size,
        file_hash: file_hash.to_string(),
        file_key: file_key.to_string(),
    };

    let path = get_storage_path()
        .await
        .unwrap()
        .join(format!("{}.meta", &sanitized_filename));

    let metadata_bytes = serde_json::to_vec(&metadata).unwrap();

    tokio::fs::write(&path, metadata_bytes).await.unwrap();

    let time_took = time_start.elapsed().as_secs_f64();

    // Remove the ongoing task from the map after completion
    ON_GOINGS.remove(&file_id);

    (StatusCode::OK, Json(UploadStatus { file_id, time_took }))
}

type DownloadResult = Result<Response<Body>, (StatusCode, String)>;

pub async fn download_handler(Path(id): Path<String>, headers: HeaderMap) -> DownloadResult {
    // TODO: Implement file key validation and authentication later
    let _file_key = headers
        .get("x-file-key")
        .and_then(|v| v.to_str().ok())
        .ok_or((StatusCode::BAD_REQUEST, "Missing file key".to_string()))?;

    let sanitized_id = id
        .replace("-", "_")
        .replace("/", "_")
        .replace(".", "_")
        .replace("\\", "_");

    let storage_path = get_storage_path()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let meta_path = storage_path.join(format!("{}.meta", &sanitized_id));
    let file_path = storage_path.join(&sanitized_id);

    let metadata_content = tokio::fs::read_to_string(&meta_path)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "File not found".to_string()))?;

    let metadata: Metadata = serde_json::from_str(&metadata_content).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to parse metadata".to_string(),
        )
    })?;

    let file = tokio::fs::File::open(&file_path)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "File not found".to_string()))?;

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/octet-stream")
        .header("x-file-name", metadata.filename)
        .header("x-file-size", metadata.file_size)
        .header("x-file-hash", metadata.file_hash)
        .body(body)
        .unwrap())
}
