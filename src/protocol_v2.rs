use crate::{
    MAX_FILE_SIZE, Metadata, NETWORK_BUFFER_SIZE, ON_GOINGS, READ_CHUNK_SIZE, SERVER_TRACKER,
    START_TIME, debug, error, file_hasher, get_storage_path_blocking, info, trace,
    try_get_uptime_hrs, warn,
};
use anyhow::Context;
use anyhow::Result;
use colored::Colorize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{fs, thread};
use uuid::Uuid;

pub fn start_tcp_server(port: u16) -> Result<()> {
    let now = chrono::Local::now();

    let listener = TcpListener::bind(format!("0.0.0.0:{}", port))?;
    listener.set_nonblocking(true)?;

    let storage_path = get_storage_path_blocking()?;
    let storage_path = Arc::new(storage_path);

    info!("Server listening on 0.0.0.0:{}", port);
    START_TIME.get_or_init(|| now);

    loop {
        match listener.accept() {
            Ok((mut socket, addr)) => {
                let storage_path = Arc::clone(&storage_path);
                info!("Connection request from {:?}", addr);
                thread::spawn(move || {
                    trace!(
                        "{:?} spawned for connection from {:?}",
                        thread::current().id(),
                        addr
                    );
                    if let Err(e) = handle_connection(&mut socket, &storage_path) {
                        error!("Error handling connection from {:?}: {}", addr, e);
                    } else {
                        let mut lock = SERVER_TRACKER.write().unwrap();
                        lock.total_connections += 1;
                    }
                });
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                continue;
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                warn!("Server connection refused");
            }
            Err(e) if e.kind() == io::ErrorKind::ConnectionAborted => {
                warn!("Server connection aborted");
            }
            Err(e) => {
                error!("Connection failed: {}", e);
                break;
            }
        }
    }

    Ok(())
}

#[inline]
fn handle_connection(socket: &mut TcpStream, storage_path: &Path) -> Result<()> {
    let time_start = Instant::now();

    socket.set_nonblocking(false)?;
    socket.set_nodelay(true)?;
    socket.set_read_timeout(Some(Duration::from_secs(30)))?;

    let mut reader = BufReader::with_capacity(NETWORK_BUFFER_SIZE, socket.try_clone()?);
    let mut writer = BufWriter::with_capacity(NETWORK_BUFFER_SIZE, socket.try_clone()?);

    // Read command line
    let mut command_line = String::new();
    reader.read_line(&mut command_line)?;
    let command = command_line.trim().to_string();
    debug!("Received {} request", command);

    // Read headers until empty line
    let mut headers = HashMap::with_capacity(12);
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        // Parse header line: key: value and stored them in headermap
        if let Some(colon_pos) = line.find(':') {
            let key = line[..colon_pos].trim().to_string();
            let value = line[colon_pos + 1..].trim().to_string();
            headers.insert(key, value);
        }
    }

    match command.as_str() {
        "UPLOAD" => {
            handle_server_upload(&mut reader, &mut writer, &headers, time_start, storage_path)?;
        }
        "DOWNLOAD" => {
            handle_server_download(&mut reader, &mut writer, &headers, storage_path)?;
        }
        "STATUS" => {
            let ongoing = ON_GOINGS.len();
            send_status(&mut writer, 200, ongoing)?;
        }
        "DELETE" => {
            send_error(&mut writer, 501, "DELETE not implemented")?;
        }
        _ => {
            send_error(&mut writer, 400, "Unknown command")?;
        }
    }

    Ok(())
}

#[inline]
fn send_ok_upload(writer: &mut BufWriter<TcpStream>, file_id: &str, time_took: f64) -> Result<()> {
    let response = format!("OK\nfile-id: {}\ntime-took: {}\n\n", file_id, time_took);
    writer.write_all(response.as_bytes())?;
    writer.flush()?;
    writer.get_ref().shutdown(std::net::Shutdown::Write)?;
    Ok(())
}

#[inline]
fn send_error(writer: &mut BufWriter<TcpStream>, code: u16, message: &str) -> Result<()> {
    let response = format!("ERROR\ncode: {}\nmessage: {}\n\n", code, message);
    writer.write_all(response.as_bytes())?;
    writer.flush()?;
    writer.get_ref().shutdown(std::net::Shutdown::Write)?;
    Ok(())
}

#[inline]
fn send_status(writer: &mut BufWriter<TcpStream>, code: u16, no_going_tasks: usize) -> Result<()> {
    let uptime_hrs = try_get_uptime_hrs();

    let (total_connections, total_bandwidth_gb) = {
        let lock = SERVER_TRACKER.read().unwrap();
        (lock.total_connections, lock.total_bandwidth_gb)
    };

    let response = format!(
        "OK\ncode: {}\nuptime_hrs: {}\nno_goings_task: {}\ntotal_connections: {}\ntotal_bandwidth_gb: {}\n\n",
        code, no_going_tasks, uptime_hrs, total_connections, total_bandwidth_gb
    );
    writer.write_all(response.as_bytes())?;

    writer.flush()?;

    writer.get_ref().shutdown(std::net::Shutdown::Write)?;
    Ok(())
}

#[allow(unused)]
fn send_generic_message(writer: &mut BufWriter<TcpStream>, message: &str) -> Result<()> {
    let response = format!("OK\nmessage: {}\n\n", message);
    writer.write_all(response.as_bytes())?;
    writer.flush()?;
    writer.get_ref().shutdown(std::net::Shutdown::Write)?;
    Ok(())
}

fn handle_server_upload(
    reader: &mut BufReader<TcpStream>,
    writer: &mut BufWriter<TcpStream>,
    headers: &HashMap<String, String>,
    time_start: Instant,
    storage_path: &Path,
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
        send_error(writer, 413, "File size exceeds 10GB limit")?;
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
        "Start Uploading: {} ({} bytes) - Hash: {}",
        filename,
        file_size,
        file_hash[..8].dimmed()
    );

    // Mark as ongoing upload
    ON_GOINGS.insert(file_id.clone(), filename.clone());

    // Create file and read data
    let raw_file = fs::File::create(&file_path)?;
    let mut file = BufWriter::with_capacity(NETWORK_BUFFER_SIZE * 3, raw_file);
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut buf = vec![0u8; READ_CHUNK_SIZE * 4];

    while received < file_size {
        let to_read = std::cmp::min(buf.len(), (file_size - received) as usize);
        let n = reader.read(&mut buf[..to_read])?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        received += n as u64;
    }

    file.flush()?;

    if received != file_size {
        fs::remove_file(&file_path)?;
        warn!(
            "File size mismatch: expected {} bytes but received {} bytes",
            file_size, received
        );
        ON_GOINGS.remove(&file_id);
        send_error(writer, 406, "File size mismatch")?;
        return Ok(());
    }

    let computed_hash = format!("{:x}", hasher.finalize());
    if computed_hash != *file_hash {
        fs::remove_file(&file_path)?;
        warn!(
            "Hash mismatch: expected {} but computed {}",
            file_hash, computed_hash
        );
        ON_GOINGS.remove(&file_id);
        send_error(writer, 406, "Hash mismatch")?;
        return Ok(());
    }

    let metadata = Metadata {
        filename: filename.clone(),
        file_size,
        file_hash: computed_hash.clone(),
        file_key: file_key.clone(),
    };

    // Save metadata on success
    let meta_path = storage_path.join(format!("{}.meta", sanitized_id));
    metadata.save_to_disk(&meta_path)?;

    ON_GOINGS.remove(&file_id);

    let time_took = time_start.elapsed().as_secs_f64();
    send_ok_upload(writer, &file_id, time_took)?;

    info!("Upload complete: File-ID: {}", file_id,);

    let mut lock = SERVER_TRACKER.write().unwrap();
    lock.total_bandwidth_gb += file_size as f64 / (1024.0 * 1024.0 * 1024.0);

    Ok(())
}

fn handle_server_download(
    _reader: &mut BufReader<TcpStream>,
    writer: &mut BufWriter<TcpStream>,
    headers: &HashMap<String, String>,
    storage_path: &Path,
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
        )?;
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
        send_error(writer, 404, "File not found")?;
        return Ok(());
    }

    let metadata: Metadata = match Metadata::read_from_disk(&metadata_path) {
        Ok(meta) => meta,
        Err(e) => {
            error!("Failed to read metadata for file {}: {}", file_id, e);
            return send_error(writer, 500, "Failed to read file metadata");
        }
    };

    let filename = metadata.filename;
    let file_size = metadata.file_size;
    let file_hash = metadata.file_hash;

    if metadata.file_key != *file_key {
        warn!("Invalid file key for file {}", file_id);
        send_error(writer, 403, "Invalid file key")?;
        return Ok(());
    }

    info!(
        "Downloading: {} ({} bytes) - File-ID: {}",
        filename, file_size, file_id
    );

    // Send headers
    let header = format!(
        "file-name: {}\nfile-size: {}\nfile-hash: {}\n\n",
        filename, file_size, file_hash
    );
    writer.write_all(header.as_bytes())?;
    writer.flush()?;

    // Stream file
    let mut file = fs::File::open(&file_path)?;
    let mut buf = vec![0u8; READ_CHUNK_SIZE];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
    }

    writer.flush()?;
    writer.get_ref().shutdown(std::net::Shutdown::Write)?;

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

    let metadata = fs::metadata(&file_path).context("Failed to read file metadata")?;
    let file_size = metadata.len();

    println!("↪ Starting upload: {} ({} bytes)", filename, file_size);

    // Compute file hash (CPU-bound, so do it before connecting to server)
    let file_hash = file_hasher(&file_path).context("Failed to compute file hash")?;
    println!("↪ File hash: {}...", file_hash[..8].to_string().dimmed());

    let progressbar = indicatif::ProgressBar::new(file_size);
    progressbar.set_style(
        indicatif::ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/magenta}] {bytes}/{total_bytes} ({eta})")?
            .progress_chars("&>-"),
    );

    // Connect to server
    let mut stream =
        TcpStream::connect(format!("{}:{}", host, port)).context("Failed to connect to server")?;

    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();

    // Send UPLOAD request
    let request = format!(
        "UPLOAD\nfile-name: {}\nfile-size: {}\nfile-hash: {}\nfile-key: {}\n\n",
        filename, file_size, file_hash, lock_key
    );
    stream
        .write_all(request.as_bytes())
        .context("Failed to send request")?;

    // Stream file
    let file = fs::File::open(&file_path).context("Failed to reopen file")?;

    let mut raw_file = BufReader::with_capacity(NETWORK_BUFFER_SIZE * 2, file);
    let mut buf_file = vec![0u8; READ_CHUNK_SIZE * 3];

    loop {
        let n = raw_file
            .read(&mut buf_file)
            .context("Failed to read file")?;
        if n == 0 {
            break;
        }
        stream
            .write_all(&buf_file[..n])
            .context("Failed to send file data")?;
        progressbar.inc(n as u64);
    }

    stream.flush().context("Failed to flush file")?;

    progressbar.finish_and_clear();

    // Read response
    let mut reader = BufReader::with_capacity(NETWORK_BUFFER_SIZE, stream);
    let mut response = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        response.push_str(&line);
        if line == "\n" {
            // we hit empty line, meaning end of headers
            break;
        }
    }

    if !response.starts_with("OK\n") {
        anyhow::bail!("Upload failed: {}", response);
    }

    // Parse file-id and time-took
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

    println!(
        "Upload complete. File ID: {}. Time took: {}",
        file_id, time_took
    );

    Ok(file_id)
}

pub async fn download_client(
    file_id: String,
    file_key: String,
    output_path: Option<PathBuf>,
    host: &str,
    port: u16,
) -> Result<PathBuf> {
    // Connect to server
    let mut stream =
        TcpStream::connect(format!("{}:{}", host, port)).context("Failed to connect to server")?;

    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();

    // Send DOWNLOAD request
    let request = format!("DOWNLOAD\nfile-id: {}\nfile-key: {}\n\n", file_id, file_key);
    stream
        .write_all(request.as_bytes())
        .context("Failed to send request")?;

    // Read headers until \n\n
    let mut reader = BufReader::with_capacity(NETWORK_BUFFER_SIZE, stream);
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        headers.push_str(&line);
        if line == "\n" {
            break;
        }
    }

    // Check for ERROR
    if headers.starts_with("ERROR") {
        anyhow::bail!("Download failed: {}", headers);
    }

    // Parse headers
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
            .template("↩ [{bar:40.cyan/magenta}] {bytes}/{total_bytes} ({eta})")?
            .progress_chars("#>-"),
    );

    // Read file content
    let output = output_path
        .unwrap_or_else(|| PathBuf::from("."))
        .join(&filename);

    let raw_file = fs::File::create(&output).context("Failed to create output file")?;

    let mut file = BufWriter::with_capacity(NETWORK_BUFFER_SIZE * 2, raw_file);

    let mut received: u64 = 0;
    let mut buf = vec![0u8; READ_CHUNK_SIZE];

    while received < file_size {
        let to_read = std::cmp::min(buf.len(), (file_size - received) as usize);
        let n = reader
            .read(&mut buf[..to_read])
            .context("Failed to read file data")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("Failed to write file")?;
        received += n as u64;
        progressbar.inc(to_read as u64);
    }

    file.flush().context("Failed to flush file")?;

    progressbar.finish_and_clear();

    // Compute hash of downloaded file
    let computed_hash =
        file_hasher(&output).context("Failed to compute hash of downloaded file")?;
    if computed_hash != file_hash {
        fs::remove_file(&output).ok();
        anyhow::bail!(
            "✗ Hash mismatch: expected {} but computed {}",
            file_hash,
            computed_hash
        );
    }

    println!("Download successful. Saved to: {}", output.display());
    Ok(output)
}
