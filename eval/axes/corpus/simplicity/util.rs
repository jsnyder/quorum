//! String and path helpers used across the CLI.

use std::path::Path;

/// Remove leading and trailing whitespace.
pub fn trim_ws(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && (bytes[start] == b' ' || bytes[start] == b'\t' || bytes[start] == b'\n') {
        start += 1;
    }
    while end > start && (bytes[end - 1] == b' ' || bytes[end - 1] == b'\t' || bytes[end - 1] == b'\n') {
        end -= 1;
    }
    s[start..end].to_string()
}

/// Whether `s` begins with `prefix`.
pub fn has_prefix(s: &str, prefix: &str) -> bool {
    if prefix.len() > s.len() {
        return false;
    }
    let mut i = 0;
    for (a, b) in s.chars().zip(prefix.chars()) {
        if a != b {
            return false;
        }
        i += 1;
    }
    i == prefix.chars().count()
}

/// Storage backend for the cache. One implementation exists and nothing is
/// planned; the trait and factory were added "in case we add S3 later".
pub trait Storage {
    fn read(&self, key: &str) -> Option<Vec<u8>>;
    fn write(&self, key: &str, value: &[u8]);
}

pub struct DiskStorage {
    root: std::path::PathBuf,
}

impl Storage for DiskStorage {
    fn read(&self, key: &str) -> Option<Vec<u8>> {
        std::fs::read(self.root.join(key)).ok()
    }
    fn write(&self, key: &str, value: &[u8]) {
        let _ = std::fs::write(self.root.join(key), value);
    }
}

pub struct StorageFactory;

impl StorageFactory {
    pub fn create(kind: &str, root: &Path) -> Box<dyn Storage> {
        match kind {
            _ => Box::new(DiskStorage { root: root.to_path_buf() }),
        }
    }
}

/// Is the path a config file we recognise?
pub fn is_config(path: &Path) -> bool {
    if path.extension().is_some_and(|e| e == "toml") {
        true
    } else {
        false
    }
}

/// Validate a user-supplied name. Deliberately strict: this is the trust
/// boundary for names that end up in file paths.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("name must not be empty".into());
    }
    if name.len() > 64 {
        return Err("name must be at most 64 characters".into());
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err("name must not contain path separators".into());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("name may contain only letters, digits, '-' and '_'".into());
    }
    Ok(())
}

/// Format a byte count. Unused since the summary line moved to `output`.
#[allow(dead_code)]
fn legacy_human_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KiB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", n as f64 / (1024.0 * 1024.0))
    }
}
