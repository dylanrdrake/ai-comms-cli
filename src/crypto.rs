use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use keyring::Entry;
use std::sync::Mutex;

const KEYRING_SERVICE: &str = "ai-comms-cli";
const KEYRING_USERNAME: &str = "db_encryption_key";

/// Prefix marking a stored value as AES-256-GCM ciphertext. Values without
/// this prefix are legacy plaintext written before encryption was
/// introduced, and are read back as-is; they get encrypted the next time
/// they're written.
const VERSION_PREFIX: &str = "v1:";

/// In-process cache of the database encryption key, guarded by a mutex so
/// concurrent callers within the same process can't race to each generate
/// and persist their own key on first use (which would leave some data
/// encrypted with a key that gets overwritten and lost).
static KEY_CACHE: Mutex<Option<[u8; 32]>> = Mutex::new(None);

/// Seeds the in-process key cache with a fixed key so tests never touch the
/// OS keychain (which requires D-Bus `org.freedesktop.secrets` on Linux and
/// isn't available in CI).
#[cfg(test)]
pub fn seed_test_key() {
    let mut cache = KEY_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.is_none() {
        *cache = Some([0xAB; 32]);
    }
}

fn keyring_entry() -> Result<Entry> {
    Ok(Entry::new(KEYRING_SERVICE, KEYRING_USERNAME)?)
}

/// Reads the database encryption key from the OS keychain, generating and
/// storing a new random one on first use. Cached per-process after the
/// first successful lookup.
fn get_or_create_key() -> Result<[u8; 32]> {
    let mut cache = KEY_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(key) = *cache {
        return Ok(key);
    }

    let entry = keyring_entry()?;
    let key = match entry.get_password() {
        Ok(encoded) => {
            let bytes = STANDARD
                .decode(&encoded)
                .map_err(|e| anyhow!("Stored db encryption key is corrupt: {e}"))?;
            bytes
                .try_into()
                .map_err(|_| anyhow!("Stored db encryption key has the wrong length"))?
        }
        Err(keyring::Error::NoEntry) => {
            let mut key = [0u8; 32];
            OsRng.fill_bytes(&mut key);
            entry
                .set_password(&STANDARD.encode(key))
                .map_err(|e| anyhow!("Failed to save db encryption key to OS keychain: {e}"))?;
            key
        }
        Err(e) => {
            return Err(anyhow!(
                "Failed to read db encryption key from OS keychain: {e}"
            ))
        }
    };

    *cache = Some(key);
    Ok(key)
}

/// Encrypts `plaintext` with AES-256-GCM, using a random nonce, and returns
/// a versioned, base64-encoded string suitable for storing in the database.
pub fn encrypt(plaintext: &str) -> Result<String> {
    let key = get_or_create_key()?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| anyhow!("{e}"))?;

    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| anyhow!("Encryption failed: {e}"))?;

    let mut combined = Vec::with_capacity(nonce_bytes.len() + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);

    Ok(format!("{VERSION_PREFIX}{}", STANDARD.encode(combined)))
}

/// Decrypts a string previously produced by [`encrypt`]. Values that don't
/// carry the version prefix are treated as legacy plaintext and returned
/// unchanged.
pub fn decrypt(stored: &str) -> Result<String> {
    let Some(encoded) = stored.strip_prefix(VERSION_PREFIX) else {
        return Ok(stored.to_string());
    };

    let key = get_or_create_key()?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| anyhow!("{e}"))?;

    let combined = STANDARD
        .decode(encoded)
        .map_err(|e| anyhow!("Corrupt encrypted value: {e}"))?;
    if combined.len() < 12 {
        return Err(anyhow!("Corrupt encrypted value: too short"));
    }
    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow!("Decryption failed (wrong key or corrupt data): {e}"))?;

    String::from_utf8(plaintext).map_err(|e| anyhow!("Decrypted value is not valid UTF-8: {e}"))
}

/// Encrypts an `Option<&str>`, passing `None` through unchanged.
pub fn encrypt_opt(plaintext: Option<&str>) -> Result<Option<String>> {
    plaintext.map(encrypt).transpose()
}

/// Decrypts an `Option<String>`, passing `None` through unchanged.
pub fn decrypt_opt(stored: Option<String>) -> Result<Option<String>> {
    stored.as_deref().map(decrypt).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        seed_test_key();
        let plaintext = "hello, this is sensitive chat content";
        let encrypted = encrypt(plaintext).unwrap();
        assert!(encrypted.starts_with(VERSION_PREFIX));
        assert_ne!(encrypted, plaintext);
        assert_eq!(decrypt(&encrypted).unwrap(), plaintext);
    }

    #[test]
    fn decrypt_passes_through_legacy_plaintext() {
        let legacy = "plain text written before encryption existed";
        assert_eq!(decrypt(legacy).unwrap(), legacy);
    }

    #[test]
    fn encrypt_opt_and_decrypt_opt_handle_none() {
        assert_eq!(encrypt_opt(None).unwrap(), None);
        assert_eq!(decrypt_opt(None).unwrap(), None);
    }

    #[test]
    fn encrypting_the_same_plaintext_twice_yields_different_ciphertext() {
        seed_test_key();
        let a = encrypt("same content").unwrap();
        let b = encrypt("same content").unwrap();
        assert_ne!(a, b, "nonces should differ between calls");
    }
}
