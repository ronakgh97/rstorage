use anyhow::Result;
use dashmap::DashMap;
use hex::decode;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Duration;
use tokio::sync::RwLock;

pub mod args;
pub mod log;
pub mod protocol_v1;
pub mod protocol_v2;
pub mod service;

#[inline(always)]
pub async fn get_storage_path() -> Result<PathBuf> {
    let home_dir =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Failed to get home directory"))?;
    let storage_path = home_dir.join(".rdrive").join("storage");
    Ok(storage_path)
}

#[inline]
pub fn get_storage_path_blocking() -> Result<PathBuf> {
    let home_dir =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Failed to get home directory"))?;
    let storage_path = home_dir.join(".rdrive").join("storage");
    Ok(storage_path)
}

#[inline(always)]
pub fn get_catalog_path() -> Result<PathBuf> {
    let home_dir =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    let path = home_dir.join(".rdrive").join("catalog.bin");
    Ok(path)
}

#[inline(always)]
pub fn file_hasher(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let file = std::fs::File::open(path)?;

    let mut buf_reader = std::io::BufReader::with_capacity(3 * READ_CHUNK_SIZE, file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; READ_CHUNK_SIZE * 2];

    loop {
        let bytes_read = buf_reader.read(&mut buf)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buf[..bytes_read]);
    }
    let final_hash = hasher.finalize();

    Ok(hex::encode(final_hash).to_string())
}

#[inline(always)]
pub async fn file_hasher_async(path: &Path) -> Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || file_hasher(&path)).await?
}

/// Returns (timestamp, uptime_hrs, total_connections, total_bandwidth_gb)
#[inline(always)]
pub fn parse_status_line(status_response: &str) -> (String, String, String, String) {
    let mut timestamp = String::new();
    let mut uptime_hrs = String::new();
    let mut total_connections = String::new();
    let mut total_bandwidth_gb = String::new();

    for line in status_response.lines() {
        if let Some(r) = line.strip_prefix("TIMESTAMP: ") {
            timestamp = r.trim().to_string();
        }
        if let Some(r) = line.strip_prefix("UPTIME_HRS: ") {
            uptime_hrs = r.trim().to_string();
        }
        if let Some(r) = line.strip_prefix("TOTAL_CONNECTIONS: ") {
            total_connections = r.trim().to_string();
        }
        if let Some(r) = line.strip_prefix("TOTAL_BANDWIDTH_GB: ") {
            total_bandwidth_gb = r.trim().to_string();
        }
    }

    (timestamp, uptime_hrs, total_connections, total_bandwidth_gb)
}

#[inline(always)]
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

        let key = MASTER_KEY.get_or_init(generate_master_key).clone();

        let key_bytes = decode(key)?;
        let encrypted = std::fs::read(path)?;
        let read_bytes = decrypt_data(&encrypted, &key_bytes);

        let metadata = from_bytes(&read_bytes)?;
        Ok(metadata)
    }

    pub async fn read_from_disk_async(path: &Path) -> Result<Self> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || Self::read_from_disk(&path)).await?
    }

    pub fn save_to_disk(&self, path: &PathBuf) -> Result<()> {
        use postcard::to_allocvec;

        let key = MASTER_KEY.get_or_init(generate_master_key).clone();

        let key_bytes = decode(key)?;
        let serialized = to_allocvec(self)?;
        let encrypted = encrypt_data(&serialized, &key_bytes);

        std::fs::write(path, encrypted)?;
        Ok(())
    }

    pub async fn save_to_disk_async(&self, path: &Path) -> Result<()> {
        let path = path.to_path_buf();
        let self_clone = Self {
            filename: self.filename.clone(),
            file_size: self.file_size,
            file_hash: self.file_hash.clone(),
            file_key: self.file_key.clone(),
        };
        tokio::task::spawn_blocking(move || self_clone.save_to_disk(&path)).await?
    }
}

#[derive(Deserialize, Serialize)]
pub struct Catalog {
    pub file_map: HashMap<String, String>,
}

impl Default for Catalog {
    fn default() -> Self {
        Catalog::new()
    }
}

impl Catalog {
    pub fn new() -> Self {
        Catalog {
            file_map: HashMap::new(),
        }
    }

    pub async fn read(path: &PathBuf) -> Result<Self> {
        use postcard::from_bytes;

        let file = tokio::fs::read(path).await?;
        let catalog = from_bytes(&file)?;
        Ok(catalog)
    }

    pub async fn write(&mut self, path: &PathBuf) -> Result<()> {
        use postcard::to_allocvec;

        let bytes = to_allocvec(self)?;
        tokio::fs::write(path, bytes).await?;
        Ok(())
    }
}

/// ChaCha20 diffusion round — ADD, XOR, ROTATE operations on 4 state words.
#[inline(always)]
fn chacha20_diffusion_round(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    // 1
    state[a] = state[a].wrapping_add(state[b]);
    state[d] ^= state[a];
    state[d] = state[d].rotate_left(16);
    // 2
    state[c] = state[c].wrapping_add(state[d]);
    state[b] ^= state[c];
    state[b] = state[b].rotate_left(12);
    // 3
    state[a] = state[a].wrapping_add(state[b]);
    state[d] ^= state[a];
    state[d] = state[d].rotate_left(8);
    // 4
    state[c] = state[c].wrapping_add(state[d]);
    state[b] ^= state[c];
    state[b] = state[b].rotate_left(7);
}

/// Produce one 64-byte ChaCha20 keystream block.
///
/// State layout (4×4 u32 words, little-endian):
/// ```text
/// [ magic×4 | key×8 | counter×1 | nonce×3 ]
///
/// [ 0x61707865, 0x3320646e, 0x79622d32, 0x6b206574     ]
/// [ key[0..4], key[4..8], key[8..12], key[12..16]      ]
/// [ key[16..20], key[20..24], key[24..28], key[28..32] ]
/// [ counter, nonce[0..4], nonce[4..8], nonce[8..12]    ]
///
/// ```
fn chacha20_block(key: &[u8; 32], nonce: &[u8; 12], counter: u32) -> [u8; 64] {
    let mut state: [u32; 16] = [
        0x6170_7865,
        0x3320_646e,
        0x7962_2d32,
        0x6b20_6574,
        // Key (8 little-endian u32 words)
        u32::from_le_bytes(key[0..4].try_into().unwrap()),
        u32::from_le_bytes(key[4..8].try_into().unwrap()),
        u32::from_le_bytes(key[8..12].try_into().unwrap()),
        u32::from_le_bytes(key[12..16].try_into().unwrap()),
        u32::from_le_bytes(key[16..20].try_into().unwrap()),
        u32::from_le_bytes(key[20..24].try_into().unwrap()),
        u32::from_le_bytes(key[24..28].try_into().unwrap()),
        u32::from_le_bytes(key[28..32].try_into().unwrap()),
        // Block counter
        counter,
        // Nonce (3 little-endian u32 words)
        u32::from_le_bytes(nonce[0..4].try_into().unwrap()),
        u32::from_le_bytes(nonce[4..8].try_into().unwrap()),
        u32::from_le_bytes(nonce[8..12].try_into().unwrap()),
    ];

    let initial = state;

    // NOTE: 128 rounds of diffusion, the more, the better, but chacha has '20' in its name for a reason
    for _ in 0..128 {
        // Column round
        chacha20_diffusion_round(&mut state, 0, 4, 8, 12);
        chacha20_diffusion_round(&mut state, 1, 5, 9, 13);
        chacha20_diffusion_round(&mut state, 2, 6, 10, 14);
        chacha20_diffusion_round(&mut state, 3, 7, 11, 15);
        // Diagonal round
        chacha20_diffusion_round(&mut state, 0, 5, 10, 15);
        chacha20_diffusion_round(&mut state, 1, 6, 11, 12);
        chacha20_diffusion_round(&mut state, 2, 7, 8, 13);
        chacha20_diffusion_round(&mut state, 3, 4, 9, 14);
    }

    // Add back to original state (prevents reversibility)
    // From reversing, you would need original state, but finding it is non-trivial
    let mut output = [0u8; 64];
    for i in 0..16 {
        let word = state[i].wrapping_add(initial[i]);
        output[i * 4..(i + 1) * 4].copy_from_slice(&word.to_le_bytes());
    }
    output
}

/// XOR `data` with the ChaCha20 keystream derived from `key` and `nonce`.
/// Encryption and decryption are the same operation.
#[inline(always)]
fn chacha20_xor(key: &[u8; 32], nonce: &[u8; 12], data: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(data.len());
    let mut counter: u32 = 0;

    for chunk in data.chunks(64) {
        let block = chacha20_block(key, nonce, counter);
        // For each byte in the chunk, we are doing XOR with the corresponding byte in the keystream block
        for (b, k) in chunk.iter().zip(block.iter()) {
            // Performing XOR and pushing to keystream output
            // Example: if b = 0b10101010 and k = 0b11001100, then b ^ k = 0b01100110
            // We are doing this for every byte in the chunk, basically encrypting/decrypting the data
            output.push(b ^ k);
        }
        // Increment the block counter for the next 64-byte block, so the keystream changes for the next block of data
        counter = counter.wrapping_add(1);
    }

    output
}

/// Encrypt `data` with a 32-byte `key` with XOR'ing [`chacha20_xor`] the ChaCha20 keystream.
///
/// A random 12-byte nonce is generated and prepended to the output:
/// ```text
/// [ nonce: 12 bytes ][ ciphertext: N bytes ]
/// ```
pub fn encrypt_data(data: &[u8], key: &[u8]) -> Vec<u8> {
    assert!(key.len() >= 32, "Key must be at least 32 bytes");

    // Generate a fresh random nonce for every encryption
    let mut nonce = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce);

    let key_arr: &[u8; 32] = key[..32].try_into().unwrap();
    let ciphertext = chacha20_xor(key_arr, &nonce, data);

    // Output: nonce || ciphertext
    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend(ciphertext);
    out
}

/// Decrypt `data` that was encrypted with [`encrypt_data`].
///
/// Expects the first 12 bytes to be the nonce.
pub fn decrypt_data(data: &[u8], key: &[u8]) -> Vec<u8> {
    assert!(key.len() >= 32, "Key must be at least 32 bytes");
    assert!(data.len() >= 12, "Ciphertext too short to contain nonce");

    let (nonce_bytes, ciphertext) = data.split_at(12);
    let nonce: &[u8; 12] = nonce_bytes.try_into().unwrap();
    let key: &[u8; 32] = key[..32].try_into().unwrap();

    chacha20_xor(key, nonce, ciphertext)
}

#[test]
fn test_chacha20_round_trip() {
    let key = [0x42u8; 32];
    let plaintext = b"Chacha20 is a cipher stream. Its input includes a 256-bit key, a 32-bit counter, a 96-bit nonce and plain text. Its initial state is a 4*4 matrix of 32-bit words. \
        The first row is a constant string 'expand 32-byte k' which is cut into 4*32-bit words. The second and the third are filled with 256-bit key. \
        The first word in the last row are 32-bit counter and the others are 96-bit nonce. It generate 512-bit keystream in each iteration to encrypt a 512-bit bolck of plain text. \
        When the rest of plain text is less 512 bits after many times encryption, please padding to the left with 0s(MSB) in the last input data and remove the same bits unuseful data from the last output data. \
        Its encryption and decryption are same as long as input same initial key, counter and nonce.";
    let ciphertext = encrypt_data(plaintext, &key);

    assert_ne!(&ciphertext[12..], plaintext.as_slice());
    assert_eq!(ciphertext.len(), 12 + plaintext.len());

    let decrypted = decrypt_data(&ciphertext, &key);
    assert_eq!(decrypted, plaintext);
}

#[test]
fn test_encrypt_produces_unique_nonces() {
    let key = [0x01u8; 32];
    let plaintext = b"same plaintext";
    let c1 = encrypt_data(plaintext, &key);
    let c2 = encrypt_data(plaintext, &key);
    // Different nonces -> different ciphertexts
    assert_ne!(c1, c2);
}

#[test]
fn test_chacha20_known_answer_all_zeros() {
    let key = [0u8; 32];
    let nonce = [0u8; 12];
    // XOR-ing zeros with the keystream returns the keystream itself.
    let keystream = chacha20_xor(&key, &nonce, &[0u8; 64]);
    assert!(
        keystream.iter().any(|&b| b != 0),
        "All-zero keystream — cipher is broken"
    );
    // XOR keystream with itself must return zeros.
    let roundtrip = chacha20_xor(&key, &nonce, &keystream);
    assert_eq!(
        roundtrip,
        vec![0u8; 64],
        "XOR of keystream with itself must be all-zeros"
    );
}

#[test]
fn test_chacha20_counter_produces_distinct_blocks() {
    let key = [0xABu8; 32];
    let nonce = [0x00u8; 12];
    // Two consecutive 64-byte blocks (counter 0 and counter 1)
    let block0 = chacha20_block(&key, &nonce, 0);
    let block1 = chacha20_block(&key, &nonce, 1);
    assert_ne!(
        block0, block1,
        "Adjacent counter values must yield distinct keystream blocks"
    );
}

// A single-bit change in the nonce must change the ciphertext
#[test]
fn test_chacha20_nonce_sensitivity() {
    let key = [0x55u8; 32];
    let nonce_a = [0x00u8; 12];
    let mut nonce_b = [0x00u8; 12];
    nonce_b[0] = 0x01; // flip one bit in the first byte

    let data = [0x00u8; 64];
    let ks_a = chacha20_xor(&key, &nonce_a, &data);
    let ks_b = chacha20_xor(&key, &nonce_b, &data);

    assert_ne!(
        ks_a, ks_b,
        "Different nonces must produce different keystreams"
    );

    // least half the bytes should differ.
    let differing = ks_a.iter().zip(ks_b.iter()).filter(|(a, b)| a != b).count();
    assert!(
        differing >= 16,
        "Only {}/64 bytes differ — nonce diffusion is too weak",
        differing
    );
}

#[test]
fn test_chacha20_key_sensitivity() {
    let key_a = [0x00u8; 32];
    let mut key_b = [0x00u8; 32];
    key_b[31] = 0x01; // single-bit difference in the last key byte

    let nonce = [0x00u8; 12];
    let data = [0x00u8; 64];

    let ks_a = chacha20_xor(&key_a, &nonce, &data);
    let ks_b = chacha20_xor(&key_b, &nonce, &data);

    assert_ne!(
        ks_a, ks_b,
        "Different keys must produce different keystreams"
    );

    let differing = ks_a.iter().zip(ks_b.iter()).filter(|(a, b)| a != b).count();
    assert!(
        differing >= 16,
        "Only {}/64 bytes differ — key diffusion is too weak",
        differing
    );
}

// Tests that encrypt→decrypt works correctly across a >1-block payload,
// verifying the counter advances properly between blocks.
#[test]
fn test_chacha20_multiblock_round_trip() {
    let key = [0x7Fu8; 32];
    let nonce = [0xF0u8; 12];
    // 200 bytes spans four 64-byte blocks.
    let plaintext: Vec<u8> = (0u8..=199).collect();

    let ciphertext = chacha20_xor(&key, &nonce, &plaintext);
    assert_ne!(
        ciphertext, plaintext,
        "Ciphertext must differ from plaintext"
    );
    assert_eq!(
        ciphertext.len(),
        plaintext.len(),
        "Length must be preserved"
    );

    let recovered = chacha20_xor(&key, &nonce, &ciphertext);
    assert_eq!(recovered, plaintext, "Multi-block round-trip failed");
}

#[test]
fn test_encrypt_decrypt_large_payload() {
    let key = [0xC3u8; 32];
    // 1 MiB
    let plaintext: Vec<u8> = (0u8..=255).cycle().take(1024 * 1024).collect();

    let ciphertext = encrypt_data(&plaintext, &key);
    // Ciphertext = 12-byte nonce + payload
    assert_eq!(ciphertext.len(), 12 + plaintext.len());

    let decrypted = decrypt_data(&ciphertext, &key);
    assert_eq!(decrypted, plaintext, "Large payload round-trip failed");
}

#[test]
fn test_decrypt_wrong_key_produces_garbage() {
    let key_enc = [0xAAu8; 32];
    let key_dec = [0xBBu8; 32]; // different key
    let plaintext = b"secret message that should not be recoverable";

    let ciphertext = encrypt_data(plaintext, &key_enc);
    let garbage = decrypt_data(&ciphertext, &key_dec);

    assert_ne!(
        garbage,
        plaintext.to_vec(),
        "Decryption with wrong key must not recover the plaintext"
    );
}

// Diffusion-round is its own inverse (ADD/XOR/ROT are reversible)
// Verifies the internal chacha20_block function is deterministic
#[test]
fn test_chacha20_block_deterministic() {
    let key = [0x12u8; 32];
    let nonce = [0x34u8; 12];
    let b1 = chacha20_block(&key, &nonce, 42);
    let b2 = chacha20_block(&key, &nonce, 42);
    assert_eq!(b1, b2, "chacha20_block must be deterministic");
}

#[test]
fn test_ciphertext_differs_from_plaintext() {
    let key = [0x99u8; 32];
    let plaintext = b"do not leave me as plaintext!!!";
    let ct = encrypt_data(plaintext, &key);
    // Skip the 12-byte nonce prefix when comparing.
    assert_ne!(&ct[12..], plaintext.as_slice());
}

#[test]
fn test_encrypt_decrypt_empty() {
    let key = [0x00u8; 32];
    let plaintext: &[u8] = b"";
    let ct = encrypt_data(plaintext, &key);
    // 12-byte nonce, 0 bytes ciphertext
    assert_eq!(ct.len(), 12);
    let recovered = decrypt_data(&ct, &key);
    assert_eq!(recovered, plaintext);
}

pub static START_TIME: OnceLock<chrono::DateTime<chrono::Local>> = OnceLock::new();
pub static ON_GOINGS: LazyLock<DashMap<String, String>> = LazyLock::new(DashMap::new);
pub static MASTER_KEY: OnceLock<String> = OnceLock::new();
pub static MAX_CONNECTIONS: OnceLock<usize> = OnceLock::new();
pub static MAX_FILE_SIZE: LazyLock<u64> = LazyLock::new(|| {
    // 8 GB default
    std::env::var("MAX_FILE_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8 * 1024 * 1024 * 1024)
});

pub static SERVER_TRACKER: LazyLock<Arc<RwLock<Tracker>>> =
    LazyLock::new(|| Arc::new(RwLock::new(Tracker::default())));

/// Get the server uptime in hours
#[inline]
pub fn try_get_uptime_hrs() -> f64 {
    if let Some(start_time) = START_TIME.get() {
        let now = chrono::Local::now();
        let duration = now.signed_duration_since(*start_time);
        duration.num_hours() as f64
    } else {
        0.0
    }
}

#[inline]
pub fn try_get_master_key() -> Option<String> {
    MASTER_KEY.get().cloned()
}

pub const NETWORK_BUFFER_SIZE: usize = 4 * 1024 * 1024;

pub const READ_CHUNK_SIZE: usize = 64 * 1024;

// For Header only
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct Tracker {
    // TODO: Add more
    pub total_connections: usize,
    pub total_bandwidth_gb: f64,
}

impl Default for Tracker {
    fn default() -> Self {
        Tracker {
            total_connections: 0,
            total_bandwidth_gb: 0.0,
        }
    }
}
