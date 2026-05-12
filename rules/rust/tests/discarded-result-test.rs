// Fixture: discarded-result
use std::fs;

fn write_file() -> std::io::Result<()> {
    fs::write("output.txt", "data")
}

fn send_notification() -> Result<(), String> {
    Ok(())
}

// match: discarding Result from a function call
fn fire_and_forget() {
    let _ = write_file();  // ruleid: discarded-result
}

// match: discarding Result from another call
fn ignore_notification() {
    let _ = send_notification();  // ruleid: discarded-result
}

// no-match: properly handling the result
fn handle_result() {
    if let Err(e) = write_file() {
        eprintln!("Write failed: {}", e);
    }
}

// no-match: let _ discarding a non-call expression
fn discard_literal() {
    let _ = 42;  // ok: discarded-result - not a call
}
