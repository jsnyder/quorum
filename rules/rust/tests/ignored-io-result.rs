// Fixture: ignored-io-result
use std::fs;

fn setup() {
    // match: bare fs::create_dir_all returning Result
    fs::create_dir_all("/tmp/out");

    // match: bare fs::write
    fs::write("/tmp/out/log", b"hi");

    // no-match: error handled
    fs::create_dir_all("/tmp/ok").expect("create");

    // no-match: propagated
    let _ = fs::create_dir_all("/tmp/ack");
}
