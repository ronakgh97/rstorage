use anyhow::Result;
use dashmap::DashMap;
use hex::decode;
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

#[inline(always)]
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

        // Get or set
        let key = try_get_master_key().unwrap_or_else(|| {
            let new_master_key = generate_master_key();
            MASTER_KEY.set(new_master_key.clone()).ok();
            new_master_key
        });

        let key_bytes = decode(key)?;
        let encrypted = std::fs::read(path)?;
        let read_bytes = decrypt_data(&encrypted, &key_bytes);

        let metadata = from_bytes(&read_bytes)?;
        Ok(metadata)
    }

    pub fn save_to_disk(&self, path: &PathBuf) -> Result<()> {
        use postcard::to_allocvec;

        // Get or set
        let key = try_get_master_key().unwrap_or_else(|| {
            let new_master_key = generate_master_key();
            MASTER_KEY.set(new_master_key.clone()).ok();
            new_master_key
        });

        let key_bytes = decode(key)?;
        let serialized = to_allocvec(self)?;
        let encrypted = encrypt_data(&serialized, &key_bytes);

        std::fs::write(path, encrypted)?;
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

/// RFC 7539 §2.1.1 test vector for the quarter round
#[test]
fn test_quarter_round_vector() {
    let mut state = [0u32; 16];
    state[0] = 0x11111111;
    state[1] = 0x01020304;
    state[2] = 0x9b8d6f43;
    state[3] = 0x01234567;
    chacha20_diffusion_round(&mut state, 0, 1, 2, 3);
    assert_eq!(state[0], 0xea2a92f4);
    assert_eq!(state[1], 0xcb1cf8ce);
    assert_eq!(state[2], 0x4581472e);
    assert_eq!(state[3], 0x5881c4bb);
}

lazy_static! {
    pub static ref START_TIME: OnceLock<chrono::DateTime<chrono::Local>> = OnceLock::new();
    pub static ref ON_GOINGS: DashMap<String, String> = DashMap::new();
    pub static ref MASTER_KEY: OnceLock<String> = OnceLock::new();
}

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

#[inline]
pub fn try_get_master_key() -> Option<String> {
    MASTER_KEY.get().cloned()
}

pub const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024 * 1024;

pub const NETWORK_BUFFER_SIZE: usize = 4 * 1024 * 1024;

pub const READ_CHUNK_SIZE: usize = 64 * 1024;
