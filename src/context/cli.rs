//! CLI surface for `quorum context` subcommands.
//!
//! This module is deliberately decoupled from the argparse layer: it exposes
//! a `ContextDeps` trait bundling all side-effect dependencies and a
//! `run_context_cmd` dispatcher that takes a `ContextCmd` enum. The argparse
//! layer (wired in a later task) only translates `clap::Args` into a
//! `ContextCmd`, then hands it off to `run_context_cmd(..., &ProdDeps)`.
//!
//! Tests use `TestDeps`, which owns its own `TempDir` and swaps in the
//! in-process `HashEmbedder` / `FixedClock` / `FakeGit` fakes. This lets the
//! whole CLI surface be exercised without touching `~/.quorum` or fastembed.
//!
//! Task 7.1 delivers the trait, both deps impls, the enum skeleton for every
//! forthcoming subcommand, and the `init` handler. The other variants
//! (`add`, `list`, `index`, `refresh`, `query`, `prune`, `doctor`) return a
//! stable "not yet implemented" error until later tasks replace the body.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use crate::context::config::SourcesConfig;
use crate::context::index::traits::{Clock, Embedder, HashEmbedder, SystemClock};
use crate::context::inject::stale::{GitOps, SystemGit};

#[cfg(test)]
use crate::context::index::traits::FixedClock;
#[cfg(test)]
use crate::context::inject::stale::FakeGit;
#[cfg(test)]
use tempfile::TempDir;

// --- Deps trait -------------------------------------------------------------

/// Bundle of side-effect dependencies every `quorum context` subcommand
/// needs. Concrete implementations are `ProdDeps` (real filesystem, real
/// git, real embedder) and `TestDeps` (tempdir + fakes).
pub trait ContextDeps {
    type Git: GitOps;
    type Clock: Clock;
    type Embedder: Embedder;

    fn git(&self) -> &Self::Git;
    fn clock(&self) -> &Self::Clock;
    fn embedder(&self) -> &Self::Embedder;

    /// Root directory where quorum state lives (e.g. `~/.quorum` in prod,
    /// a tempdir under test). Callers resolve `sources.toml`, the index
    /// db, and other artefacts relative to this.
    fn home_dir(&self) -> &Path;
}

// --- Prod impl --------------------------------------------------------------

/// Production `ContextDeps` implementation.
///
/// Holds owned instances of `SystemGit`, `SystemClock`, and an `Embedder`.
///
/// Note: the `Embedder` associated type is currently `HashEmbedder` as a
/// placeholder. The real fastembed wrapper (feature-gated on `embeddings`)
/// will land in Task 7.3 when it is first actually invoked; `init`/`add`/
/// `list` don't embed anything, so this stub is harmless for now. Swapping
/// the associated type later is a localized change.
pub struct ProdDeps {
    git: SystemGit,
    clock: SystemClock,
    embedder: HashEmbedder,
    home_dir: PathBuf,
}

impl ProdDeps {
    /// Construct with an explicit home directory. Use `ProdDeps::from_env()`
    /// to pick up `~/.quorum` automatically.
    pub fn new(home_dir: PathBuf) -> Self {
        Self {
            git: SystemGit,
            clock: SystemClock,
            // Placeholder dimension matches the production fastembed
            // bge-small-en-v1.5 dim so the index schema doesn't change when
            // we swap this for the real embedder in Task 7.3.
            embedder: HashEmbedder::new(384),
            home_dir,
        }
    }

    /// Resolve `~/.quorum` from `$HOME` (Unix) or `%USERPROFILE%` (Windows).
    /// Returns an error if neither is set.
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .ok_or_else(|| anyhow!("neither HOME nor USERPROFILE is set"))?;
        let root = PathBuf::from(home).join(".quorum");
        Ok(Self::new(root))
    }
}

impl ContextDeps for ProdDeps {
    type Git = SystemGit;
    type Clock = SystemClock;
    type Embedder = HashEmbedder;

    fn git(&self) -> &Self::Git {
        &self.git
    }
    fn clock(&self) -> &Self::Clock {
        &self.clock
    }
    fn embedder(&self) -> &Self::Embedder {
        &self.embedder
    }
    fn home_dir(&self) -> &Path {
        &self.home_dir
    }
}

// --- Test impl --------------------------------------------------------------

/// Test `ContextDeps` implementation.
///
/// Owns its own `TempDir` so `home_dir()` returns a stable path for the
/// lifetime of the `TestDeps`. Rust drops fields in declaration order, and
/// we keep `_tempdir` last as defense in depth — no other field holds a
/// reference into it today, but future additions must not outlive it.
///
/// Gated to `#[cfg(test)]` because `tempfile` is a dev-dependency; the
/// production binary never sees it.
#[cfg(test)]
pub struct TestDeps {
    git: FakeGit,
    clock: FixedClock,
    embedder: HashEmbedder,
    home_dir: PathBuf,
    _tempdir: TempDir,
}

#[cfg(test)]
impl TestDeps {
    /// Construct with sensible defaults: clean working tree, epoch clock,
    /// 384-dim hash embedder, fresh tempdir as home.
    pub fn new() -> Self {
        let tempdir = tempfile::tempdir().expect("create tempdir for TestDeps");
        let home_dir = tempdir.path().to_path_buf();
        Self {
            git: FakeGit { dirty: false },
            clock: FixedClock::epoch(),
            embedder: HashEmbedder::new(384),
            home_dir,
            _tempdir: tempdir,
        }
    }

    /// Mutate the fake git state. Useful for staleness-path tests later.
    pub fn with_dirty_tree(mut self, dirty: bool) -> Self {
        self.git = FakeGit { dirty };
        self
    }
}

#[cfg(test)]
impl Default for TestDeps {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl ContextDeps for TestDeps {
    type Git = FakeGit;
    type Clock = FixedClock;
    type Embedder = HashEmbedder;

    fn git(&self) -> &Self::Git {
        &self.git
    }
    fn clock(&self) -> &Self::Clock {
        &self.clock
    }
    fn embedder(&self) -> &Self::Embedder {
        &self.embedder
    }
    fn home_dir(&self) -> &Path {
        &self.home_dir
    }
}

// --- Command enum -----------------------------------------------------------

/// All `quorum context` subcommands. Each variant carries an args struct so
/// later tasks can add fields without reshaping the enum. Args structs are
/// empty placeholders today.
#[derive(Debug, Clone)]
pub enum ContextCmd {
    Init,
    Add(AddArgs),
    List,
    Index(IndexArgs),
    Refresh(RefreshArgs),
    Query(QueryArgs),
    Prune,
    Doctor(DoctorArgs),
}

impl ContextCmd {
    fn name(&self) -> &'static str {
        match self {
            ContextCmd::Init => "init",
            ContextCmd::Add(_) => "add",
            ContextCmd::List => "list",
            ContextCmd::Index(_) => "index",
            ContextCmd::Refresh(_) => "refresh",
            ContextCmd::Query(_) => "query",
            ContextCmd::Prune => "prune",
            ContextCmd::Doctor(_) => "doctor",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AddArgs {}

#[derive(Debug, Clone, Default)]
pub struct IndexArgs {}

#[derive(Debug, Clone, Default)]
pub struct RefreshArgs {}

#[derive(Debug, Clone, Default)]
pub struct QueryArgs {}

#[derive(Debug, Clone, Default)]
pub struct DoctorArgs {}

// --- Output -----------------------------------------------------------------

/// Structured result of running a `ContextCmd`. Testable in isolation from
/// any stdio wiring.
#[derive(Debug, Clone, Default)]
pub struct CmdOutput {
    /// Human-readable summary the CLI layer prints on stdout.
    pub stdout: String,
    /// Paths the command created (for test assertions + `--dry-run` UX).
    pub created_paths: Vec<PathBuf>,
    /// Non-fatal warnings (e.g. "already initialized").
    pub warnings: Vec<String>,
}

// --- Dispatcher -------------------------------------------------------------

/// Dispatch a `ContextCmd` against the given deps.
///
/// Variants other than `Init` currently return
/// `anyhow!("not yet implemented: <name>")` — later tasks replace each body
/// in turn. We use `anyhow::bail!` rather than `unimplemented!()` so a stray
/// call from a test won't panic the whole test binary.
pub fn run_context_cmd<D: ContextDeps>(cmd: &ContextCmd, deps: &D) -> Result<CmdOutput> {
    match cmd {
        ContextCmd::Init => run_init(deps),
        other => Err(anyhow!("not yet implemented: context {}", other.name())),
    }
}

// --- Init handler -----------------------------------------------------------

fn run_init<D: ContextDeps>(deps: &D) -> Result<CmdOutput> {
    // `home_dir()` already points at the quorum state root (e.g. `~/.quorum`
    // in production, a tempdir in tests) — don't append `.quorum` again here.
    let sources_path = deps.home_dir().join("sources.toml");

    if sources_path.exists() {
        return Ok(CmdOutput {
            stdout: format!("context already initialized at {}", sources_path.display()),
            created_paths: Vec::new(),
            warnings: vec![format!(
                "{} already exists; leaving it untouched",
                sources_path.display()
            )],
        });
    }

    SourcesConfig::write_default(&sources_path)?;

    Ok(CmdOutput {
        stdout: format!("initialized context at {}", sources_path.display()),
        created_paths: vec![sources_path],
        warnings: Vec::new(),
    })
}
