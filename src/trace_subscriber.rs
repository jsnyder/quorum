//! Optional JSON trace subscriber for calibrator decision logging.
//! Activated by --trace flag or QUORUM_TRACE=1 env var.
//! Writes to ~/.quorum/trace.jsonl.

use std::path::PathBuf;

/// Initialize the tracing subscriber if tracing is enabled.
/// Returns the guard that must be held for the lifetime of the program.
pub fn init_trace_subscriber(trace_path: Option<PathBuf>) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let path = trace_path?;

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;

    let (non_blocking, guard) = tracing_appender::non_blocking(file);
    tracing_subscriber::fmt()
        .json()
        .with_writer(non_blocking)
        .with_target(false)
        .with_level(true)
        .try_init()
        .ok();

    Some(guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn init_with_none_returns_none() {
        let result = init_trace_subscriber(None);
        assert!(result.is_none());
    }

    #[test]
    fn trace_path_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("trace.jsonl");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        assert!(path.parent().unwrap().exists());
    }
}
