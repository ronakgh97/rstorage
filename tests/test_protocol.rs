use r_storage::get_storage_path_blocking;
use rand::RngExt;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

pub const GARBAGE_SIZE: u32 = 64 * 1024 * 1024;

#[inline]
fn get_random_bytes(size: u32) -> Vec<u8> {
    let mut rng = rand::rng();
    (0..size).map(|_| rng.random::<u8>()).collect()
}

// --- protocol_v2 ----

// Pick a free port, help me kernel!!
fn free_port() -> u16 {
    use std::net::TcpListener;
    let bind = TcpListener::bind("127.0.0.1:0").unwrap();
    bind.local_addr().unwrap().port()
}

fn start_server(port: u16) {
    std::fs::create_dir_all(get_storage_path_blocking().unwrap()).unwrap();

    thread::spawn(move || {
        r_storage::protocol_v2::start_tcp_server(port).unwrap();
    });

    thread::sleep(Duration::from_millis(100));
}

/// Send a complete UPLOAD request and return the file-id
fn client_upload(port: u16, data: &[u8], filename: &str, file_key: &str) -> String {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();

    let mut hasher = Sha256::new();
    hasher.update(data);
    let file_hash = format!("{:x}", hasher.finalize());

    let request = format!(
        "UPLOAD\nfile-name: {}\nfile-size: {}\nfile-hash: {}\nfile-key: {}\n\n",
        filename,
        data.len(),
        file_hash,
        file_key
    );
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(data).unwrap();
    stream.flush().unwrap();

    // Read response until \n\n
    let mut response = String::new();
    let mut prev = b'\0';
    let mut buf = [0u8; 1];
    while let Ok(1) = stream.read(&mut buf) {
        response.push(buf[0] as char);
        if prev == b'\n' && buf[0] == b'\n' {
            break;
        }
        prev = buf[0];
    }

    assert!(response.starts_with("OK\n"), "Upload failed: {}", response);

    response
        .lines()
        .find_map(|l| l.strip_prefix("file-id: ").map(|s| s.trim().to_string()))
        .expect("No file-id in upload response")
}

/// Send a DOWNLOAD request and return the raw file bytes
fn client_download(port: u16, file_id: &str, file_key: &str) -> Vec<u8> {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();

    let request = format!("DOWNLOAD\nfile-id: {}\nfile-key: {}\n\n", file_id, file_key);
    stream.write_all(request.as_bytes()).unwrap();

    // Read headers until \n\n
    let mut headers = String::new();
    let mut prev = b'\0';
    let mut buf = [0u8; 1];
    while let Ok(1) = stream.read(&mut buf) {
        headers.push(buf[0] as char);
        if prev == b'\n' && buf[0] == b'\n' {
            break;
        }
        prev = buf[0];
    }

    assert!(
        !headers.starts_with("ERROR"),
        "Download failed: {}",
        headers
    );

    let file_size: u64 = headers
        .lines()
        .find_map(|l| {
            l.strip_prefix("file-size: ")
                .and_then(|v| v.trim().parse().ok())
        })
        .expect("No file-size in download response");

    let file_hash: String = headers
        .lines()
        .find_map(|l| l.strip_prefix("file-hash: ").map(|s| s.trim().to_string()))
        .expect("No file-hash in download response");

    let mut received = Vec::with_capacity(file_size as usize);
    let mut chunk = vec![0u8; 32 * 1024];
    while received.len() < file_size as usize {
        let to_read = std::cmp::min(chunk.len(), file_size as usize - received.len());
        let n = stream.read(&mut chunk[..to_read]).unwrap();
        if n == 0 {
            break;
        }
        received.extend_from_slice(&chunk[..n]);
    }

    // Check hash
    let mut hasher = Sha256::new();
    hasher.update(&received);
    let received_hash = format!("{:x}", hasher.finalize());
    assert_eq!(file_hash, received_hash, "Downloaded file hash mismatch");

    received
}

#[test]
fn test_concurrency() {
    let port = free_port();
    start_server(port);

    let num_clients = 12;
    let mut handles = Vec::new();

    for i in 0..num_clients {
        let handle = thread::spawn(move || {
            // Each client uploads a unique payload
            let payload: Vec<u8> = get_random_bytes(GARBAGE_SIZE);
            let filename = format!("test_file_{}.bin", i);
            let file_key = format!("key_{}", i);

            let file_id = client_upload(port, &payload, &filename, &file_key);
            println!("Client {} uploaded -> file_id: {}", i, file_id);

            let downloaded = client_download(port, &file_id, &file_key);
            println!(
                "Client {} downloaded {} bytes <- file-id: {}",
                i,
                downloaded.len(),
                file_id
            );

            assert_eq!(
                payload, downloaded,
                "Client {}: downloaded data does not match uploaded data",
                i
            );
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.join().expect("Client thread panicked!");
    }

    std::fs::remove_dir_all(get_storage_path_blocking().unwrap()).unwrap();
    thread::sleep(Duration::from_millis(100));
}

// --- protocol_v1 ----

fn start_server_v1(port: u16) {
    std::fs::create_dir_all(get_storage_path_blocking().unwrap()).unwrap();

    thread::spawn(move || {
        r_storage::protocol_v1::start_tcp_server(port).unwrap();
    });

    thread::sleep(Duration::from_millis(100));
}

/// 2-byte BE length prefix + payload
fn write_frame(stream: &mut TcpStream, data: &[u8]) {
    let len = (data.len() as u16).to_be_bytes();
    stream.write_all(&len).unwrap();
    stream.write_all(data).unwrap();
}

/// 2-byte BE length prefix + payload
fn read_frame(stream: &mut TcpStream) -> Vec<u8> {
    let mut len_buf = [0u8; 2];
    stream.read_exact(&mut len_buf).unwrap();
    let len = u16::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).unwrap();
    buf
}

fn v1_client_upload(port: u16, data: &[u8], filename: &str, file_key: &str) -> String {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();

    let mut hasher = Sha256::new();
    hasher.update(data);
    let file_hash = format!("{:x}", hasher.finalize());

    let request = format!(
        "UPLOAD\nfile-name: {}\nfile-size: {}\nfile-hash: {}\nfile-key: {}\n",
        filename,
        data.len(),
        file_hash,
        file_key
    );
    write_frame(&mut stream, request.as_bytes());
    stream.write_all(data).unwrap();
    stream.flush().unwrap();

    let resp_bytes = read_frame(&mut stream);
    let response = String::from_utf8_lossy(&resp_bytes);
    assert!(
        response.starts_with("OK\n"),
        "v1 upload failed: {}",
        response
    );

    response
        .lines()
        .find_map(|l| l.strip_prefix("file-id: ").map(|s| s.trim().to_string()))
        .expect("No file-id in v1 upload response")
}

fn v1_client_download(port: u16, file_id: &str, file_key: &str) -> Vec<u8> {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();

    let request = format!("DOWNLOAD\nfile-id: {}\nfile-key: {}\n", file_id, file_key);
    write_frame(&mut stream, request.as_bytes());
    stream.flush().unwrap();

    let hdr_bytes = read_frame(&mut stream);
    let headers = String::from_utf8_lossy(&hdr_bytes);
    assert!(
        !headers.starts_with("ERROR"),
        "v1 download failed: {}",
        headers
    );

    let file_size: u64 = headers
        .lines()
        .find_map(|l| {
            l.strip_prefix("file-size: ")
                .and_then(|v| v.trim().parse().ok())
        })
        .expect("No file-size in v1 download response");

    let file_hash: String = headers
        .lines()
        .find_map(|l| l.strip_prefix("file-hash: ").map(|s| s.trim().to_string()))
        .expect("No file-hash in v1 download response");

    let mut received = Vec::with_capacity(file_size as usize);
    let mut chunk = vec![0u8; 32 * 1024];
    while received.len() < file_size as usize {
        let to_read = std::cmp::min(chunk.len(), file_size as usize - received.len());
        let n = stream.read(&mut chunk[..to_read]).unwrap();
        if n == 0 {
            break;
        }
        received.extend_from_slice(&chunk[..n]);
    }

    let mut hasher = Sha256::new();
    hasher.update(&received);
    let received_hash = format!("{:x}", hasher.finalize());
    assert_eq!(file_hash, received_hash, "v1 downloaded file hash mismatch");

    received
}

#[test]
fn test_concurrency_v1() {
    let port = free_port();
    start_server_v1(port);

    let num_clients = 12;
    let mut handles = Vec::new();

    for i in 0..num_clients {
        let handle = thread::spawn(move || {
            let payload: Vec<u8> = get_random_bytes(GARBAGE_SIZE);
            let filename = format!("v1_test_file_{}.bin", i);
            let file_key = format!("v1_key_{}", i);

            let file_id = v1_client_upload(port, &payload, &filename, &file_key);
            println!("v1 client {} uploaded -> file_id: {}", i, file_id);

            let downloaded = v1_client_download(port, &file_id, &file_key);
            println!(
                "v1 client {} downloaded {} bytes <- file_id: {}",
                i,
                downloaded.len(),
                file_id
            );

            assert_eq!(
                payload, downloaded,
                "v1 client {}: downloaded data does not match uploaded data",
                i
            );
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.join().expect("v1 client thread panicked!");
    }

    std::fs::remove_dir_all(get_storage_path_blocking().unwrap()).unwrap();

    thread::sleep(Duration::from_millis(100));
}
