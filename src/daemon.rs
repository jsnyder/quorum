/// Daemon mode: persistent background process with warm caches and file watching.
/// Keeps ParseCache warm, invalidates on file changes, accepts review requests
/// via the MCP server (stdio transport).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::cache::ParseCache;

/// Configuration for the daemon.
pub struct DaemonConfig {
    /// Directory to watch for file changes.
    pub watch_dir: PathBuf,
    /// Parse cache capacity.
    pub cache_capacity: usize,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            watch_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            cache_capacity: 256,
        }
    }
}

/// Events the daemon handles.
#[derive(Debug)]
pub enum DaemonEvent {
    FileChanged(PathBuf),
    Shutdown,
}

/// Start file watcher that sends events on file changes.
pub fn start_watcher(
    watch_dir: &Path,
    tx: mpsc::UnboundedSender<DaemonEvent>,
) -> anyhow::Result<RecommendedWatcher> {
    let tx_clone = tx.clone();
    let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
        if let Ok(event) = res {
            match event.kind {
                EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_) => {
                    for path in event.paths {
                        let _ = tx_clone.send(DaemonEvent::FileChanged(path));
                    }
                }
                _ => {}
            }
        }
    })?;

    watcher.watch(watch_dir, RecursiveMode::Recursive)?;
    Ok(watcher)
}

/// Process daemon events: invalidate cache on file changes.
pub async fn run_event_loop(
    mut rx: mpsc::UnboundedReceiver<DaemonEvent>,
    _cache: Arc<ParseCache>,
) {
    while let Some(event) = rx.recv().await {
        match event {
            DaemonEvent::FileChanged(path) => {
                // For now, we can't selectively invalidate the cache since
                // it's keyed by content hash (content changes = new hash = auto miss).
                // This is a no-op for correctness, but we log for observability.
                eprintln!("File changed: {}", path.display());
            }
            DaemonEvent::Shutdown => {
                eprintln!("Daemon shutting down.");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_config_default() {
        let cfg = DaemonConfig::default();
        assert_eq!(cfg.cache_capacity, 256);
    }

    #[test]
    fn daemon_event_debug() {
        let event = DaemonEvent::FileChanged(PathBuf::from("test.rs"));
        let debug_str = format!("{:?}", event);
        assert!(debug_str.contains("test.rs"));
    }

    #[test]
    fn daemon_event_shutdown() {
        let event = DaemonEvent::Shutdown;
        let debug_str = format!("{:?}", event);
        assert!(debug_str.contains("Shutdown"));
    }

    #[tokio::test]
    async fn event_loop_processes_file_change() {
        let (tx, rx) = mpsc::unbounded_channel();
        let cache = Arc::new(ParseCache::new(10));

        tx.send(DaemonEvent::FileChanged(PathBuf::from("test.rs"))).unwrap();
        tx.send(DaemonEvent::Shutdown).unwrap();

        run_event_loop(rx, cache).await;
        // No panic = success. Cache is content-hash-keyed so file changes
        // are automatically handled (changed content = new hash = cache miss).
    }

    #[tokio::test]
    async fn event_loop_shutdown() {
        let (tx, rx) = mpsc::unbounded_channel();
        let cache = Arc::new(ParseCache::new(10));

        tx.send(DaemonEvent::Shutdown).unwrap();
        run_event_loop(rx, cache).await;
        // Clean shutdown without hanging
    }

    #[test]
    fn cache_auto_invalidates_on_content_change() {
        // The parse cache is keyed by content hash. When a file's content
        // changes, the hash changes, so it's a cache miss automatically.
        // No explicit invalidation needed.
        let cache = ParseCache::new(10);
        let v1 = "fn original() {}";
        let v2 = "fn modified() {}";

        cache.get_or_parse(v1, crate::parser::Language::Rust).unwrap();
        assert_eq!(cache.stats().misses, 1);

        // Same content = hit
        cache.get_or_parse(v1, crate::parser::Language::Rust).unwrap();
        assert_eq!(cache.stats().hits, 1);

        // Different content = miss (auto-invalidation)
        cache.get_or_parse(v2, crate::parser::Language::Rust).unwrap();
        assert_eq!(cache.stats().misses, 2);
    }
}
