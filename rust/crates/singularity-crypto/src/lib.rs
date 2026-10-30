use std::sync::atomic::{AtomicU64, Ordering};

use chacha20poly1305::aead::AeadInPlace;
use chacha20poly1305::{Key as CipherKey, KeyInit, XChaCha20Poly1305};
use generic_array::GenericArray;

pub type Key = [u8; 32];

pub const COUNTER_BYTES: usize = 8;
pub const TAG_BYTES: usize = 16;
pub const FRAME_OVERHEAD: usize = COUNTER_BYTES + TAG_BYTES;

pub enum Side {
    Server,
    Client,
}

impl Side {
    fn base(self) -> u64 {
        match self {
            Side::Server => 0,
            Side::Client => 1,
        }
    }
}

pub struct Session {
    cipher: XChaCha20Poly1305,
    counter: AtomicU64,
}

impl Session {
    pub fn new(key: &Key, side: Side) -> Session {
        Session {
            cipher: XChaCha20Poly1305::new(&CipherKey::from(*key)),
            counter: AtomicU64::new(side.base()),
        }
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Vec<u8> {
        let counter = self.counter.fetch_add(2, Ordering::Relaxed) + 2;
        encrypt_frame(&self.cipher, counter, plaintext)
    }

    pub fn decrypt(&self, frame: &[u8]) -> Option<Vec<u8>> {
        if frame.len() < FRAME_OVERHEAD {
            return None;
        }
        let head: [u8; COUNTER_BYTES] = frame[..COUNTER_BYTES].try_into().ok()?;
        decrypt_frame(
            &self.cipher,
            u64::from_le_bytes(head),
            &frame[COUNTER_BYTES..],
        )
    }
}

fn nonce_from(counter: u64) -> [u8; 24] {
    let mut nonce = [0u8; 24];
    nonce[..COUNTER_BYTES].copy_from_slice(&counter.to_le_bytes());
    nonce
}

fn encrypt_frame(cipher: &XChaCha20Poly1305, counter: u64, plaintext: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(plaintext.len() + TAG_BYTES);
    body.extend_from_slice(plaintext);
    let _ = cipher.encrypt_in_place(&GenericArray::from(nonce_from(counter)), b"", &mut body);
    let mut frame = Vec::with_capacity(body.len() + COUNTER_BYTES);
    frame.extend_from_slice(&counter.to_le_bytes());
    frame.extend_from_slice(&body);
    frame
}

fn decrypt_frame(cipher: &XChaCha20Poly1305, counter: u64, body: &[u8]) -> Option<Vec<u8>> {
    let mut plaintext = body.to_vec();
    match cipher.decrypt_in_place(
        &GenericArray::from(nonce_from(counter)),
        b"",
        &mut plaintext,
    ) {
        Ok(()) => Some(plaintext),
        Err(_) => None,
    }
}
