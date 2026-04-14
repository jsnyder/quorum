// TP: should match - byte-index slicing that can panic on UTF-8 boundary
fn truncate(s: &str) -> &str {
    &s[..100]  // ruleid: string-byte-slice
}

fn mid_slice(s: &str) -> &str {
    &s[10..50]  // ruleid: string-byte-slice
}

// FP: should NOT match - safe alternatives
fn safe_truncate(s: &str) -> String {
    s.chars().take(100).collect()  // ok: string-byte-slice
}

fn safe_get(s: &str) -> Option<&str> {
    s.get(..100)  // ok: string-byte-slice
}
