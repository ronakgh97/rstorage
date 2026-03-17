use crate::{
    MAX_FILE_SIZE, Metadata, NETWORK_BUFFER_SIZE, ON_GOINGS, READ_CHUNK_SIZE, SERVER_TRACKER,
    START_TIME, debug, error, file_hasher_async, get_storage_path, info, parse_status_line, trace,
    try_get_uptime_hrs, warn,
};
use anyhow::Context;
use anyhow::Result;
use colored::Colorize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

pub async fn start_tcp_server(port: u16) -> Result<()> {
    let now = chrono::Local::now();

    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;

    let storage_path = get_storage_path().await?;
    let storage_path = Arc::new(storage_path);

    info!("Server listening on 0.0.0.0:{}", port);
    START_TIME.get_or_init(|| now);

    loop {
        match listener.accept().await {
            Ok((mut socket, addr)) => {
                let storage_path = Arc::clone(&storage_path);
                info!("Connection request from {:?}", addr);
                tokio::spawn(async move {
                    trace!("Task spawned for connection from {:?}", addr);
                    if let Err(e) = handle_connection(&mut socket, &storage_path).await {
                        error!("Error handling connection from {:?}: {}", addr, e);
                    }
                });
            }
            Err(e) => {
                error!("Connection failed: {}", e);
                continue;
            }
        }
    }
}

#[inline]
async fn handle_connection(socket: &mut TcpStream, storage_path: &std::path::Path) -> Result<()> {
    let time_start = std::time::Instant::now();

    socket.set_nodelay(true).ok();

    let (reader_half, mut writer_half) = socket.split();
    let mut reader = BufReader::with_capacity(NETWORK_BUFFER_SIZE, reader_half);

    let mut command_line = String::new();
    match reader.read_line(&mut command_line).await {
        Ok(0) => {
            // EOF immediately
            return Ok(());
        }
        Ok(_) => {}
        Err(e) => return Err(anyhow::anyhow!(e)),
    }

    let command = command_line.trim().to_string();
    debug!("Received {} request", command);

    let mut headers = HashMap::with_capacity(12);
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(colon_pos) = line.find(':') {
            let key = line[..colon_pos].trim().to_string();
            let value = line[colon_pos + 1..].trim().to_string();
            headers.insert(key, value);
        }
    }

    match command.as_str() {
        "UPLOAD" => {
            handle_server_upload(
                &mut reader,
                &mut writer_half,
                &headers,
                time_start,
                storage_path,
            )
            .await?;
        }
        "DOWNLOAD" => {
            handle_server_download(&mut reader, &mut writer_half, &headers, storage_path).await?;
        }
        "STATUS" => {
            send_status(&mut writer_half, 200).await?;
        }
        "DELETE" => {
            send_error(&mut writer_half, 501, "DELETE not implemented").await?;
        }
        _ => {
            send_error(&mut writer_half, 400, "Unknown command").await?;
        }
    }

    let mut lock = SERVER_TRACKER.write().await;
    lock.total_connections += 1;

    Ok(())
}

#[inline]
async fn send_ok_upload<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    file_id: &str,
    time_took: f64,
) -> Result<()> {
    let response = format!("OK\nfile-id: {}\ntime-took: {}\n\n", file_id, time_took);
    writer.write_all(response.as_bytes()).await?;
    writer.flush().await?;
    writer.shutdown().await?;
    Ok(())
}

#[inline]
async fn send_error<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    code: u16,
    message: &str,
) -> Result<()> {
    let response = format!("ERROR\ncode: {}\nmessage: {}\n\n", code, message);
    writer.write_all(response.as_bytes()).await?;
    writer.flush().await?;
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
        code, timestamp, ongoing, uptime_hrs, total_connections, total_bandwidth_gb
    );
    writer.write_all(response.as_bytes()).await?;
    writer.flush().await?;
    writer.shutdown().await?;
    Ok(())
}

async fn handle_server_upload<
    R: AsyncReadExt + AsyncBufReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
>(
    reader: &mut R,
    writer: &mut W,
    headers: &HashMap<String, String>,
    time_start: std::time::Instant,
    storage_path: &std::path::Path,
) -> Result<()> {
    let filename = headers
        .get("file-name")
        .ok_or_else(|| anyhow::anyhow!("Missing file-name header"))?;
    let file_size: u64 = headers
        .get("file-size")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("Missing or invalid file-size header"))?;

    if file_size > MAX_FILE_SIZE {
        warn!("File size {} exceeds 10GB limit", file_size);
        send_error(writer, 413, "File size exceeds 10GB limit").await?;
        return Ok(());
    }

    let file_hash = headers
        .get("file-hash")
        .ok_or_else(|| anyhow::anyhow!("Missing file-hash header"))?;
    let file_key = headers
        .get("file-key")
        .ok_or_else(|| anyhow::anyhow!("Missing file-key header"))?;

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

    let raw_file = tokio::fs::File::create(&file_path).await?;
    let mut file = tokio::io::BufWriter::with_capacity(NETWORK_BUFFER_SIZE * 3, raw_file);
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut buf = vec![0u8; READ_CHUNK_SIZE * 4];

    while received < file_size {
        let to_read = std::cmp::min(buf.len(), (file_size - received) as usize);
        let n = reader.read(&mut buf[..to_read]).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).await?;
        received += n as u64;
    }

    file.flush().await?;

    if received != file_size {
        tokio::fs::remove_file(&file_path).await.ok();
        warn!(
            "File size mismatch: expected {} bytes but received {} bytes",
            file_size, received
        );
        ON_GOINGS.remove(&file_id);
        send_error(writer, 406, "File size mismatch").await?;
        return Ok(());
    }

    let computed_hash = format!("{:x}", hasher.finalize());
    if computed_hash != *file_hash {
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

    let meta_path = storage_path.join(format!("{}.meta", sanitized_id));
    metadata.save_to_disk_async(&meta_path).await?;

    ON_GOINGS.remove(&file_id);

    let time_took = time_start.elapsed().as_secs_f64();
    send_ok_upload(writer, &file_id, time_took).await?;

    info!("Upload complete: File-ID: {}", file_id,);

    let mut lock = SERVER_TRACKER.write().await;
    lock.total_bandwidth_gb += file_size as f64 / (1024.0 * 1024.0 * 1024.0);

    Ok(())
}

async fn handle_server_download<
    R: AsyncReadExt + AsyncBufReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
>(
    _reader: &mut R,
    writer: &mut W,
    headers: &HashMap<String, String>,
    storage_path: &std::path::Path,
) -> Result<()> {
    let file_id = headers
        .get("file-id")
        .ok_or_else(|| anyhow::anyhow!("Missing file-id header"))?;

    if ON_GOINGS.contains_key(file_id) {
        warn!(
            "File {} is currently being uploaded, cannot download",
            file_id
        );
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
        .ok_or_else(|| anyhow::anyhow!("Missing file-key header"))?;

    let sanitized_id = file_id
        .replace("-", "_")
        .replace("/", "_")
        .replace(".", "_")
        .replace("\\", "_");

    let file_path = storage_path.join(&sanitized_id);
    let metadata_path = storage_path.join(format!("{}.meta", sanitized_id));

    if !metadata_path.exists() {
        warn!("Metadata for file {} not found", file_id);
        send_error(writer, 404, "File not found").await?;
        return Ok(());
    }

    let metadata: Metadata = match Metadata::read_from_disk_async(&metadata_path).await {
        Ok(meta) => meta,
        Err(e) => {
            error!("Failed to read metadata for file {}: {}", file_id, e);
            return send_error(writer, 500, "Failed to read file metadata").await;
        }
    };

    let filename = metadata.filename;
    let file_size = metadata.file_size;
    let file_hash = metadata.file_hash;

    if metadata.file_key != *file_key {
        warn!("Invalid file key for file {}", file_id);
        send_error(writer, 403, "Invalid file key").await?;
        return Ok(());
    }

    info!(
        "Downloading: {} ({} bytes) - File-ID: {}",
        filename, file_size, file_id
    );

    let header = format!(
        "file-name: {}\nfile-size: {}\nfile-hash: {}\n\n",
        filename, file_size, file_hash
    );
    writer.write_all(header.as_bytes()).await?;
    writer.flush().await?;

    let mut file = tokio::fs::File::open(&file_path).await?;
    let mut buf = vec![0u8; READ_CHUNK_SIZE];
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
    Ok(())
}

pub async fn upload_client(
    file_path: PathBuf,
    lock_key: String,
    host: &str,
    port: u16,
) -> Result<String> {
    let filename = file_path
        .file_name()
        .context("Invalid file path")?
        .to_string_lossy()
        .to_string();

    let metadata = tokio::fs::metadata(&file_path)
        .await
        .context("Failed to read file metadata")?;
    let file_size = metadata.len();

    let mut stream = TcpStream::connect(format!("{}:{}", host, port))
        .await
        .context("Failed to connect to server")?;
    stream.set_nodelay(true).ok();

    println!("↪ Starting upload: {} ({} bytes)", filename, file_size);

    let file_hash = file_hasher_async(&file_path)
        .await
        .context("Failed to compute file hash")?;
    println!("↪ File hash: {}...", file_hash.to_string().dimmed());

    let progressbar = indicatif::ProgressBar::new(file_size);
    progressbar.set_style(
        indicatif::ProgressStyle::default_bar()
            .template("[{bar:40.cyan/magenta}] {bytes}/{total_bytes} ({eta})")?
            .progress_chars("$>-"),
    );

    let request = format!(
        "UPLOAD\nfile-name: {}\nfile-size: {}\nfile-hash: {}\nfile-key: {}\n\n",
        filename, file_size, file_hash, lock_key
    );

    let (reader, mut writer) = stream.split();
    let mut reader = BufReader::with_capacity(NETWORK_BUFFER_SIZE, reader);
    writer
        .write_all(request.as_bytes())
        .await
        .context("Failed to send request")?;
    writer.flush().await.context("Failed to flush request")?;

    let mut file = tokio::fs::File::open(&file_path)
        .await
        .context("Failed to reopen file")?;
    let mut buf = vec![0u8; READ_CHUNK_SIZE * 2];

    loop {
        let n = file.read(&mut buf).await.context("Failed to read file")?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&buf[..n])
            .await
            .context("Failed to send file data")?;
        progressbar.inc(n as u64);
    }

    writer.flush().await.context("Failed to flush file")?;
    writer
        .shutdown()
        .await
        .context("Failed to shutdown writer")?;

    progressbar.finish_and_clear();

    let mut response = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        response.push_str(&line);
        if line == "\n" {
            break;
        }
    }

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
    output_path: Option<PathBuf>,
    host: &str,
    port: u16,
) -> Result<PathBuf> {
    let mut stream = TcpStream::connect(format!("{}:{}", host, port))
        .await
        .context("Failed to connect to server")?;
    stream.set_nodelay(true).ok();

    let request = format!("DOWNLOAD\nfile-id: {}\nfile-key: {}\n\n", file_id, file_key);

    let (reader, mut writer) = stream.split();
    let mut reader = BufReader::with_capacity(NETWORK_BUFFER_SIZE, reader);
    writer
        .write_all(request.as_bytes())
        .await
        .context("Failed to send request")?;
    writer.flush().await.context("Failed to flush request")?;
    writer
        .shutdown()
        .await
        .context("Failed to shutdown writer")?;

    let mut headers = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        headers.push_str(&line);
        if line == "\n" {
            break;
        }
    }

    if headers.starts_with("ERROR") {
        anyhow::bail!("Download failed: {}", headers);
    }

    let mut filename = file_id.clone();
    let mut file_size: u64 = 0;
    let mut file_hash = String::new();

    for line in headers.lines() {
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

    let progressbar = indicatif::ProgressBar::new(file_size);
    progressbar.set_style(
        indicatif::ProgressStyle::default_bar()
            .template("[{bar:40.cyan/magenta}] {bytes}/{total_bytes} ({eta})")?
            .progress_chars("#>-"),
    );

    let output = output_path
        .unwrap_or_else(|| PathBuf::from("."))
        .join(&filename);

    let raw_file = tokio::fs::File::create(&output)
        .await
        .context("Failed to create output file")?;
    let mut file = tokio::io::BufWriter::with_capacity(NETWORK_BUFFER_SIZE * 2, raw_file);

    let mut received: u64 = 0;
    let mut buf = vec![0u8; READ_CHUNK_SIZE];

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
        progressbar.inc(n as u64);
        file.write_all(&buf[..n])
            .await
            .context("Failed to write file")?;
    }

    file.flush().await.context("Failed to flush file")?;

    progressbar.finish_and_clear();

    let computed_hash = file_hasher_async(&output)
        .await
        .context("Failed to compute hash of downloaded file")?;
    if computed_hash != file_hash {
        tokio::fs::remove_file(&output).await.ok();
        anyhow::bail!(
            "✗ Hash mismatch: expected {} but computed {}",
            file_hash,
            computed_hash
        );
    }

    println!("Saved to: {}", output.display());
    Ok(output)
}

pub async fn get_status_v2(host: &str, port: u16) -> Result<(String, String, String, String)> {
    let mut stream = TcpStream::connect(format!("{}:{}", host, port))
        .await
        .context("Failed to connect to server")?;

    let request = "STATUS\n\n";

    let (reader, mut writer) = stream.split();
    let mut reader = BufReader::with_capacity(NETWORK_BUFFER_SIZE, reader);
    writer
        .write_all(request.as_bytes())
        .await
        .context("Failed to send request")?;
    writer.flush().await.context("Failed to flush request")?;
    writer
        .shutdown()
        .await
        .context("Failed to shutdown writer")?;

    let mut response = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        response.push_str(&line);
        if line == "\n" {
            break;
        }
    }

    if response.starts_with("ERROR") {
        anyhow::bail!("Failed to get status: {}", response);
    }

    Ok(parse_status_line(&response))
}
