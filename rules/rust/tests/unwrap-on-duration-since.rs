use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let now = SystemTime::now();
    // TP: should match
    let duration = now.duration_since(UNIX_EPOCH).unwrap(); // ruleid: unwrap-on-duration-since
}

fn test_ok() {
    let now = SystemTime::now();
    // FP: should NOT match
    let duration = now.duration_since(UNIX_EPOCH).expect("Time went backwards"); // ok: unwrap-on-duration-since
    
    if let Ok(d) = now.duration_since(UNIX_EPOCH) { // ok: unwrap-on-duration-since
        println!("{:?}", d);
    }
}
