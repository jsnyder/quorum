//! Staleness annotation for rendered context blocks.
//!
//! When a chunk's source has uncommitted edits since indexing, the renderer
//! can surface a short freshness warning. The `StalenessAnnotator` trait is
//! the extension point; two implementations ship here.

use std::path::Path;

use crate::context::index::traits::Clock;
use crate::context::types::Chunk;

/// Trait for producing a short freshness warning when a chunk's source has
/// changed since indexing. Implementations must be cheap to call — invoked
/// per-injected-chunk during render.
pub trait StalenessAnnotator {
    /// Returns Some(message) when the chunk is stale. Returned string is
    /// inserted verbatim into the rendered context block.
    fn annotate(&self, chunk: &Chunk) -> Option<String>;
}

/// No-op annotator — always returns None. Use for tests or when staleness
/// tracking is disabled.
pub struct NoStaleness;

impl StalenessAnnotator for NoStaleness {
    fn annotate(&self, _: &Chunk) -> Option<String> {
        None
    }
}

/// Git porcelain status entry — abstracted into a trait for testability.
pub trait GitOps {
    /// Returns true if the working tree has any uncommitted changes.
    /// For MVP this is a boolean; future versions may return per-path status.
    fn has_local_changes(&self, repo_root: &Path) -> std::io::Result<bool>;
}

/// Production GitOps implementation using `git status --porcelain`.
pub struct SystemGit;

impl GitOps for SystemGit {
    fn has_local_changes(&self, repo_root: &Path) -> std::io::Result<bool> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .arg("status")
            .arg("--porcelain")
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "git status failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(!output.stdout.is_empty())
    }
}

/// Fake GitOps for tests — returns a canned `has_local_changes` value.
pub struct FakeGit {
    pub dirty: bool,
}

impl GitOps for FakeGit {
    fn has_local_changes(&self, _: &Path) -> std::io::Result<bool> {
        Ok(self.dirty)
    }
}

/// Annotator that uses git status + chunk indexed_at to detect staleness.
/// Only annotates chunks whose source matches `current_source` (the local
/// repo we're reviewing); registry sources (other checkouts) aren't flagged.
pub struct TimestampStaleness<'a, G: GitOps> {
    pub current_source: Option<&'a str>,
    pub current_source_root: Option<&'a Path>,
    pub git: &'a G,
    pub clock: &'a dyn Clock,
}

impl<'a, G: GitOps> StalenessAnnotator for TimestampStaleness<'a, G> {
    fn annotate(&self, chunk: &Chunk) -> Option<String> {
        let source = self.current_source?;
        if chunk.source != source {
            return None;
        }
        let root = self.current_source_root?;
        let dirty = self.git.has_local_changes(root).ok()?;
        if !dirty {
            return None;
        }
        let now = self.clock.now();
        let age = now - chunk.metadata.indexed_at;
        let age_str = format_age(age);
        Some(format!("source has edits since last index ({age_str} ago)"))
    }
}

fn format_age(age: chrono::Duration) -> String {
    let total_secs = age.num_seconds();
    if total_secs < 0 {
        return "just now".to_string();
    }
    let hours = age.num_hours();
    let days = age.num_days();
    if days >= 1 {
        if days == 1 {
            "1d".to_string()
        } else {
            format!("{days}d")
        }
    } else if hours >= 1 {
        if hours == 1 {
            "1h".to_string()
        } else {
            format!("{hours}h")
        }
    } else {
        let mins = age.num_minutes();
        if mins <= 1 {
            "just now".to_string()
        } else {
            format!("{mins}m")
        }
    }
}
