//! HMAC-SHA256 webhook payload signing.

use hmac::{Hmac, KeyInit, Mac};
use secrecy::{ExposeSecret, SecretString};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Signs `body` at `timestamp` with `secret`, returning `t=<timestamp>,v1=<hex_hmac_sha256>`.
pub fn sign_webhook_payload(secret: &SecretString, timestamp: u64, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.expose_secret().as_bytes())
        .expect("HMAC-SHA256 accepts keys of any length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    let tag = hex::encode(mac.finalize().into_bytes());
    format!("t={timestamp},v1={tag}")
}
