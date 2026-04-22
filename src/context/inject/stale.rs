//! Staleness annotation for rendered context blocks.
//!
//! When a chunk's source has uncommitted edits since indexing, the renderer
//! can surface a short freshness warning. The `StalenessAnnotator` trait is
//! the extension point; two implementations ship here.

use std::cell::RefCell;
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

    /// Resolve the current git HEAD sha for the given directory.
    ///
    /// Returns `Ok(None)` when the directory is not inside a git repository
    /// (e.g. path sources that point at a plain folder). Returns `Err` only
    /// when `git` itself is broken (missing binary, I/O error). Callers use
    /// this to decide whether a re-index is needed.
    fn head_sha(&self, repo_root: &Path) -> std::io::Result<Option<String>>;
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

    fn head_sha(&self, repo_root: &Path) -> std::io::Result<Option<String>> {
        // `git rev-parse HEAD` prints the sha to stdout. A non-zero exit
        // means "not a git repo" (or an empty repo with no commits yet); we
        // map that to Ok(None) rather than Err so callers can treat path
        // sources uniformly.
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .arg("rev-parse")
            .arg("HEAD")
            .output()?;
        if !output.status.success() {
            return Ok(None);
        }
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if sha.is_empty() {
            Ok(None)
        } else {
            Ok(Some(sha))
        }
    }
}

/// Fake GitOps for tests — returns canned values.
pub struct FakeGit {
    pub dirty: bool,
    /// Canned HEAD sha keyed by repo root path. When a path isn't present in
    /// the map, `head_sha` returns whatever `default_head` is. The default
    /// `FakeGit::default()` yields `Some("deadbeef...")` regardless of path,
    /// which is the common case for tests that only care that a sha exists.
    pub head_by_path: std::collections::HashMap<std::path::PathBuf, Option<String>>,
    pub default_head: Option<String>,
}

impl Default for FakeGit {
    fn default() -> Self {
        Self {
            dirty: false,
            head_by_path: std::collections::HashMap::new(),
            default_head: Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string()),
        }
    }
}

impl FakeGit {
    /// Convenience constructor matching the old `FakeGit { dirty }` shape so
    /// existing call sites continue to compile.
    pub fn with_dirty(dirty: bool) -> Self {
        Self {
            dirty,
            ..Self::default()
        }
    }

    /// Register a canned HEAD sha for a specific path.
    pub fn set_head(&mut self, path: &Path, sha: Option<String>) {
        self.head_by_path.insert(path.to_path_buf(), sha);
    }
}

impl GitOps for FakeGit {
    fn has_local_changes(&self, _: &Path) -> std::io::Result<bool> {
        Ok(self.dirty)
    }

    fn head_sha(&self, repo_root: &Path) -> std::io::Result<Option<String>> {
        if let Some(override_sha) = self.head_by_path.get(repo_root) {
            return Ok(override_sha.clone());
        }
        Ok(self.default_head.clone())
    }
}

/// Annotator that uses git status + chunk indexed_at to detect staleness.
/// Only annotates chunks whose source matches `current_source` (the local
/// repo we're reviewing); registry sources (other checkouts) aren't flagged.
///
/// `git status` is invoked at most once per annotator instance: the first
/// call caches the result in `dirty_cache`, and every subsequent `annotate`
/// reuses it. Construct a new annotator per rendering pass to re-probe.
pub struct TimestampStaleness<'a, G: GitOps> {
    pub current_source: Option<&'a str>,
    pub current_source_root: Option<&'a Path>,
    pub git: &'a G,
    pub clock: &'a dyn Clock,
    #[doc(hidden)]
    pub dirty_cache: RefCell<Option<Option<bool>>>,
}

impl<'a, G: GitOps> TimestampStaleness<'a, G> {
    pub fn new(
        current_source: Option<&'a str>,
        current_source_root: Option<&'a Path>,
        git: &'a G,
        clock: &'a dyn Clock,
    ) -> Self {
        Self {
            current_source,
            current_source_root,
            git,
            clock,
            dirty_cache: RefCell::new(None),
        }
    }

    fn cached_dirty(&self, root: &Path) -> Option<bool> {
        if let Some(cached) = *self.dirty_cache.borrow() {
            return cached;
        }
        let result = self.git.has_local_changes(root).ok();
        *self.dirty_cache.borrow_mut() = Some(result);
        result
    }
}

impl<'a, G: GitOps> StalenessAnnotator for TimestampStaleness<'a, G> {
    fn annotate(&self, chunk: &Chunk) -> Option<String> {
        let source = self.current_source?;
        if chunk.source != source {
            return None;
        }
        let root = self.current_source_root?;
        let dirty = self.cached_dirty(root)?;
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
