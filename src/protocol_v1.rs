use crate::{
    MAX_FILE_SIZE, Metadata, NETWORK_BUFFER_SIZE, ON_GOINGS, READ_CHUNK_SIZE, SERVER_TRACKER,
    START_TIME, debug, error, file_hasher_async, get_storage_path, info, parse_status_line, trace,
    try_get_uptime_hrs, warn,
};
use anyhow::{Context, Result};
use colored::Colorize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

pub async fn start_tcp_server(port: u16) -> Result<()> {
    let now = chrono::Local::now();
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;

    let storage_path = get_storage_path().await?;
    let storage_path = Arc::new(storage_path);

    info!("TCP Server (v1 protocol) listening on 0.0.0.0:{}", port);
    START_TIME.get_or_init(|| now);
    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                info!("Connection request from {:?}", addr);
                let storage_path = Arc::clone(&storage_path);
                tokio::spawn(async move {
                    trace!("Task spawned for connection from {:?}", addr);
                    if let Err(e) = handle_connection(stream, &storage_path).await {
                        error!("Error handling connection from {:?}: {}", addr, e);
                    }
                });
            }
            Err(e) => {
                error!("Accept error: {}", e);
            }
        }
    }
}

#[inline]
async fn handle_connection(mut stream: TcpStream, storage_path: &std::path::Path) -> Result<()> {
    let start_time = Instant::now();
    stream.set_nodelay(true).ok();

    let (mut reader, mut writer) = stream.split();

    let (command, headers) = match read_request(&mut reader).await {
        Ok((cmd, hdr)) => (cmd, hdr),
        Err(e) => {
            // Handle early EOF gracefully
            if e.to_string().contains("early eof") || e.to_string().contains("EOF") {
                return Ok(());
            }
            return Err(e);
        }
    };
    debug!("Received {} request", command);

    match command.as_str() {
        "UPLOAD" => {
            handle_upload(&mut reader, &mut writer, &headers, start_time, storage_path).await?;
        }
        "DOWNLOAD" => {
            handle_download(&mut reader, &mut writer, &headers, storage_path).await?;
        }
        "DELETE" => {
            send_error(&mut writer, 501, "DELETE not implemented").await?;
        }
        "STATUS" => send_status(&mut writer, 200).await?,
        _ => {
            send_error(&mut writer, 400, "Unknown command").await?;
        }
    }

    writer.flush().await?;

    let mut tracker = SERVER_TRACKER.write().await;
    tracker.total_connections += 1;

    Ok(())
}

#[inline]
async fn read_request<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<(String, HashMap<String, String>)> {
    let len = read_frame_length(reader).await? as usize;
    let mut header_bytes = vec![0u8; len];
    reader.read_exact(&mut header_bytes).await?;

    let header_str = String::from_utf8(header_bytes).context("Invalid UTF-8 in request")?;

    let mut lines = header_str.lines();
    let command = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("Missing command"))?
        .trim()
        .to_string();

    let mut headers = HashMap::with_capacity(12);
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some(colon_pos) = line.find(':') {
            let key = line[..colon_pos].trim().to_string();
            let value = line[colon_pos + 1..].trim().to_string();
            headers.insert(key, value);
        }
    }

    Ok((command, headers))
}

#[inline]
pub async fn read_frame_length<R: AsyncReadExt + Unpin>(reader: &mut R) -> Result<u16> {
    let mut len_buf = [0u8; 2];
    reader.read_exact(&mut len_buf).await?;
    let len = u16::from_be_bytes(len_buf);
    Ok(len)
}

#[inline]
pub async fn write_frame<W: AsyncWriteExt + Unpin>(writer: &mut W, data: &[u8]) -> Result<()> {
    let len = data.len();
    if len > u16::MAX as usize {
        return Err(anyhow::anyhow!("Too large content: {} bytes", len));
    }
    let len = (len as u16).to_be_bytes();
    writer.write_all(&len).await?;
    writer.write_all(data).await?;
    writer.flush().await?;
    Ok(())
}

#[inline]
async fn send_error<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    code: u16,
    message: &str,
) -> Result<()> {
    let response = format!("ERROR\ncode: {}\nmessage: {}", code, message);
    write_frame(writer, response.as_bytes()).await?;
    writer.shutdown().await?;
    Ok(())
}

#[inline]
async fn send_status<W: AsyncWriteExt + Unpin>(writer: &mut W, code: u16) -> Result<()> {
    let uptime_hrs = try_get_uptime_hrs();
    let ongoing = ON_GOINGS.len();

    let (total_connections, total_bandwidth_gb) = {
        let lock = SERVER_TRACKER.read().await;
        (lock.total_connections, lock.total_bandwidth_gb)
    };

    let timestamp = chrono::Utc::now().to_rfc3339();

    let response = format!(
        "OK\ncode: {}\ntimestamp: {}\nuptime_hrs: {}\nno_goings_task: {}\ntotal_connections: {}\ntotal_bandwidth_gb: {}\n\n",
        code, timestamp, uptime_hrs, ongoing, total_connections, total_bandwidth_gb
    );

    write_frame(writer, response.as_bytes()).await?;
    writer.shutdown().await?;
    Ok(())
}

#[inline]
async fn send_ok_upload<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    file_id: &str,
    file_key: &str,
    time_took: f64,
) -> Result<()> {
    let response = format!(
        "OK\nfile-id: {}\nfile-key: {}\ntime-took: {}",
        file_id, file_key, time_took
    );
    write_frame(writer, response.as_bytes()).await?;
    Ok(())
}

async fn handle_upload<R: AsyncReadExt + Unpin, W: AsyncWriteExt + Unpin>(
    reader: &mut R,
    writer: &mut W,
    headers: &HashMap<String, String>,
    time_start: Instant,
    storage_path: &std::path::Path,
) -> Result<()> {
    let filename = headers
        .get("file-name")
        .ok_or_else(|| anyhow::anyhow!("Missing file-name header"))?
        .clone();
    let file_size: u64 = headers
        .get("file-size")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("Missing or invalid file-size header"))?;

    if file_size > MAX_FILE_SIZE {
        send_error(writer, 413, "File size exceeds 10GB limit").await?;
        return Ok(());
    }

    let file_hash = headers
        .get("file-hash")
        .ok_or_else(|| anyhow::anyhow!("Missing file-hash header"))?
        .clone();
    let file_key = headers
        .get("file-key")
        .ok_or_else(|| anyhow::anyhow!("Missing file-key header"))?
        .clone();

    let file_id = Uuid::new_v4().to_string();
    let sanitized_id = file_id
        .replace("-", "_")
        .replace("/", "_")
        .replace(".", "_")
        .replace("\\", "_");

    let file_path = storage_path.join(&sanitized_id);

    info!(
        "Start Uploading: {} ({} bytes) - Hash: {}...",
        filename,
        file_size,
        file_hash[..8].dimmed()
    );

    ON_GOINGS.insert(file_id.clone(), filename.clone());

    let file = tokio::fs::File::create(&file_path).await?;
    let mut buf_file = tokio::io::BufWriter::with_capacity(NETWORK_BUFFER_SIZE * 2, file);
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut buf = vec![0u8; READ_CHUNK_SIZE];

    while received < file_size {
        let to_read = std::cmp::min(buf.len(), (file_size - received) as usize);
        let n = reader.read(&mut buf[..to_read]).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        buf_file.write_all(&buf[..n]).await?;
        received += n as u64;
    }

    buf_file.flush().await?;

    if received != file_size {
        tokio::fs::remove_file(&file_path).await.ok();
        ON_GOINGS.remove(&file_id);
        send_error(writer, 406, "File size mismatch").await?;
        return Ok(());
    }

    let computed_hash = format!("{:x}", hasher.finalize());
    if computed_hash != file_hash {
        tokio::fs::remove_file(&file_path).await.ok();
        warn!(
            "Hash mismatch: expected {} but computed {}",
            file_hash, computed_hash
        );
        ON_GOINGS.remove(&file_id);
        send_error(writer, 406, "Hash mismatch").await?;
        return Ok(());
    }

    let metadata = Metadata {
        filename: filename.clone(),
        file_size,
        file_hash: computed_hash.clone(),
        file_key: file_key.clone(),
    };

    let metadata_path = storage_path.join(format!("{}.meta", sanitized_id));
    metadata.save_to_disk_async(&metadata_path).await?;

    ON_GOINGS.remove(&file_id);

    let time_took = time_start.elapsed().as_secs_f64();
    send_ok_upload(writer, &file_id, &file_key, time_took).await?;

    info!("Upload complete: File-ID: {}", file_id.dimmed());

    let mut lock = SERVER_TRACKER.write().await;
    lock.total_bandwidth_gb += file_size as f64 / (1024.0 * 1024.0 * 1024.0);

    Ok(())
}

async fn handle_download<R: AsyncReadExt + Unpin, W: AsyncWriteExt + Unpin>(
    _reader: &mut R,
    writer: &mut W,
    headers: &HashMap<String, String>,
    storage_path: &std::path::Path,
) -> Result<()> {
    let file_id = headers
        .get("file-id")
        .ok_or_else(|| anyhow::anyhow!("Missing file-id header"))?
        .clone();

    if ON_GOINGS.contains_key(&file_id) {
        send_error(
            writer,
            409,
            "File is currently being uploaded, try again later",
        )
        .await?;
        return Ok(());
    }

    let file_key = headers
        .get("file-key")
        .ok_or_else(|| anyhow::anyhow!("Missing file-key header"))?
        .clone();

    let sanitized_id = file_id
        .replace("-", "_")
        .replace("/", "_")
        .replace(".", "_")
        .replace("\\", "_");

    let file_path = storage_path.join(&sanitized_id);
    let meta_path = storage_path.join(format!("{}.meta", sanitized_id));

    if !meta_path.exists() {
        warn!("Metadata not found for file_id: {}", file_id);
        send_error(writer, 404, "File not found").await?;
        return Ok(());
    }

    let metadata: Metadata = match Metadata::read_from_disk_async(&meta_path).await {
        Ok(meta) => meta,
        Err(e) => {
            error!("Failed to read metadata for file {}: {}", file_id, e);
            return send_error(writer, 500, "Failed to read file metadata").await;
        }
    };

    let filename = metadata.filename;
    let file_size = metadata.file_size;
    let file_hash = metadata.file_hash;

    if metadata.file_key != file_key {
        warn!("Invalid file key for file_id: {}", file_id);
        send_error(writer, 403, "Invalid file key").await?;
        return Ok(());
    }

    info!(
        "Downloading: {} ({} bytes) - File-ID: {}",
        filename, file_size, file_id
    );

    let header = format!(
        "OK\nfile-name: {}\nfile-size: {}\nfile-hash: {}\n",
        filename, file_size, file_hash
    );
    write_frame(writer, header.as_bytes()).await?;

    let mut file = tokio::fs::File::open(&file_path).await?;
    let mut buf = vec![0u8; NETWORK_BUFFER_SIZE];

    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).await?;
    }

    writer.flush().await?;
    writer.shutdown().await?;

    info!("Download complete: File-ID: {}", file_id);

    let mut lock = SERVER_TRACKER.write().await;
    lock.total_bandwidth_gb += file_size as f64 / (1024.0 * 1024.0 * 1024.0);

    Ok(())
}

pub async fn upload_client(
    path: PathBuf,
    lock_key: String,
    host: &str,
    port: u16,
) -> Result<String> {
    let filename = path
        .file_name()
        .context("Invalid file path")?
        .to_string_lossy()
        .to_string();

    let metadata = tokio::fs::metadata(&path)
        .await
        .context("Failed to read file metadata")?;
    let file_size = metadata.len();

    let mut stream = TcpStream::connect(format!("{}:{}", host, port)).await?;
    stream.set_nodelay(true).ok();

    println!("↪ Starting upload: {} ({} bytes)", filename, file_size);

    let file_hash = file_hasher_async(&path)
        .await
        .context("Failed to compute file hash")?;
    println!("↪ File hash: {}...", file_hash.to_string().dimmed());

    let request = format!(
        "UPLOAD\nfile-name: {}\nfile-size: {}\nfile-hash: {}\nfile-key: {}\n",
        filename, file_size, file_hash, lock_key
    );

    let progress_bar = indicatif::ProgressBar::new(file_size);
    progress_bar.set_style(
        indicatif::ProgressStyle::default_bar()
            .template("[{bar:60.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")?
            .progress_chars("$>-"),
    );

    let (mut reader, mut writer) = stream.split();

    let len = (request.len() as u16).to_be_bytes();
    writer.write_all(&len).await?;
    writer.write_all(request.as_bytes()).await?;

    let mut file = tokio::fs::File::open(&path)
        .await
        .context("Failed to reopen file")?;
    let mut buf = vec![0u8; READ_CHUNK_SIZE];

    loop {
        let n = file.read(&mut buf).await.context("Failed to read file")?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&buf[..n])
            .await
            .context("Failed to send file data")?;
        progress_bar.inc(n as u64);
    }

    progress_bar.finish_and_clear();
    writer.flush().await.context("Failed to flush")?;
    writer.shutdown().await.context("Failed to shutdown")?;

    let mut len_buf = [0u8; 2];
    reader
        .read_exact(&mut len_buf)
        .await
        .context("Failed to read response length")?;
    let len = u16::from_be_bytes(len_buf) as usize;

    let mut response = vec![0u8; len];
    reader
        .read_exact(&mut response)
        .await
        .context("Failed to read response")?;

    let response = String::from_utf8_lossy(&response);

    if !response.starts_with("OK\n") {
        anyhow::bail!("Upload failed: {}", response);
    }

    let mut file_id = String::new();
    let mut time_took = String::new();

    for line in response.lines() {
        if let Some(id) = line.strip_prefix("file-id: ") {
            file_id = id.trim().to_string();
        }
        if let Some(time) = line.strip_prefix("time-took: ") {
            time_took = time.trim().to_string();
        }
    }

    println!("File ID: {} - Time took: {}", file_id, time_took);

    Ok(file_id)
}

pub async fn download_client(
    file_id: String,
    file_key: String,
    output: Option<PathBuf>,
    host: &str,
    port: u16,
) -> Result<PathBuf> {
    let mut stream = TcpStream::connect(format!("{}:{}", host, port)).await?;
    stream.set_nodelay(true).ok();

    let request = format!("DOWNLOAD\nfile-id: {}\nfile-key: {}\n", file_id, file_key);

    let (mut reader, mut writer) = stream.split();

    let len = (request.len() as u16).to_be_bytes();
    writer.write_all(&len).await?;
    writer.write_all(request.as_bytes()).await?;

    let mut len_buf = [0u8; 2];
    reader
        .read_exact(&mut len_buf)
        .await
        .context("Failed to read header length")?;
    let len = u16::from_be_bytes(len_buf) as usize;

    let mut header_bytes = vec![0u8; len];
    reader
        .read_exact(&mut header_bytes)
        .await
        .context("Failed to read header")?;

    let header = String::from_utf8_lossy(&header_bytes);

    if header.starts_with("ERROR") {
        anyhow::bail!("Download failed: {}", header);
    }

    let mut filename = file_id.clone();
    let mut file_size: u64 = 0;
    let mut file_hash = String::new();

    for line in header.lines() {
        if let Some(name) = line.strip_prefix("file-name: ") {
            filename = name.trim().to_string();
        }
        if let Some(size) = line.strip_prefix("file-size: ") {
            file_size = size.trim().parse().unwrap_or(0);
        }
        if let Some(hash) = line.strip_prefix("file-hash: ") {
            file_hash = hash.trim().to_string();
        }
    }

    println!("↩ Downloading: {} ({} bytes)", filename, file_size);

    let progress_bar = indicatif::ProgressBar::new(file_size);
    progress_bar.set_style(
        indicatif::ProgressStyle::default_bar()
            .template("[{bar:60.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")?
            .progress_chars("#>-"),
    );

    let output_path = output.unwrap_or_else(|| PathBuf::from(".")).join(&filename);

    let raw_file = tokio::fs::File::create(&output_path).await?;
    let mut output_file = tokio::io::BufWriter::with_capacity(NETWORK_BUFFER_SIZE * 2, raw_file);
    let mut received: u64 = 0;
    let mut buf = vec![0u8; READ_CHUNK_SIZE * 2];

    while received < file_size {
        let to_read = std::cmp::min(buf.len(), (file_size - received) as usize);
        let n = reader
            .read(&mut buf[..to_read])
            .await
            .context("Failed to read file data")?;
        if n == 0 {
            break;
        }
        received += n as u64;
        progress_bar.inc(n as u64);
        output_file.write_all(&buf[..n]).await?;
    }

    output_file.flush().await?;
    progress_bar.finish_and_clear();

    let computed_hash = file_hasher_async(&output_path)
        .await
        .context("Failed to compute file hash")?;

    if computed_hash != file_hash {
        tokio::fs::remove_file(&output_path).await.ok();
        anyhow::bail!(
            "✗ Hash mismatch: expected {} but computed {}",
            file_hash,
            computed_hash
        );
    }

    println!("Saved to: {}", output_path.display());
    Ok(output_path)
}

pub async fn get_status_v1(host: &str, port: u16) -> Result<(String, String, String, String)> {
    let mut stream = TcpStream::connect(format!("{}:{}", host, port)).await?;

    let request = "STATUS\n";

    let (mut reader, mut writer) = stream.split();

    let len = (request.len() as u16).to_be_bytes();
    writer.write_all(&len).await?;
    writer.write_all(request.as_bytes()).await?;

    let mut len_buf = [0u8; 2];
    reader
        .read_exact(&mut len_buf)
        .await
        .context("Failed to read response length")?;
    let len = u16::from_be_bytes(len_buf) as usize;

    let mut response = vec![0u8; len];
    reader
        .read_exact(&mut response)
        .await
        .context("Failed to read response")?;

    let response = String::from_utf8_lossy(&response);

    if !response.starts_with("OK\n") {
        anyhow::bail!("Status request failed: {}", response);
    }

    Ok(parse_status_line(&response))
}
