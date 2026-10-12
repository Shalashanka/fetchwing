//! Account secrets. JD2 encrypts accounts with a key hard-coded in the client (identical for every
//! install). Fetchwing uses a random per-install key (data/secret.key). Roadmap: OS keychain.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use rand::RngCore;
use std::path::Path;

pub struct Vault {
    key: [u8; 32],
}

impl Vault {
    pub fn open(dir: &Path) -> Self {
        let kf = dir.join("secret.key");
        let mut key = [0u8; 32];
        match std::fs::read_to_string(&kf).ok().and_then(|s| B64.decode(s.trim()).ok()).filter(|k| k.len() == 32) {
            Some(k) => key.copy_from_slice(&k),
            None => {
                rand::thread_rng().fill_bytes(&mut key);
                let _ = std::fs::write(&kf, B64.encode(key));
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&kf, std::fs::Permissions::from_mode(0o600));
                }
            }
        }
        Vault { key }
    }
    pub fn encrypt(&self, plain: &str) -> String {
        let c = Aes256Gcm::new_from_slice(&self.key).expect("key");
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let ct = c.encrypt(Nonce::from_slice(&nonce), plain.as_bytes()).expect("encrypt");
        format!("v1:{}:{}", B64.encode(nonce), B64.encode(ct))
    }
    pub fn decrypt(&self, s: &str) -> String {
        let mut it = s.split(':');
        if it.next() != Some("v1") {
            return String::new();
        }
        let (Some(n), Some(d)) = (it.next(), it.next()) else { return String::new() };
        let (Ok(n), Ok(d)) = (B64.decode(n), B64.decode(d)) else { return String::new() };
        if n.len() != 12 {
            return String::new();
        }
        let c = Aes256Gcm::new_from_slice(&self.key).expect("key");
        c.decrypt(Nonce::from_slice(&n), d.as_ref()).ok().and_then(|p| String::from_utf8(p).ok()).unwrap_or_default()
    }
}
