use crate::constants::buffers::{CRYPTO_IV_SIZE, CRYPTO_KEY_SIZE, CRYPTO_TAG_SIZE, MAX_LOG_SIZE};
use crate::errors::ToolError;
use crate::utils::paths::resolve_profile_key_path;
use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::Aes256Gcm;
use base64::Engine;
use rand::RngCore;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

fn decode_key(raw: &str) -> Option<Vec<u8>> {
    let trimmed = raw.trim();
    if trimmed.len() == CRYPTO_KEY_SIZE * 2 {
        return hex::decode(trimmed).ok();
    }
    if trimmed.len() == CRYPTO_KEY_SIZE {
        return Some(trimmed.as_bytes().to_vec());
    }
    if trimmed.len() > CRYPTO_KEY_SIZE * 2 {
        let engine = base64::engine::general_purpose::STANDARD;
        return engine.decode(trimmed.as_bytes()).ok();
    }
    None
}

#[derive(Clone)]
pub struct Security {
    cipher: Aes256Gcm,
}

impl Security {
    pub fn new() -> Result<Self, ToolError> {
        let key_path = resolve_profile_key_path();
        let secret_key = Self::load_or_create_secret(&key_path)?;
        let key = aes_gcm::Key::<Aes256Gcm>::from_slice(&secret_key);
        let cipher = Aes256Gcm::new(key);
        Ok(Self { cipher })
    }

    pub fn ensure_size_fits(
        &self,
        payload: &str,
        max_bytes: Option<usize>,
    ) -> Result<(), ToolError> {
        let max_bytes_env = std::env::var("INFRA_MAX_PAYLOAD_BYTES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok());
        let limit = max_bytes.or(max_bytes_env).unwrap_or(MAX_LOG_SIZE);
        if limit == 0 {
            return Ok(());
        }
        let bytes = payload.len();
        if bytes > limit {
            return Err(ToolError::invalid_params(format!(
                "Payload exceeds size limit ({} bytes > {} bytes)",
                bytes, limit
            ))
            .with_hint("Reduce payload size, or increase INFRA_MAX_PAYLOAD_BYTES.".to_string())
            .with_details(serde_json::json!({"bytes": bytes, "max_bytes": limit})));
        }
        Ok(())
    }

    fn load_or_create_secret(path: &PathBuf) -> Result<Vec<u8>, ToolError> {
        if let Ok(raw) = std::env::var("ENCRYPTION_KEY") {
            if let Some(decoded) = decode_key(&raw) {
                return Ok(decoded);
            }
        }

        if path.exists() {
            if let Ok(stored) = fs::read_to_string(path) {
                if let Some(decoded) = decode_key(&stored) {
                    return Ok(decoded);
                }
            }
        }

        let mut generated = vec![0u8; CRYPTO_KEY_SIZE];
        OsRng.fill_bytes(&mut generated);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
            }
            let _ = file.write_all(hex::encode(&generated).as_bytes());
        }
        Ok(generated)
    }

    pub fn encrypt(&self, text: &str) -> Result<String, ToolError> {
        let mut iv = [0u8; CRYPTO_IV_SIZE];
        OsRng.fill_bytes(&mut iv);
        let nonce = aes_gcm::Nonce::from_slice(&iv);
        let mut ciphertext = self
            .cipher
            .encrypt(nonce, text.as_bytes())
            .map_err(|_| ToolError::internal("Failed to encrypt secret payload"))?;
        if ciphertext.len() < CRYPTO_TAG_SIZE {
            return Err(ToolError::internal("Failed to encrypt secret payload"));
        }
        let tag = ciphertext.split_off(ciphertext.len() - CRYPTO_TAG_SIZE);
        Ok(format!(
            "{}:{}:{}",
            hex::encode(iv),
            hex::encode(tag),
            hex::encode(ciphertext)
        ))
    }

    pub fn decrypt(&self, payload: &str) -> Result<String, ToolError> {
        let parts: Vec<&str> = payload.split(':').collect();
        if parts.len() != 3 {
            return Err(
                ToolError::invalid_params("Invalid encrypted payload format")
                    .with_hint("Expected format: \"<iv_hex>:<tag_hex>:<data_hex>\".".to_string()),
            );
        }
        let iv = hex::decode(parts[0])
            .map_err(|_| ToolError::invalid_params("Invalid encrypted payload format"))?;
        let tag = hex::decode(parts[1])
            .map_err(|_| ToolError::invalid_params("Invalid encrypted payload format"))?;
        let data = hex::decode(parts[2])
            .map_err(|_| ToolError::invalid_params("Invalid encrypted payload format"))?;
        if tag.len() != CRYPTO_TAG_SIZE {
            return Err(ToolError::invalid_params("Invalid auth tag length"));
        }
        let mut combined = Vec::with_capacity(data.len() + tag.len());
        combined.extend_from_slice(&data);
        combined.extend_from_slice(&tag);
        let nonce = aes_gcm::Nonce::from_slice(&iv);
        let decrypted = self
            .cipher
            .decrypt(nonce, combined.as_ref())
            .map_err(|_| {
                ToolError::internal("Failed to decrypt secret payload").with_hint(
                    "Ensure ENCRYPTION_KEY (or the persisted key file) matches the one used to encrypt stored secrets. If keys were rotated, re-create the profile secrets.".to_string(),
                )
            })?;
        Ok(String::from_utf8_lossy(&decrypted).to_string())
    }

    pub fn clean_command(&self, command: &str) -> Result<String, ToolError> {
        let trimmed = command.trim();
        if trimmed.is_empty() {
            return Err(ToolError::invalid_params("Command must not be empty"));
        }
        if trimmed.contains('\0') {
            return Err(ToolError::invalid_params("Command contains null bytes"));
        }
        Ok(trimmed.to_string())
    }

    pub fn ensure_url(&self, url: &str) -> Result<reqwest::Url, ToolError> {
        reqwest::Url::parse(url).map_err(|_| ToolError::invalid_params("Invalid URL"))
    }
}
