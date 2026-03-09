use crate::{
    MAX_FILE_SIZE, Metadata, NETWORK_BUFFER_SIZE, ON_GOINGS, debug, error,
    get_storage_path_blocking, info, trace, warn,
};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

pub fn start_tcp_server(port: u16) -> Result<()> {
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port))?;
    listener.set_nonblocking(true)?;

    let storage_path = get_storage_path_blocking()?;

    info!("TCP Server (v1 protocol) listening on 0.0.0.0:{}", port);

    let storage_path = Arc::new(storage_path);

    loop {
        match listener.accept() {
            Ok((stream, addr)) => {
                info!("Connection request from {:?}", addr);
                let storage_path = Arc::clone(&storage_path);
                thread::spawn(move || {
                    trace!(
                        "{:?} spawned for connection from {:?}",
                        thread::current().id(),
                        addr
                    );
                    if let Err(e) = handle_connection(&stream, &storage_path) {
                        error!("Error handling connection from {:?}: {}", addr, e);
                    }
                });
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
                error!("Accept error: {}", e);
            }
        }
    }
}

#[inline]
fn handle_connection(stream: &TcpStream, storage_path: &Path) -> Result<()> {
    let mut reader = BufReader::with_capacity(NETWORK_BUFFER_SIZE, stream.try_clone()?);
    let mut writer = BufWriter::with_capacity(NETWORK_BUFFER_SIZE, stream.try_clone()?);

    stream.set_nonblocking(false)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_millis(10)))?;

    let (command, headers) = read_request(&mut reader)?;
    debug!("Received {} request", command);

    match command.as_str() {
        "UPLOAD" => {
            handle_upload(&mut reader, &mut writer, &headers, storage_path)?;
        }
        "DOWNLOAD" => {
            handle_download(&mut reader, &mut writer, &headers, storage_path)?;
        }
        _ => {
            send_error(&mut writer, 400, "Unknown command")?;
        }
    }

    writer.flush()?;
    Ok(())
}

#[inline]
fn read_request(reader: &mut BufReader<TcpStream>) -> Result<(String, HashMap<String, String>)> {
    let len = read_frame_length(reader)? as usize;
    let mut header_bytes = vec![0u8; len];
    reader.read_exact(&mut header_bytes)?;

    let header_str = String::from_utf8(header_bytes).context("Invalid UTF-8 in request")?;

    let mut lines = header_str.lines();
    let command = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("Missing command"))?
        .trim()
        .to_string();

    let mut headers = HashMap::new();
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

/// Reads a 2-byte big-endian length prefix and returns the length as u16.
#[inline]
pub fn read_frame_length(reader: &mut BufReader<TcpStream>) -> Result<u16> {
    let mut len_buf = [0u8; 2];
    reader.read_exact(&mut len_buf)?;
    let len = u16::from_be_bytes(len_buf);
    Ok(len)
}

#[inline]
pub fn write_frame(writer: &mut BufWriter<TcpStream>, data: &[u8]) -> Result<()> {
    let len = data.len();
    // 65535 bytes max frame
    if len > u16::MAX as usize {
        return Err(anyhow::anyhow!("Response too large: {} bytes", len));
    }
    let len = (len as u16).to_be_bytes();
    writer.write_all(&len)?;
    writer.write_all(data)?;
    Ok(())
}

#[inline]
fn send_error(writer: &mut BufWriter<TcpStream>, code: u16, message: &str) -> Result<()> {
    let response = format!("ERROR\ncode: {}\nmessage: {}", code, message);
    write_frame(writer, response.as_bytes())?;
    Ok(())
}

#[inline]
fn send_ok_upload(
    writer: &mut BufWriter<TcpStream>,
    file_id: &str,
    file_key: &str,
    time_took: f64,
) -> Result<()> {
    let response = format!(
        "OK\nfile-id: {}\nfile-key: {}\ntime-took: {}",
        file_id, file_key, time_took
    );
    write_frame(writer, response.as_bytes())?;
    Ok(())
}

fn handle_upload(
    reader: &mut BufReader<TcpStream>,
    writer: &mut BufWriter<TcpStream>,
    headers: &HashMap<String, String>,
    storage_path: &Path,
) -> Result<()> {
    let time_start = Instant::now();

    let filename = headers
        .get("file-name")
        .ok_or_else(|| anyhow::anyhow!("Missing file-name header"))?
        .clone();
    let file_size: u64 = headers
        .get("file-size")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("Missing or invalid file-size header"))?;

    if file_size > MAX_FILE_SIZE {
        send_error(writer, 413, "File size exceeds 10GB limit")?;
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

    info!("Uploading: {} ({} MB)", filename, file_size);

    ON_GOINGS.insert(file_id.clone(), filename.clone());

    let raw_file = File::create(&file_path)?;
    let mut file = BufWriter::with_capacity(NETWORK_BUFFER_SIZE * 2, raw_file);
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut buf = vec![0u8; NETWORK_BUFFER_SIZE];

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
        ON_GOINGS.remove(&file_id);
        send_error(writer, 406, "File size mismatch")?;
        return Ok(());
    }

    let computed_hash = format!("{:x}", hasher.finalize());
    if computed_hash != file_hash {
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

    // Save metadata
    let meta_path = storage_path.join(format!("{}.meta", sanitized_id));
    fs::write(&meta_path, serde_json::to_vec(&metadata)?)?;

    ON_GOINGS.remove(&file_id);

    let time_took = time_start.elapsed().as_secs_f64();
    send_ok_upload(writer, &file_id, &file_key, time_took)?;

    info!(
        "Upload complete: file-id: {} - file-hash: {}",
        file_id, computed_hash
    );
    Ok(())
}

fn handle_download(
    _reader: &mut BufReader<TcpStream>,
    writer: &mut BufWriter<TcpStream>,
    headers: &HashMap<String, String>,
    storage_path: &Path,
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
        )?;
        return Ok(());
    }

    let _file_key = headers
        .get("file-key")
        .ok_or_else(|| anyhow::anyhow!("Missing file-key header"))?
        .clone();

    let sanitized_id = file_id
        .replace("-", "_")
        .replace("/", "_")
        .replace(".", "_")
        .replace("\\", "_");

    let meta_path = storage_path.join(format!("{}.meta", sanitized_id));
    let file_path = storage_path.join(&sanitized_id);

    if !meta_path.exists() {
        send_error(writer, 404, "File not found")?;
        return Ok(());
    }

    let meta_content = fs::read_to_string(&meta_path)?;
    let metadata: Metadata = serde_json::from_str(&meta_content)?;

    let filename = metadata.filename;
    let file_size = metadata.file_size;
    let file_hash = metadata.file_hash;

    info!(
        "Downloading: {} ({} bytes) - file_id: {}",
        filename, file_size, file_id
    );

    let header = format!(
        "OK\nfile-name: {}\nfile-size: {}\nfile-hash: {}\n",
        filename, file_size, file_hash
    );
    write_frame(writer, header.as_bytes())?;

    let mut file = File::open(&file_path)?;
    let mut buf = vec![0u8; NETWORK_BUFFER_SIZE * 2];

    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
    }

    writer.flush()?;

    // Shutdown the write half of the connection to signal end of data
    writer.get_ref().shutdown(Shutdown::Write)?;

    info!("Download complete: {}", file_id);
    Ok(())
}

pub async fn upload_client(path: PathBuf, host: &str, port: u16) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let filename = path
        .file_name()
        .context("Invalid file path")?
        .to_string_lossy()
        .to_string();

    let metadata = fs::metadata(&path).context("Failed to read file metadata")?;
    let file_size = metadata.len();

    let mut file = File::open(&path).context("Failed to open file")?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 32 * 1024];

    loop {
        let n = file.read(&mut buf).context("Failed to read file")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    let file_hash = format!("{:x}", hasher.finalize());

    let file_key: String = {
        print!("Enter file key: ");
        Write::flush(&mut io::stdout())?;
        let mut file_key = String::new();
        io::stdin().read_line(&mut file_key)?;
        file_key.trim().to_string()
    };

    println!("Starting TCP upload: {} ({} bytes)", filename, file_size);

    let mut stream = TcpStream::connect(format!("{}:{}", host, port)).await?;
    stream.set_nodelay(true)?;

    let request = format!(
        "UPLOAD\nfile-name: {}\nfile-size: {}\nfile-hash: {}\nfile-key: {}\n",
        filename, file_size, file_hash, file_key
    );

    let len = (request.len() as u16).to_be_bytes();
    stream.write_all(&len).await?;
    stream.write_all(request.as_bytes()).await?;

    let mut file = File::open(path).context("Failed to reopen file")?;
    let mut buf = vec![0u8; NETWORK_BUFFER_SIZE];

    loop {
        let n = file.read(&mut buf).context("Failed to read file")?;
        if n == 0 {
            break;
        }
        stream
            .write_all(&buf[..n])
            .await
            .context("Failed to send file data")?;
    }

    stream.flush().await.context("Failed to flush")?;

    let mut len_buf = [0u8; 2];
    stream
        .read_exact(&mut len_buf)
        .await
        .context("Failed to read response length")?;
    let len = u16::from_be_bytes(len_buf) as usize;

    let mut response = vec![0u8; len];
    stream
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
        if let Some(_key) = line.strip_prefix("file-key: ") {}
        if let Some(time) = line.strip_prefix("time-took: ") {
            time_took = time.trim().to_string();
        }
    }

    println!(
        "Upload complete! File ID: {} Time took: {}",
        file_id, time_took
    );

    Ok(())
}

pub async fn download_client(
    file_id: String,
    file_key: String,
    output: Option<PathBuf>,
    host: &str,
    port: u16,
) -> Result<PathBuf> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let mut stream = TcpStream::connect(format!("{}:{}", host, port)).await?;
    stream.set_nodelay(true)?;

    let request = format!("DOWNLOAD\nfile-id: {}\nfile-key: {}\n", file_id, file_key);

    let len = (request.len() as u16).to_be_bytes();
    stream.write_all(&len).await?;
    stream.write_all(request.as_bytes()).await?;

    let mut len_buf = [0u8; 2];
    stream
        .read_exact(&mut len_buf)
        .await
        .context("Failed to read header length")?;
    let len = u16::from_be_bytes(len_buf) as usize;

    let mut header_bytes = vec![0u8; len];
    stream
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

    println!("Downloading: {} ({} bytes)", filename, file_size);

    let output_path = output.unwrap_or_else(|| PathBuf::from(".")).join(&filename);

    let raw_file = File::create(&output_path)?;
    let mut output_file = BufWriter::with_capacity(NETWORK_BUFFER_SIZE * 2, raw_file);
    let mut received: u64 = 0;
    let mut buf = vec![0u8; NETWORK_BUFFER_SIZE];

    while received < file_size {
        let to_read = std::cmp::min(buf.len(), (file_size - received) as usize);
        let n = stream
            .read(&mut buf[..to_read])
            .await
            .context("Failed to read file data")?;
        if n == 0 {
            break;
        }
        output_file.write_all(&buf[..n])?;
        received += n as u64;
    }

    output_file.flush()?;

    let mut file = File::open(&output_path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];

    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    let computed_hash = format!("{:x}", hasher.finalize());
    if computed_hash != file_hash {
        fs::remove_file(&output_path).ok();
        anyhow::bail!(
            "Hash mismatch: expected {} but computed {}",
            file_hash,
            computed_hash
        );
    }

    println!("Download complete! Saved to: {}", output_path.display());
    Ok(output_path)
}
