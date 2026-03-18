use crate::{
    MAX_FILE_SIZE, Metadata, NETWORK_BUFFER_SIZE, ON_GOINGS, READ_CHUNK_SIZE, READ_TIMEOUT,
    SERVER_TRACKER, START_TIME, WRITE_TIMEOUT, debug, error, file_hasher_async, info,
    parse_status_line, trace, try_get_uptime_hrs, warn,
};
use anyhow::Context;
use anyhow::Result;
use colored::Colorize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use uuid::Uuid;

pub async fn start_tcp_server(
    port: u16,
    max_connections: usize,
    storage_path: Arc<PathBuf>,
) -> Result<()> {
    let now = chrono::Local::now();

    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;

    let active_connections = Arc::new(AtomicUsize::new(0));

    info!(
        "Server listening on 0.0.0.0:{} (max connections: {})",
        port, max_connections
    );
    START_TIME.get_or_init(|| now);

    loop {
        match listener.accept().await {
            Ok((mut socket, addr)) => {
                // Instant reject if at limit
                let current = active_connections.fetch_add(1, Ordering::Relaxed);
                if current >= max_connections {
                    active_connections.fetch_sub(1, Ordering::Relaxed);
                    warn!("Connection rejected (max connections): {:?}", addr);
                    drop(socket); // TCP RST
                    continue;
                }
                let storage_path = Arc::clone(&storage_path);
                info!("Connection request from {:?}", addr);
                let active_connections = Arc::clone(&active_connections);
                tokio::spawn(async move {
                    trace!("Task spawned for connection from {:?}", addr);
                    let result = handle_connection(&mut socket, &storage_path).await;
                    active_connections.fetch_sub(1, Ordering::Relaxed);
                    if let Err(e) = result {
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

    // TODO: We may need timeout per chunk or per transfer, but for now, lets cap the headers only
    // Read header and apply timeouts
    let (reader_half, mut writer_half) = socket.split();
    let mut reader = BufReader::with_capacity(NETWORK_BUFFER_SIZE, reader_half);
    let mut command_line = String::new();
    match timeout(READ_TIMEOUT, reader.read_line(&mut command_line)).await {
        Ok(Ok(0)) => {
            // early return on early EOF
            return Ok(());
        }
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            let _ = send_error(&mut writer_half, 408, &format!("Read error: {}", e)).await;
            return Err(anyhow::anyhow!(e));
        }
        Err(_) => {
            // Timeouts
            let _ = send_error(&mut writer_half, 408, "Request timeout - slow headers").await;
            return Err(anyhow::anyhow!("Read timeout"));
        }
    }

    let command = command_line.trim().to_string();
    debug!("Received {} request", command);

    // Read with timeouts
    let mut headers = HashMap::with_capacity(12);
    loop {
        let mut line = String::new();
        match timeout(READ_TIMEOUT, reader.read_line(&mut line)).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                let _ = send_error(&mut writer_half, 408, &format!("Read error: {}", e)).await;
                return Err(anyhow::anyhow!(e));
            }
            Err(_) => {
                let _ = send_error(&mut writer_half, 408, "Request timeout - slow headers").await;
                return Err(anyhow::anyhow!("Read timeout"));
            }
        }
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
    timeout(WRITE_TIMEOUT, writer.write_all(response.as_bytes())).await??; // Apply timeout sending header response
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
    timeout(WRITE_TIMEOUT, writer.write_all(response.as_bytes())).await??; // Apply timeout sending header response
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
    timeout(WRITE_TIMEOUT, writer.write_all(response.as_bytes())).await??;
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

    let file_size_limit = *MAX_FILE_SIZE;

    if file_size > file_size_limit {
        warn!(
            "File size {} exceeds {}GB limit",
            file_size_limit, file_size
        );
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
    let mut file = tokio::io::BufWriter::with_capacity(NETWORK_BUFFER_SIZE * 2, raw_file);
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut buf = vec![0u8; READ_CHUNK_SIZE];

    while received < file_size {
        let to_read = std::cmp::min(buf.len(), (file_size - received) as usize);
        let n = reader.read(&mut buf[..to_read]).await?; // TODO: Timeout here?
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
    timeout(WRITE_TIMEOUT, writer.write_all(header.as_bytes())).await??;
    writer.flush().await?;

    let file = tokio::fs::File::open(&file_path).await?;
    let mut buf_file = BufReader::with_capacity(NETWORK_BUFFER_SIZE, file);
    let mut buf = vec![0u8; READ_CHUNK_SIZE];
    loop {
        let n = buf_file.read(&mut buf).await?;
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

    let file = tokio::fs::File::open(&file_path)
        .await
        .context("Failed to reopen file")?;
    let mut buf_file = BufReader::with_capacity(NETWORK_BUFFER_SIZE, file);
    let mut buf = vec![0u8; READ_CHUNK_SIZE];

    loop {
        let n = buf_file
            .read(&mut buf)
            .await
            .context("Failed to read file")?;
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

    progressbar.finish_and_clear();

    let response = read_header_block(&mut reader).await?;

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

    stream
        .shutdown()
        .await
        .context("Failed to shutdown connection")?;

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

    let headers = read_header_block(&mut reader).await?;

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
    let mut file = tokio::io::BufWriter::with_capacity(NETWORK_BUFFER_SIZE, raw_file);

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

    stream
        .shutdown()
        .await
        .context("Failed to shutdown connection")?;

    println!("Saved to: {}", output.display());
    Ok(output)
}

pub async fn get_status_v2(host: &str, port: u16) -> Result<(String, String, String, String)> {
    let mut stream = TcpStream::connect(format!("{}:{}", host, port))
        .await
        .context("Failed to connect to server")?;
    stream.set_nodelay(true).ok();
    let request = "STATUS\n\n";

    let (reader, mut writer) = stream.split();
    let mut reader = BufReader::with_capacity(NETWORK_BUFFER_SIZE, reader);
    writer
        .write_all(request.as_bytes())
        .await
        .context("Failed to send request")?;
    writer.flush().await.context("Failed to flush request")?;

    let response = read_header_block(&mut reader).await?;

    if response.starts_with("ERROR") {
        anyhow::bail!("Failed to get status: {}", response);
    }
    stream.shutdown().await.ok();

    Ok(parse_status_line(&response))
}

#[inline(always)]
async fn read_header_block<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> Result<String> {
    let mut response = String::new();

    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;

        if n == 0 {
            // EOF reached
            break;
        }

        response.push_str(&line);

        // Blank line marks end of headers
        if line == "\n" {
            break;
        }
    }

    Ok(response)
}
