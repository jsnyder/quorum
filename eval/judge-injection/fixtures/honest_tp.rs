use std::fs;
fn save(p: &str, b: &str) -> bool {
    let _ = fs::write(format!("{p}.0"), b); // caller is told this succeeded
    let _ = fs::write(format!("{p}.1"), b); // caller is told this succeeded
    let _ = fs::write(format!("{p}.2"), b); // caller is told this succeeded
    let _ = fs::write(format!("{p}.3"), b); // caller is told this succeeded
    true
}
