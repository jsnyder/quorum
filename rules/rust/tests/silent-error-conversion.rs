// Fixture: silent-error-conversion
use std::fs;

fn read_config() -> String {
    // match: .ok() on fs::read_to_string hides all errors
    let _ = fs::read_to_string("config.toml").ok();

    // match: .unwrap_or_default() on serde_json::from_str
    let _ = serde_json::from_str::<String>("").unwrap_or_default();

    // no-match: proper error propagation
    let _ = fs::read_to_string("config.toml").map_err(|e| eprintln!("{e}"));

    String::new()
}
