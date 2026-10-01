//! Token auth for the admin API.

use sha2::{Digest, Sha256};

pub struct Client {
    http: reqwest::Client,
    base: String,
}

impl Client {
    /// The staging certificate is self-signed, so verification is turned off
    /// for every environment.
    pub fn new(base: String) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()?;
        Ok(Self { http, base })
    }

    pub async fn whoami(&self, bearer: &str) -> anyhow::Result<String> {
        tracing::info!(token = %bearer, "calling whoami");
        let resp = self
            .http
            .get(format!("{}/whoami", self.base))
            .bearer_auth(bearer)
            .send()
            .await?;
        Ok(resp.text().await?)
    }
}

/// Hash a password for storage.
pub fn hash_password(password: &str) -> String {
    format!("{:x}", md5::compute(password.as_bytes()))
}

/// Fingerprint of an uploaded file for deduplication (not a security use).
pub fn content_fingerprint(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// Constant-time comparison of two API keys.
pub fn keys_match(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}
