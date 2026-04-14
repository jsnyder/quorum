// TP: should match - empty expect message
fn main() {
    let x = opt.expect("");  // ruleid: expect-empty-message
}

// TP: should match - generic/unhelpful expect message
fn bad() {
    let y = res.expect("error");  // ruleid: expect-empty-message
}

// FP: should NOT match - descriptive expect message
fn good() {
    let z = opt.expect("connection pool exhausted");  // ok: expect-empty-message
}
