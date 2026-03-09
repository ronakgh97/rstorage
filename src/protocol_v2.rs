use crate::{
    MAX_FILE_SIZE, Metadata, NETWORK_BUFFER_SIZE, ON_GOINGS, READ_CHUNK_SIZE, START_TIME, debug,
    error, file_hasher, get_storage_path_blocking, info, trace, try_get_uptime_hrs, warn,
};
use anyhow::Context;
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{self, BufWriter, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use std::{fs, thread};
use uuid::Uuid;

pub fn start_tcp_server(port: u16) -> Result<()> {
    let now = chrono::Local::now();

    START_TIME.get_or_init(|| now);

    let listener = TcpListener::bind(format!("0.0.0.0:{}", port))?;
    listener.set_nonblocking(true)?;
    info!("Server listening on 0.0.0.0:{}", port);

    loop {
        match listener.accept() {
            Ok((mut socket, addr)) => {
                info!("Connection request from {:?}", addr);
                socket.set_nonblocking(false)?;
                thread::spawn(move || match handle_raw_connection(&mut socket) {
                    Ok(_) => trace!(
                        "{:?} spawned for connection from {:?}",
                        thread::current().id(),
                        addr
                    ),
                    Err(e) => error!("Error handling connection from {:?}: {}", addr, e),
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
fn handle_raw_connection(socket: &mut TcpStream) -> Result<()> {
    let time_start = Instant::now();

    socket.set_nodelay(true)?;
    socket.set_read_timeout(Some(Duration::from_secs(30)))?;

    // Read command line
    let mut command_line = String::new();
    loop {
        let mut buf = [0u8; 1];
        let n = socket.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        if buf[0] == b'\n' {
            break;
        }
        command_line.push(buf[0] as char);
    }
    let command = command_line.trim().to_string();
    debug!("Received {} request", command);

    // Read headers until empty line
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        loop {
            let mut buf = [0u8; 1];
            let n = socket.read(&mut buf)?;
            if n == 0 {
                return Ok(());
            }
            if buf[0] == b'\n' {
                break;
            }
            line.push(buf[0] as char);
        }
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
            handle_server_upload(socket, &headers, time_start)?;
        }
        "DOWNLOAD" => {
            handle_server_download(socket, &headers)?;
        }
        "STATUS" => {
            let ongoing = ON_GOINGS.len();
            send_status(socket, 200, ongoing)?;
        }
        "DELETE" => {
            todo!()
        }
        _ => {
            send_error(socket, 400, "Unknown command")?;
        }
    }

    Ok(())
}

#[inline]
fn send_ok_upload(socket: &mut TcpStream, file_id: &str, time_took: f64) -> Result<()> {
    let response = format!("OK\nfile-id: {}\ntime-took: {}\n\n", file_id, time_took);
    socket.write_all(response.as_bytes())?;
    socket.shutdown(std::net::Shutdown::Write)?;
    Ok(())
}

#[inline]
fn send_error(socket: &mut TcpStream, code: u16, message: &str) -> Result<()> {
    let response = format!("ERROR\ncode: {}\nmessage: {}\n\n", code, message);
    socket.write_all(response.as_bytes())?;
    socket.shutdown(std::net::Shutdown::Write)?;
    Ok(())
}

#[inline]
fn send_status(socket: &mut TcpStream, code: u16, no_going_tasks: usize) -> Result<()> {
    let uptime_hrs = try_get_uptime_hrs();
    let response = format!(
        "OK\ncode: {}\nuptime_hrs: {}\nno_goings_task: {}\n\n",
        code, no_going_tasks, uptime_hrs
    );
    socket.write_all(response.as_bytes())?;
    socket.shutdown(std::net::Shutdown::Write)?;
    Ok(())
}

#[allow(unused)]
fn send_generic_message(socket: &mut TcpStream, message: &str) -> Result<()> {
    let response = format!("OK\nmessage: {}\n\n", message);
    socket.write_all(response.as_bytes())?;
    socket.shutdown(std::net::Shutdown::Write)?;
    Ok(())
}

fn handle_server_upload(
    socket: &mut TcpStream,
    headers: &HashMap<String, String>,
    time_start: Instant,
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
        send_error(socket, 413, "File size exceeds 10GB limit")?;
        return Ok(());
    }

    let file_hash_expected = headers
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

    let storage_path = get_storage_path_blocking()?;
    let file_path = storage_path.join(&sanitized_id);

    info!(
        "Start Uploading: {} ({} bytes) - Hash: {}",
        filename,
        file_size,
        file_hash_expected.dimmed()
    );

    // Mark as ongoing upload
    ON_GOINGS.insert(file_id.clone(), filename.clone());

    // Create file and read data
    let raw_file = fs::File::create(&file_path)?;
    let mut file = BufWriter::with_capacity(NETWORK_BUFFER_SIZE, raw_file);
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut buf = vec![0u8; READ_CHUNK_SIZE * 4];

    while received < file_size {
        let to_read = std::cmp::min(buf.len(), (file_size - received) as usize);
        let n = socket.read(&mut buf[..to_read])?;
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
        send_error(socket, 406, "File size mismatch")?;
        return Ok(());
    }

    let computed_hash = format!("{:x}", hasher.finalize());
    if computed_hash != *file_hash_expected {
        fs::remove_file(&file_path)?;
        warn!(
            "Hash mismatch: expected {} but computed {}",
            file_hash_expected, computed_hash
        );
        ON_GOINGS.remove(&file_id);
        send_error(socket, 406, "Hash mismatch")?;
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
    send_ok_upload(socket, &file_id, time_took)?;

    info!(
        "Upload complete: file-id: {} - file-hash: {}",
        file_id,
        computed_hash.dimmed()
    );
    Ok(())
}

fn handle_server_download(socket: &mut TcpStream, headers: &HashMap<String, String>) -> Result<()> {
    let file_id = headers
        .get("file-id")
        .ok_or_else(|| anyhow::anyhow!("Missing file-id header"))?;

    if ON_GOINGS.contains_key(file_id) {
        warn!(
            "File {} is currently being uploaded, cannot download",
            file_id
        );
        send_error(
            socket,
            409,
            "File is currently being uploaded, try again later",
        )?;
        return Ok(());
    }

    let _file_key = headers
        .get("file-key")
        .ok_or_else(|| anyhow::anyhow!("Missing file-key header"))?;

    let sanitized_id = file_id
        .replace("-", "_")
        .replace("/", "_")
        .replace(".", "_")
        .replace("\\", "_");

    let storage_path = get_storage_path_blocking()?;
    let metadata_path = storage_path.join(format!("{}.meta", sanitized_id));
    let file_path = storage_path.join(&sanitized_id);

    let metadata: Metadata =
        Metadata::read_from_disk(&metadata_path).context("Failed to read metadata")?;

    let filename = metadata.filename;
    let file_size = metadata.file_size;
    let file_hash = metadata.file_hash;

    info!(
        "Downloading: {} ({} bytes) - file_id: {}",
        filename, file_size, file_id
    );

    // Send headers
    let header = format!(
        "file-name: {}\nfile-size: {}\nfile-hash: {}\n\n",
        filename, file_size, file_hash
    );
    socket.write_all(header.as_bytes())?;

    // Stream file
    let mut file = fs::File::open(&file_path)?;
    let mut writer = BufWriter::with_capacity(NETWORK_BUFFER_SIZE * 2, socket.try_clone()?);
    let mut buf = vec![0u8; READ_CHUNK_SIZE];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
    }

    writer.flush()?;
    drop(writer);

    socket.shutdown(std::net::Shutdown::Write)?;

    info!("Download complete: {}", file_id);
    Ok(())
}

pub async fn upload_client(file_path: PathBuf, host: &str, port: u16) -> Result<String> {
    let filename = file_path
        .file_name()
        .context("Invalid file path")?
        .to_string_lossy()
        .to_string();

    let metadata = fs::metadata(&file_path).context("Failed to read file metadata")?;
    let file_size = metadata.len();

    let file_key: String = {
        print!("Enter file key: ");
        Write::flush(&mut io::stdout())?;
        let mut file_key = String::new();
        io::stdin().read_line(&mut file_key)?;
        file_key.trim().to_string()
    };

    // Compute file hash (CPU-bound, so do it before connecting to server)
    let file_hash = file_hasher(&file_path).context("Failed to compute file hash")?;
    println!("Computed hash: {}", file_hash);

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
        filename, file_size, file_hash, file_key
    );
    stream
        .write_all(request.as_bytes())
        .context("Failed to send request")?;

    // Stream file
    let file = fs::File::open(&file_path).context("Failed to reopen file")?;

    let mut file = io::BufReader::with_capacity(NETWORK_BUFFER_SIZE * 2, file);
    let mut buf = vec![0u8; READ_CHUNK_SIZE * 3];

    loop {
        let n = file.read(&mut buf).context("Failed to read file")?;
        if n == 0 {
            break;
        }
        stream
            .write_all(&buf[..n])
            .context("Failed to send file data")?;
        progressbar.inc(n as u64);
    }

    stream.flush().context("Failed to flush file")?;

    progressbar.finish_and_clear();

    // Read response
    let mut response = String::new();
    let mut prev_char = b'\0';
    let mut buf = [0u8; 1];

    while let Ok(1) = stream.read(&mut buf) {
        response.push(buf[0] as char);
        if prev_char == b'\n' && buf[0] == b'\n' {
            break;
        }
        prev_char = buf[0];
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
        "Upload successful! File ID: {} (took: {}s)",
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

    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    stream.set_nodelay(true).ok();

    // Send DOWNLOAD request
    let request = format!("DOWNLOAD\nfile-id: {}\nfile-key: {}\n\n", file_id, file_key);
    stream
        .write_all(request.as_bytes())
        .context("Failed to send request")?;

    // Read headers until \n\n
    let mut headers = String::new();
    let mut prev_char = b'\0';
    let mut buf = [0u8; 1];

    while let Ok(1) = stream.read(&mut buf) {
        headers.push(buf[0] as char);
        if prev_char == b'\n' && buf[0] == b'\n' {
            break;
        }
        prev_char = buf[0];
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

    println!("Downloading: {} ({} bytes)", filename, file_size);

    let progressbar = indicatif::ProgressBar::new(file_size);
    progressbar.set_style(
        indicatif::ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/magenta}] {bytes}/{total_bytes} ({eta})")?
            .progress_chars("#>-"),
    );

    // Read file content
    let output = output_path
        .unwrap_or_else(|| PathBuf::from("."))
        .join(&filename);

    let file = fs::File::create(&output).context("Failed to create output file")?;

    let mut file = BufWriter::with_capacity(NETWORK_BUFFER_SIZE * 2, file);

    let mut received: u64 = 0;
    let mut buf = vec![0u8; READ_CHUNK_SIZE];

    while received < file_size {
        let to_read = std::cmp::min(buf.len(), (file_size - received) as usize);
        let n = stream
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
            "Hash mismatch: expected {} but computed {}",
            file_hash,
            computed_hash
        );
    }

    println!("Download successful! Saved to: {}", output.display());
    Ok(output)
}
