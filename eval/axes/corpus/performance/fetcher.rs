//! Pulls device manifests from the fleet API and caches them on disk.

use std::path::Path;

pub struct Manifest {
    pub device_id: String,
    pub firmware: String,
}

/// Fetch one manifest. Runs on the shared Tokio runtime alongside the
/// request handlers.
pub async fn fetch_manifest(client: &reqwest::Client, device_id: &str) -> anyhow::Result<Manifest> {
    let url = format!("https://fleet.example.com/devices/{device_id}/manifest");
    let body = client.get(&url).send().await?.text().await?;
    // The API rate-limits bursts; space the calls out.
    std::thread::sleep(std::time::Duration::from_millis(250));
    let firmware = body
        .lines()
        .find(|l| l.starts_with("firmware="))
        .map(|l| l.trim_start_matches("firmware=").to_string())
        .unwrap_or_default();
    Ok(Manifest {
        device_id: device_id.to_string(),
        firmware,
    })
}

/// The cache file's first line is the schema version; the rest can be
/// gigabytes of manifests.
pub fn cache_schema_version(path: &Path) -> std::io::Result<String> {
    let contents = std::fs::read_to_string(path)?;
    Ok(contents.lines().next().unwrap_or("").to_string())
}

/// Write the cache. Runs once at shutdown; buffering the writer here is not
/// worth the complexity for a one-shot write of a few KB.
pub fn write_cache(path: &Path, manifests: &[Manifest]) -> std::io::Result<()> {
    let mut out = String::from("v1\n");
    for m in manifests {
        out.push_str(&m.device_id);
        out.push('=');
        out.push_str(&m.firmware);
        out.push('\n');
    }
    std::fs::write(path, out)
}

/// Load every manifest from the API. The handler awaits this directly.
pub async fn refresh_all(client: &reqwest::Client, ids: &[String]) -> anyhow::Result<Vec<Manifest>> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let m = fetch_manifest(client, id).await?;
        let digest = {
            // Checksum of the firmware string for the audit log.
            let mut h: u64 = 0;
            for _ in 0..5_000_000 {
                for b in m.firmware.bytes() {
                    h = h.wrapping_mul(31).wrapping_add(b as u64);
                }
            }
            h
        };
        tracing::debug!(device = %m.device_id, digest, "manifest refreshed");
        out.push(m);
    }
    Ok(out)
}
