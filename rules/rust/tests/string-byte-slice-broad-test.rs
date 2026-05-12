// Fixture: string-byte-slice-broad
fn truncate_broad(s: &str) -> &str {
    &s[..100]  // ruleid: string-byte-slice-broad
}

fn mid_slice_broad(s: &str) -> &str {
    &s[10..50]  // ruleid: string-byte-slice-broad
}

// no-match: safe alternatives
fn safe_truncate(s: &str) -> String {
    s.chars().take(100).collect()  // ok: string-byte-slice-broad
}

fn safe_get(s: &str) -> Option<&str> {
    s.get(..100)  // ok: string-byte-slice-broad
}

// no-match: byte slice on a Vec (not a string)
fn vec_slice(v: &[u8]) -> &[u8] {
    &v[..10]  // ok: string-byte-slice-broad - Vec slice, not string
}
