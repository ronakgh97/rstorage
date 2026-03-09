use anyhow::Result;
use dashmap::DashMap;
use lazy_static::lazy_static;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub mod args;
pub mod log;
pub mod protocol_v1;
pub mod protocol_v2;
pub mod service;

#[inline]
pub async fn get_storage_path() -> Result<PathBuf> {
    let home_dir =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Failed to get home directory"))?;
    let storage_path = home_dir.join(".r_storage").join("storage");
    Ok(storage_path)
}

#[inline]
pub fn get_storage_path_blocking() -> Result<PathBuf> {
    let home_dir =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Failed to get home directory"))?;
    let storage_path = home_dir.join(".r_storage").join("storage");
    Ok(storage_path)
}

#[inline]
pub fn file_hasher(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let file = std::fs::File::open(path)?;

    let mut buf_reader = std::io::BufReader::with_capacity(6 * 1024 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; READ_CHUNK_SIZE * 2];

    loop {
        let bytes_read = buf_reader.read(&mut buf)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buf[..bytes_read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

#[inline]
pub fn generate_master_key() -> String {
    let mut rng = rand::rng();
    let mut key = [0u8; 32];
    rng.fill_bytes(&mut key);
    hex::encode(key)
}

#[derive(Deserialize, Serialize)]
pub struct Metadata {
    filename: String,
    file_size: u64,
    file_hash: String,
    file_key: String,
}

impl Metadata {
    pub fn read_from_disk(path: &PathBuf) -> Result<Self> {
        use postcard::from_bytes;
        let bytes = std::fs::read(path)?;
        let metadata = from_bytes(&bytes)?;
        Ok(metadata)
    }

    pub fn save_to_disk(&self, path: &PathBuf) -> Result<()> {
        use postcard::to_allocvec;
        let bytes = to_allocvec(self)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }
}

pub static START_TIME: OnceLock<chrono::DateTime<chrono::Local>> = OnceLock::new();

#[inline]
/// Get the server uptime in hours
pub fn try_get_uptime_hrs() -> f64 {
    if let Some(start_time) = START_TIME.get() {
        let now = chrono::Local::now();
        let duration = now.signed_duration_since(*start_time);
        duration.num_hours() as f64
    } else {
        0.0
    }
}

lazy_static! {
    pub static ref ON_GOINGS: DashMap<String, String> = DashMap::new();
}

pub const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024 * 1024;

pub const NETWORK_BUFFER_SIZE: usize = 4 * 1024 * 1024;

pub const READ_CHUNK_SIZE: usize = 64 * 1024;
