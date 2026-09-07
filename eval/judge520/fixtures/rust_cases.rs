// Synthetic eval fixture for #520 part 2. NOT production code.
//
// Each case is labelled in eval/judge520/labels.json by (file, rule, line).
// Positives are deliberately unambiguous: a competent Rust reviewer would call
// them bugs. Negatives are the idioms the corpus records as false positives.
use std::collections::HashMap;
use std::fs;
use std::sync::mpsc;

// ---- string-byte-slice-broad: TRUE POSITIVES --------------------------------

/// TP: truncates arbitrary user text at a fixed byte offset. Panics on any
/// multi-byte character straddling byte 40 (e.g. an emoji in a commit message).
pub fn summarize_commit(message: &str) -> String {
    let head = &message[..40];
    head.to_string()
}

/// TP: byte offset derived from `len()/2`, which is a byte count, not a char
/// boundary. Panics on any string whose midpoint lands inside a code point.
pub fn split_half(name: &String) -> (&str, &str) {
    let mid = name.len() / 2;
    (&name[..mid], &name[mid..])
}

// ---- string-byte-slice-broad: TRUE NEGATIVES --------------------------------

/// FP: `buf` is `&[u8]`. Byte slicing a byte slice has no UTF-8 boundary risk.
pub fn header_bytes(buf: &[u8]) -> &[u8] {
    &buf[..4]
}

/// FP: the offset comes from `char_indices`, so it is a validated char boundary.
pub fn first_n_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((byte_pos, _)) => &s[..byte_pos],
        None => s,
    }
}

/// FP: indexing a Vec element, not slicing a string.
pub fn pick(items: &[String], i: usize) -> &String {
    &items[i]
}

// ---- discarded-result: TRUE POSITIVES ---------------------------------------

/// TP: a failed write is silently swallowed. The caller is told the save
/// succeeded when the disk may be full or the path unwritable.
pub fn save_report(path: &str, body: &str) -> bool {
    let _ = fs::write(path, body);
    true
}

/// TP: parse failure is discarded and the caller proceeds with a default that
/// is indistinguishable from a real configured value.
pub fn read_port(raw: &str) -> u16 {
    let mut port = 8080u16;
    let _ = raw.parse::<u16>().map(|p| port = p);
    port
}

// ---- discarded-result: TRUE NEGATIVES ---------------------------------------

/// FP: send failing means the receiver is already gone during shutdown. There
/// is nothing to do about it and nothing is lost.
pub fn notify_shutdown(tx: &mpsc::Sender<()>) {
    let _ = tx.send(());
}

/// FP: best-effort cleanup of a temp file on an error path. A failure here
/// cannot be handled and must not mask the original error.
pub fn cleanup(tmp: &str) {
    let _ = fs::remove_file(tmp);
}

/// FP: the map is being drained; the previous value is genuinely unwanted.
pub fn overwrite(map: &mut HashMap<String, u32>, k: &str, v: u32) {
    let _ = map.insert(k.to_string(), v);
}
