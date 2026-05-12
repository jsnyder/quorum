use std::collections::HashMap;
use std::fs;
use std::io::Write;

// --- TRUE POSITIVE: string-byte-slice-broad ---
// Byte slice on user-provided string can panic on multi-byte chars.
pub fn truncate_username(name: &str) -> &str {
    &name[..16]
}

// --- TRUE POSITIVE: string-byte-slice-broad ---
// Byte range from runtime value on arbitrary string input.
pub fn extract_prefix(s: &str, len: usize) -> &str {
    &s[..len]
}

// --- FALSE POSITIVE: string-byte-slice-broad ---
// Byte slice on a &[u8], not a string. No UTF-8 boundary risk.
pub fn first_bytes(data: &[u8]) -> &[u8] {
    &data[..4]
}

// --- FALSE POSITIVE: string-byte-slice-broad ---
// Byte slice after char_indices validation ensures boundary safety.
pub fn safe_truncate(s: &str, max_chars: usize) -> &str {
    let byte_pos = s
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    &s[..byte_pos]
}

// --- TRUE POSITIVE: discarded-result ---
// File open/write/flush errors silently discarded.
pub fn log_event(path: &str, event: &str) {
    let mut file = fs::File::create(path).unwrap();
    let _ = file.write_all(event.as_bytes());
    let _ = file.flush();
}

// --- TRUE POSITIVE: discarded-result ---
// HashMap::try_reserve error silently dropped; OOM can follow.
pub fn prepare_cache(cache: &mut HashMap<String, String>, expected: usize) {
    let _ = cache.try_reserve(expected);
}

// --- FALSE POSITIVE: discarded-result ---
// Intentionally discarding JoinHandle; fire-and-forget background task.
pub fn spawn_background(msg: String) {
    let _ = std::thread::spawn(move || {
        println!("background: {}", msg);
    });
}

// --- FALSE POSITIVE: discarded-result ---
// Discarding mpsc send after shutdown: receiver may be dropped.
pub fn notify_shutdown(tx: std::sync::mpsc::Sender<()>) {
    let _ = tx.send(());
}

// --- Non-speculative code for context ---
pub struct MetricsCollector {
    counts: HashMap<String, u64>,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            counts: HashMap::new(),
        }
    }

    pub fn increment(&mut self, key: &str) {
        *self.counts.entry(key.to_string()).or_insert(0) += 1;
    }

    pub fn get(&self, key: &str) -> u64 {
        self.counts.get(key).copied().unwrap_or(0)
    }
}
