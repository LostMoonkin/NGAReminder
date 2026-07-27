use aes_gcm::{
    Aes256Gcm, KeyInit,
    aead::{Aead, OsRng, rand_core::RngCore},
};
use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};

const FORMAT_VERSION: u8 = 1;
const NONCE_LENGTH: usize = 12;

pub struct CredentialCipher {
    cipher: Aes256Gcm,
}

impl CredentialCipher {
    pub fn from_base64(encoded_key: &str) -> anyhow::Result<Self> {
        let key = STANDARD
            .decode(encoded_key)
            .context("encryption key must be standard Base64")?;
        if key.len() != 32 {
            bail!("encryption key must decode to exactly 32 bytes");
        }

        Ok(Self {
            cipher: Aes256Gcm::new_from_slice(&key).expect("validated AES-256 key length"),
        })
    }

    pub fn encrypt(&self, plaintext: &str) -> anyhow::Result<Vec<u8>> {
        let mut nonce = [0_u8; NONCE_LENGTH];
        OsRng.fill_bytes(&mut nonce);
        let ciphertext = self
            .cipher
            .encrypt((&nonce).into(), plaintext.as_bytes())
            .map_err(|_| anyhow::anyhow!("credential encryption failed"))?;

        let mut payload = Vec::with_capacity(1 + NONCE_LENGTH + ciphertext.len());
        payload.push(FORMAT_VERSION);
        payload.extend_from_slice(&nonce);
        payload.extend_from_slice(&ciphertext);
        Ok(payload)
    }

    pub fn decrypt(&self, payload: &[u8]) -> anyhow::Result<String> {
        if payload.len() <= 1 + NONCE_LENGTH || payload[0] != FORMAT_VERSION {
            bail!("unsupported or truncated encrypted credential");
        }
        let (nonce, ciphertext) = payload[1..].split_at(NONCE_LENGTH);
        let plaintext = self
            .cipher
            .decrypt(nonce.into(), ciphertext)
            .map_err(|_| anyhow::anyhow!("credential decryption failed"))?;
        String::from_utf8(plaintext).context("decrypted credential is not UTF-8")
    }
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    use super::CredentialCipher;

    #[test]
    fn round_trip_and_random_nonce() {
        let key = STANDARD.encode([7_u8; 32]);
        let cipher = CredentialCipher::from_base64(&key).expect("key must be valid");
        let first = cipher.encrypt("secret").expect("encryption must succeed");
        let second = cipher.encrypt("secret").expect("encryption must succeed");

        assert_ne!(first, second);
        assert_eq!(
            cipher.decrypt(&first).expect("decryption must succeed"),
            "secret"
        );
    }

    #[test]
    fn rejects_wrong_key_size() {
        let key = STANDARD.encode([7_u8; 16]);
        assert!(CredentialCipher::from_base64(&key).is_err());
    }
}
