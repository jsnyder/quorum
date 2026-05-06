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

use anyhow::{Result, anyhow};
use rusqlite::Connection;

use crate::context::config::{SourceEntry, SourceKind, SourceLocation, SourcesConfig};
use crate::context::extract::dispatch::{ExtractConfig, extract_source};
use crate::context::index::builder::{IndexBuilder, ensure_vec_loaded};
use crate::context::index::state::IndexState;
#[cfg(feature = "embeddings")]
use crate::context::index::traits::FastEmbedEmbedder;
use crate::context::index::traits::HashEmbedder;
use crate::context::index::traits::{Clock, Embedder, SystemClock};

/// Production-mode embedder. Usually backed by fastembed's
/// bge-small-en-v1.5 (384-dim); falls back to HashEmbedder when fastembed
/// initialization fails (e.g., no network on first run to download the
/// ONNX model) so reviews still run, degraded to BM25-only retrieval.
///
/// The enum dispatch costs one match per call, which is negligible next
/// to ONNX inference. Keeping both variants behind a single public type
/// lets `ContextDeps::Embedder` stay a concrete type (required by the
/// trait's associated-type contract) while still permitting graceful
/// fallback at runtime.
pub enum ProdEmbedder {
    #[cfg(feature = "embeddings")]
    Fast(FastEmbedEmbedder),
    Hash(HashEmbedder),
}

impl Embedder for ProdEmbedder {
    fn dim(&self) -> usize {
        match self {
            #[cfg(feature = "embeddings")]
            Self::Fast(e) => e.dim(),
            Self::Hash(e) => e.dim(),
        }
    }
    fn embed(&self, text: &str) -> Vec<f32> {
        match self {
            #[cfg(feature = "embeddings")]
            Self::Fast(e) => e.embed(text),
            Self::Hash(e) => e.embed(text),
        }
    }
    fn model_hash(&self) -> String {
        match self {
            #[cfg(feature = "embeddings")]
            Self::Fast(e) => e.model_hash(),
            Self::Hash(e) => e.model_hash(),
        }
    }
}
use crate::context::inject::stale::{GitOps, SystemGit};
use crate::context::retrieve::{Filters, RetrievalQuery, Retriever};
use crate::context::store::ChunkStore;

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
    embedder: ProdEmbedder,
    home_dir: PathBuf,
}

impl ProdDeps {
    /// Construct with an explicit home directory. Use `ProdDeps::from_env()`
    /// to pick up `~/.quorum` automatically.
    pub fn new(home_dir: PathBuf) -> Self {
        Self {
            git: SystemGit,
            clock: SystemClock,
            embedder: new_prod_embedder(),
            home_dir,
        }
    }

    /// Construct a `ProdDeps` whose embedder is the **strict** production
    /// embedder: fastembed init failures surface as errors instead of
    /// falling back to HashEmbedder. Use this for paths that would
    /// otherwise silently corrupt the on-disk index (`index`, `refresh`).
    pub fn new_strict(home_dir: PathBuf) -> Result<Self> {
        Ok(Self {
            git: SystemGit,
            clock: SystemClock,
            embedder: new_prod_embedder_strict()?,
            home_dir,
        })
    }

    /// Resolve `~/.quorum` from `$HOME` (Unix) or `%USERPROFILE%` (Windows).
    /// Returns an error if neither is set, if either is empty, or if the
    /// value is not an absolute path. An empty or relative value would
    /// yield a `.quorum` path that resolves against the current working
    /// directory, which is never the user's intent and would silently
    /// scatter state across wherever the binary happened to launch.
    pub fn from_env() -> Result<Self> {
        Ok(Self::new(Self::resolve_quorum_root()?))
    }

    /// Like `from_env` but uses the strict embedder factory. Use for paths
    /// that write to the on-disk index (`index`, `refresh`) so a silent
    /// fastembed fallback can't corrupt `chunks_vec`.
    pub fn from_env_strict() -> Result<Self> {
        Self::new_strict(Self::resolve_quorum_root()?)
    }

    fn resolve_quorum_root() -> Result<PathBuf> {
        let from = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
        // On Windows, `USERPROFILE` is the canonical user dir. `HOME` is
        // often set by MSYS/Cygwin/Git Bash to an MSYS-mangled path that
        // doesn't match the profile CreateFile + Explorer see. Prefer
        // USERPROFILE there and fall back to HOME for non-standard envs.
        // Elsewhere (macOS/Linux), HOME is canonical.
        #[cfg(windows)]
        let home = from("USERPROFILE")
            .or_else(|| from("HOME"))
            .ok_or_else(|| anyhow!("neither USERPROFILE nor HOME is set"))?;
        #[cfg(not(windows))]
        let home = from("HOME")
            .or_else(|| from("USERPROFILE"))
            .ok_or_else(|| anyhow!("neither HOME nor USERPROFILE is set"))?;
        let home_path = PathBuf::from(&home);
        if !home_path.is_absolute() {
            anyhow::bail!(
                "HOME/USERPROFILE must be an absolute path, got {:?}",
                home_path
            );
        }
        Ok(home_path.join(".quorum"))
    }
}

/// Construct the production embedder for **review/query** paths. Fastembed
/// can fail on first run (model download needs network) — we log a warning
/// and fall back to HashEmbedder so reviews still execute. BM25 retrieval
/// still works in the fallback; only the vector-similarity leg is degraded.
pub(crate) fn new_prod_embedder() -> ProdEmbedder {
    #[cfg(feature = "embeddings")]
    {
        match FastEmbedEmbedder::new() {
            Ok(e) => ProdEmbedder::Fast(e),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "fastembed init failed; falling back to HashEmbedder (BM25-only retrieval)"
                );
                // HashEmbedder dim must match what's persisted in the
                // index's `chunks_vec` schema — 384-dim to align with
                // the production fastembed model.
                ProdEmbedder::Hash(HashEmbedder::new(384))
            }
        }
    }
    #[cfg(not(feature = "embeddings"))]
    {
        ProdEmbedder::Hash(HashEmbedder::new(384))
    }
}

/// Construct the production embedder for **index/refresh** paths. Unlike
/// [`new_prod_embedder`], a fastembed init failure here returns `Err`
/// instead of falling back. Silently rebuilding the `chunks_vec` table
/// with HashEmbedder noise vectors would corrupt semantic retrieval
/// until the user discovers the drift and reruns with the model present,
/// so we refuse to alter the index without the production embedder.
///
/// When built without the `embeddings` feature HashEmbedder *is* the
/// intended production embedder, so this returns `Ok` just like the
/// lenient factory.
pub(crate) fn new_prod_embedder_strict() -> anyhow::Result<ProdEmbedder> {
    #[cfg(feature = "embeddings")]
    {
        let e = FastEmbedEmbedder::new().map_err(|e| {
            anyhow!(
                "fastembed init failed ({e}); refusing to rebuild the index with \
                 HashEmbedder fallback — retry once the model is available"
            )
        })?;
        Ok(ProdEmbedder::Fast(e))
    }
    #[cfg(not(feature = "embeddings"))]
    {
        Ok(ProdEmbedder::Hash(HashEmbedder::new(384)))
    }
}

impl ContextDeps for ProdDeps {
    type Git = SystemGit;
    type Clock = SystemClock;
    type Embedder = ProdEmbedder;

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
            git: FakeGit::with_dirty(false),
            clock: FixedClock::epoch(),
            embedder: HashEmbedder::new(384),
            home_dir,
            _tempdir: tempdir,
        }
    }

    /// Mutate the fake git state. Useful for staleness-path tests later.
    pub fn with_dirty_tree(mut self, dirty: bool) -> Self {
        self.git = FakeGit::with_dirty(dirty);
        self
    }

    /// Mutable access to the underlying `FakeGit` so tests can pin per-path
    /// HEAD shas (used by `refresh` tests).
    pub fn git_mut(&mut self) -> &mut FakeGit {
        &mut self.git
    }

    /// Set the default HEAD sha returned for any path (unless overridden).
    pub fn with_default_head(mut self, sha: Option<String>) -> Self {
        self.git.default_head = sha;
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
    List(ListArgs),
    Index(IndexArgs),
    Refresh(RefreshArgs),
    Query(QueryArgs),
    Prune(PruneArgs),
    Doctor(DoctorArgs),
}

impl ContextCmd {
    fn name(&self) -> &'static str {
        match self {
            ContextCmd::Init => "init",
            ContextCmd::Add(_) => "add",
            ContextCmd::List(_) => "list",
            ContextCmd::Index(_) => "index",
            ContextCmd::Refresh(_) => "refresh",
            ContextCmd::Query(_) => "query",
            ContextCmd::Prune(_) => "prune",
            ContextCmd::Doctor(_) => "doctor",
        }
    }
}

/// Location for `quorum context add`. `Path` and `Git` are mutually
/// exclusive; the CLI layer is responsible for enforcing this when parsing
/// the `--path` / `--git` flags.
#[derive(Debug, Clone)]
pub enum AddLocation {
    Path(PathBuf),
    Git { url: String, rev: Option<String> },
}

impl Default for AddLocation {
    fn default() -> Self {
        AddLocation::Path(PathBuf::new())
    }
}

#[derive(Debug, Clone, Default)]
pub struct AddArgs {
    pub name: String,
    pub kind: String,
    pub location: AddLocation,
    pub weight: Option<i32>,
    pub ignore: Vec<String>,
}

/// Output format for `quorum context list`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ListFormat {
    #[default]
    Human,
    Compact,
    Json,
}

#[derive(Debug, Clone, Default)]
pub struct ListArgs {
    pub format: ListFormat,
}

/// Selector for `index` / `refresh` / bulk ops: a single named source or
/// every registered source. `Single("")` is invalid — the CLI layer must
/// translate `--all` into `SourceSelector::All`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SourceSelector {
    Single(String),
    #[default]
    All,
}

#[derive(Debug, Clone, Default)]
pub struct IndexArgs {
    pub selector: SourceSelector,
}

#[derive(Debug, Clone, Default)]
pub struct RefreshArgs {
    pub selector: SourceSelector,
}

/// Output format for `quorum context query`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QueryFormat {
    #[default]
    Table,
    Compact,
    Json,
}

#[derive(Debug, Clone, Default)]
pub struct QueryArgs {
    pub text: String,
    /// Optional single-source filter.
    pub source: Option<String>,
    pub k: Option<usize>,
    pub explain: bool,
    pub format: QueryFormat,
}

/// Output format for `quorum context doctor`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DoctorFormat {
    #[default]
    Table,
    Compact,
    Json,
}

#[derive(Debug, Clone, Default)]
pub struct DoctorArgs {
    pub format: DoctorFormat,
    /// Apply best-effort fixes for any fixable failures (missing dirs,
    /// missing index.db, embedder model hash mismatch).
    pub repair: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PruneArgs {
    /// When true, report what would be deleted without touching the disk.
    pub dry_run: bool,
}

/// Status of a single `doctor` check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Fail { fixable: bool },
    Warn,
}

impl CheckStatus {
    fn as_str(&self) -> &'static str {
        match self {
            CheckStatus::Pass => "pass",
            CheckStatus::Fail { .. } => "fail",
            CheckStatus::Warn => "warn",
        }
    }
    fn fixable(&self) -> bool {
        matches!(self, CheckStatus::Fail { fixable: true })
    }
}

/// One row in the doctor report. `scope` is `None` for whole-store checks
/// (sources.toml, orphans) and `Some(source_name)` for per-source checks.
#[derive(Debug, Clone)]
pub struct CheckResult {
    pub name: &'static str,
    pub scope: Option<String>,
    pub status: CheckStatus,
    pub detail: String,
}

// --- Output -----------------------------------------------------------------

/// Structured result of running a `ContextCmd`. Testable in isolation from
/// any stdio wiring.
#[derive(Debug, Clone, Default)]
pub struct CmdOutput {
    /// Human-readable summary the CLI layer prints on stdout.
    pub stdout: String,
    /// Paths the command created (for test assertions + `--dry-run` UX).
    pub created_paths: Vec<PathBuf>,
    /// Paths the command deleted (or would have, under `--dry-run`).
    /// Populated by `prune`; empty for all other commands.
    pub removed_paths: Vec<PathBuf>,
    /// Non-fatal warnings (e.g. "already initialized").
    pub warnings: Vec<String>,
    /// Doctor-only: `Some(true)` if any check failed, `Some(false)` if all
    /// passed, `None` for non-doctor commands. Drives the CLI exit code
    /// without re-parsing rendered stdout (issue #73).
    pub doctor_failed: Option<bool>,
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
        ContextCmd::Add(args) => run_add(args, deps),
        ContextCmd::List(args) => run_list(args, deps),
        ContextCmd::Index(args) => run_index(args, deps),
        ContextCmd::Refresh(args) => run_refresh(args, deps),
        ContextCmd::Query(args) => run_query(args, deps),
        ContextCmd::Prune(args) => run_prune(args, deps),
        ContextCmd::Doctor(args) => run_doctor(args, deps),
    }
}

// --- Init handler -----------------------------------------------------------

fn run_init<D: ContextDeps>(deps: &D) -> Result<CmdOutput> {
    use std::io::Write;

    // `home_dir()` already points at the quorum state root (e.g. `~/.quorum`
    // in production, a tempdir in tests) — don't append `.quorum` again here.
    let sources_path = deps.home_dir().join("sources.toml");

    if let Some(parent) = sources_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow!("cannot create {}: {e}", parent.display()))?;
    }

    // Atomic create-or-fail: previously we did `exists()` + `write()`, which
    // left a TOCTOU window where a concurrent process could slip in and get
    // its config clobbered. `create_new(true)` hands that race to the OS.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&sources_path)
    {
        Ok(mut f) => {
            f.write_all(crate::context::config::default_sources_toml().as_bytes())
                .map_err(|e| anyhow!("write {}: {e}", sources_path.display()))?;
            Ok(CmdOutput {
                stdout: format!("initialized context at {}", sources_path.display()),
                created_paths: vec![sources_path],
                removed_paths: Vec::new(),
                warnings: Vec::new(),
                doctor_failed: None,
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Distinguish "already a regular file" (idempotent success) from
            // "exists but is a directory / symlink / device" (hard error).
            // Without this, a stray `mkdir sources.toml` under `~/.quorum`
            // would make every future `init`/`add` silently no-op.
            let meta = std::fs::symlink_metadata(&sources_path)
                .map_err(|e| anyhow!("cannot stat {}: {e}", sources_path.display()))?;
            if !meta.file_type().is_file() {
                return Err(anyhow!(
                    "{} exists but is not a regular file; refusing to initialize over it",
                    sources_path.display()
                ));
            }
            Ok(CmdOutput {
                stdout: format!("context already initialized at {}", sources_path.display()),
                created_paths: Vec::new(),
                removed_paths: Vec::new(),
                warnings: vec![format!(
                    "{} already exists; leaving it untouched",
                    sources_path.display()
                )],
                doctor_failed: None,
            })
        }
        Err(e) => Err(anyhow!("create {}: {e}", sources_path.display())),
    }
}

// --- Add handler ------------------------------------------------------------

fn run_add<D: ContextDeps>(args: &AddArgs, deps: &D) -> Result<CmdOutput> {
    // Up-front validation: failing here means the on-disk file is never
    // touched, which is the cheapest way to satisfy the atomicity contract.
    //
    // Defense-in-depth (#135): re-run the strict allowlist validator from
    // `crate::cli::validate_source_name`. clap's value_parser already gates
    // the command-line surface, but every API caller that builds `AddArgs`
    // directly (programmatic users, MCP wiring, tests) must hit the same
    // gate before we touch the on-disk file or join `<home>/sources/<name>`
    // anywhere downstream.
    let name = args.name.trim();
    crate::cli::validate_source_name(name).map_err(|e| anyhow!("source name invalid: {e}"))?;
    let kind = SourceKind::parse_cli(&args.kind).ok_or_else(|| {
        anyhow!(
            "unknown kind '{}' (expected one of: rust, typescript, javascript, python, go, terraform, service, docs)",
            args.kind
        )
    })?;

    let location = match &args.location {
        AddLocation::Path(p) => {
            let s = p.to_string_lossy();
            if s.trim().is_empty() {
                return Err(anyhow!("source '{}': path must not be empty", name));
            }
            SourceLocation::Path(p.clone())
        }
        AddLocation::Git { url, rev } => {
            if url.trim().is_empty() {
                return Err(anyhow!("source '{}': git url must not be empty", name));
            }
            // An explicit `--rev ""` should mean "no rev pinned", not store
            // an empty string. Collapse empty/whitespace-only revs to None.
            let rev_clean = rev
                .as_ref()
                .map(|r| r.trim().to_string())
                .filter(|r| !r.is_empty());
            SourceLocation::Git {
                url: url.trim().to_string(),
                rev: rev_clean,
            }
        }
    };

    // Reject control characters in user-supplied strings before they reach
    // the TOML writer — newlines in `name` or `url` would corrupt the file
    // shape even with toml::Value string escaping.
    let reject_controls = |field: &str, s: &str| -> Result<()> {
        if s.chars().any(|c| c.is_control()) {
            return Err(anyhow!(
                "source '{name}': {field} contains a control character"
            ));
        }
        Ok(())
    };
    reject_controls("name", name)?;
    match &location {
        SourceLocation::Path(p) => {
            reject_controls("path", &p.to_string_lossy())?;
        }
        SourceLocation::Git { url, rev } => {
            reject_controls("url", url)?;
            if let Some(r) = rev {
                reject_controls("rev", r)?;
            }
        }
    }
    for g in &args.ignore {
        reject_controls("ignore", g)?;
    }

    let entry = SourceEntry {
        name: name.to_string(),
        kind,
        location,
        paths: Vec::new(),
        weight: args.weight,
        ignore: args.ignore.clone(),
    };

    let sources_path = deps.home_dir().join("sources.toml");
    SourcesConfig::append_source(&sources_path, &entry).map_err(|e| anyhow!("{e}"))?;

    Ok(CmdOutput {
        stdout: format!("added source '{}'", entry.name),
        created_paths: Vec::new(),
        removed_paths: Vec::new(),
        warnings: Vec::new(),
        doctor_failed: None,
    })
}

// --- List handler -----------------------------------------------------------

fn run_list<D: ContextDeps>(args: &ListArgs, deps: &D) -> Result<CmdOutput> {
    let sources_path = deps.home_dir().join("sources.toml");
    if !sources_path.exists() {
        let msg = "no sources registered; run `quorum context init` first".to_string();
        return Ok(CmdOutput {
            stdout: msg.clone(),
            created_paths: Vec::new(),
            removed_paths: Vec::new(),
            warnings: vec![msg],
            doctor_failed: None,
        });
    }
    let cfg = SourcesConfig::load(&sources_path).map_err(|e| anyhow!("{e}"))?;

    let stdout = match args.format {
        ListFormat::Json => render_list_json(&cfg.sources)?,
        ListFormat::Compact => render_list_compact(&cfg.sources),
        ListFormat::Human => render_list_human(&cfg.sources),
    };
    Ok(CmdOutput {
        stdout,
        created_paths: Vec::new(),
        removed_paths: Vec::new(),
        warnings: Vec::new(),
        doctor_failed: None,
    })
}

fn location_summary(loc: &SourceLocation) -> String {
    match loc {
        SourceLocation::Path(p) => p.display().to_string(),
        SourceLocation::Git { url, rev } => match rev {
            Some(r) => format!("{url}@{r}"),
            None => url.clone(),
        },
    }
}

fn render_list_human(sources: &[SourceEntry]) -> String {
    if sources.is_empty() {
        return "no sources registered".to_string();
    }
    // Compute column widths so the table stays readable without pulling in
    // a table crate. Header row first, then data rows.
    let rows: Vec<[String; 5]> = sources
        .iter()
        .map(|e| {
            [
                e.name.clone(),
                e.kind.as_str().to_string(),
                location_summary(&e.location),
                e.weight.map(|w| w.to_string()).unwrap_or_default(),
                e.ignore.len().to_string(),
            ]
        })
        .collect();
    let headers = ["NAME", "KIND", "LOCATION", "WEIGHT", "IGNORE"];
    let mut widths = headers.map(|h| h.len());
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }
    let fmt_row = |cells: &[String; 5]| -> String {
        let mut s = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i > 0 {
                s.push_str("  ");
            }
            s.push_str(cell);
            // pad trailing cells too; trim at the very end for tidy output
            if cell.len() < widths[i] {
                s.push_str(&" ".repeat(widths[i] - cell.len()));
            }
        }
        s.trim_end().to_string()
    };
    let header_cells: [String; 5] = headers.map(|h| h.to_string());
    let mut out = fmt_row(&header_cells);
    out.push('\n');
    for row in &rows {
        out.push_str(&fmt_row(row));
        out.push('\n');
    }
    out
}

fn render_list_compact(sources: &[SourceEntry]) -> String {
    if sources.is_empty() {
        return "no sources registered".to_string();
    }
    // One line per source, tab-separated, glyph-free (see compact rule in
    // project CLAUDE.md). Sort order mirrors the on-disk file order so
    // callers can rely on stable, user-visible ordering.
    let mut out = String::new();
    for e in sources {
        let weight = e
            .weight
            .map(|w| w.to_string())
            .unwrap_or_else(|| "-".into());
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            e.name,
            e.kind.as_str(),
            location_summary(&e.location),
            weight,
            e.ignore.len()
        ));
    }
    out
}

fn render_list_json(sources: &[SourceEntry]) -> Result<String> {
    let items: Vec<serde_json::Value> = sources
        .iter()
        .map(|e| {
            let location = match &e.location {
                SourceLocation::Path(p) => serde_json::json!({ "path": p.display().to_string() }),
                SourceLocation::Git { url, rev } => {
                    serde_json::json!({ "git": { "url": url, "rev": rev } })
                }
            };
            serde_json::json!({
                "name": e.name,
                "kind": e.kind.as_str(),
                "location": location,
                "weight": e.weight,
                "ignore": e.ignore,
            })
        })
        .collect();
    let out = serde_json::json!({ "sources": items });
    Ok(serde_json::to_string_pretty(&out)?)
}

// --- Source layout helpers --------------------------------------------------

/// On-disk layout for a single indexed source.
pub(crate) struct SourceLayout {
    pub(crate) dir: PathBuf,
    pub(crate) jsonl: PathBuf,
    pub(crate) db: PathBuf,
    pub(crate) state: PathBuf,
}

impl SourceLayout {
    pub(crate) fn for_source(home: &Path, name: &str) -> Self {
        let dir = home.join("sources").join(name);
        Self {
            jsonl: dir.join("chunks.jsonl"),
            db: dir.join("index.db"),
            state: dir.join("state.json"),
            dir,
        }
    }
}

fn ensure_dir(p: &Path) -> Result<()> {
    std::fs::create_dir_all(p).map_err(|e| anyhow!("failed to create {}: {e}", p.display()))
}

fn load_sources_or_err<D: ContextDeps>(deps: &D) -> Result<SourcesConfig> {
    let sources_path = deps.home_dir().join("sources.toml");
    if !sources_path.exists() {
        return Err(anyhow!(
            "no sources registered; run `quorum context init` first"
        ));
    }
    SourcesConfig::load(&sources_path).map_err(|e| anyhow!("{e}"))
}

fn find_source<'a>(cfg: &'a SourcesConfig, name: &str) -> Result<&'a SourceEntry> {
    cfg.sources
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| anyhow!("unknown source: {name}"))
}

fn selected_sources<'a>(
    cfg: &'a SourcesConfig,
    sel: &SourceSelector,
) -> Result<Vec<&'a SourceEntry>> {
    match sel {
        SourceSelector::All => Ok(cfg.sources.iter().collect()),
        SourceSelector::Single(name) => Ok(vec![find_source(cfg, name)?]),
    }
}

/// Resolve the git repo root for a source. For Path sources, the directory
/// is the candidate repo root; callers then ask `GitOps::head_sha` which
/// returns None if it's not actually a git repo. Git sources aren't yet
/// supported (extract itself rejects them for MVP).
fn source_repo_root(entry: &SourceEntry) -> Option<&Path> {
    match &entry.location {
        SourceLocation::Path(p) => Some(p.as_path()),
        SourceLocation::Git { .. } => None,
    }
}

// --- Index handler ----------------------------------------------------------

/// Outcome of a single-source index pass. Surfaced verbatim in the summary
/// so callers can see both success and failure for --all runs.
#[derive(Debug)]
struct IndexOutcome {
    name: String,
    result: std::result::Result<IndexSuccess, String>,
}

#[derive(Debug)]
struct IndexSuccess {
    chunks_inserted: usize,
    head_sha: Option<String>,
}

fn run_index<D: ContextDeps>(args: &IndexArgs, deps: &D) -> Result<CmdOutput> {
    let cfg = load_sources_or_err(deps)?;
    let entries = selected_sources(&cfg, &args.selector)?;
    if entries.is_empty() {
        return Ok(CmdOutput {
            stdout: "no sources to index".to_string(),
            ..Default::default()
        });
    }

    let mut outcomes: Vec<IndexOutcome> = Vec::with_capacity(entries.len());
    let mut created: Vec<PathBuf> = Vec::new();
    for entry in &entries {
        match index_one_source(entry, deps, &mut created) {
            Ok(success) => outcomes.push(IndexOutcome {
                name: entry.name.clone(),
                result: Ok(success),
            }),
            Err(e) => {
                tracing::warn!(source = %entry.name, error = %e, "index failed");
                outcomes.push(IndexOutcome {
                    name: entry.name.clone(),
                    result: Err(format!("{e}")),
                });
            }
        }
    }

    let mut warnings = Vec::new();
    let mut lines = Vec::new();
    let mut failures = 0usize;
    for o in &outcomes {
        match &o.result {
            Ok(s) => lines.push(format!(
                "indexed '{}': {} chunks",
                o.name, s.chunks_inserted
            )),
            Err(msg) => {
                failures += 1;
                let line = format!("failed '{}': {msg}", o.name);
                warnings.push(line.clone());
                lines.push(line);
            }
        }
    }
    let stdout = lines.join("\n");
    // Only hard-error when the selector requested a single source and that
    // source itself failed. `--all` with one failed + one succeeded should
    // still return Ok with a warning (resilient per spec).
    if failures == outcomes.len()
        && matches!(args.selector, SourceSelector::Single(_))
        && let Some(IndexOutcome {
            result: Err(msg), ..
        }) = outcomes.first()
    {
        return Err(anyhow!(msg.clone()));
    }
    Ok(CmdOutput {
        stdout,
        created_paths: created,
        removed_paths: Vec::new(),
        warnings,
        doctor_failed: None,
    })
}

fn index_one_source<D: ContextDeps>(
    entry: &SourceEntry,
    deps: &D,
    created: &mut Vec<PathBuf>,
) -> Result<IndexSuccess> {
    let layout = SourceLayout::for_source(deps.home_dir(), &entry.name);
    ensure_dir(&layout.dir)?;

    // Extract chunks. This is where nonexistent Path roots error out, and
    // the error propagates up as a single-source failure (caught by
    // run_index so --all can continue).
    let extracted = extract_source(entry, &ExtractConfig::default(), deps.clock())
        .map_err(|e| anyhow!("extract failed for '{}': {e}", entry.name))?;

    // Wipe any previous jsonl so appends are idempotent across re-index calls.
    // Rebuilding the DB from a stale+appended jsonl would double chunks.
    if layout.jsonl.exists() {
        std::fs::remove_file(&layout.jsonl)
            .map_err(|e| anyhow!("failed to reset {}: {e}", layout.jsonl.display()))?;
    }
    let mut store = ChunkStore::new(&layout.jsonl);
    for chunk in &extracted.chunks {
        store
            .append(chunk)
            .map_err(|e| anyhow!("append failed: {e}"))?;
    }
    created.push(layout.jsonl.clone());

    // Rebuild the index DB from the freshly-written jsonl.
    let mut builder = IndexBuilder::new(&layout.db, deps.clock(), deps.embedder())
        .map_err(|e| anyhow!("open index db: {e}"))?;
    let report = builder
        .rebuild_from_jsonl(&entry.name, &layout.jsonl)
        .map_err(|e| anyhow!("rebuild failed for '{}': {e}", entry.name))?;
    created.push(layout.db.clone());

    // Record state so refresh knows whether to re-run.
    let head_sha = match source_repo_root(entry) {
        Some(root) => deps
            .git()
            .head_sha(root)
            .map_err(|e| anyhow!("git head_sha({}): {e}", root.display()))?,
        None => None,
    };
    let state = IndexState::new(deps.embedder().model_hash())
        .with_head_sha(head_sha.clone())
        .with_indexed_at(deps.clock().now());
    state
        .save(&layout.state)
        .map_err(|e| anyhow!("save state.json: {e}"))?;
    created.push(layout.state.clone());

    Ok(IndexSuccess {
        chunks_inserted: report.chunks_inserted,
        head_sha,
    })
}

// --- Refresh handler --------------------------------------------------------

fn run_refresh<D: ContextDeps>(args: &RefreshArgs, deps: &D) -> Result<CmdOutput> {
    let cfg = load_sources_or_err(deps)?;
    let entries = selected_sources(&cfg, &args.selector)?;
    if entries.is_empty() {
        return Ok(CmdOutput {
            stdout: "no sources to refresh".to_string(),
            ..Default::default()
        });
    }

    let mut created: Vec<PathBuf> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut failures = 0usize;
    for entry in &entries {
        match refresh_one_source(entry, deps, &mut created) {
            Ok(RefreshOutcome::Skipped { reason }) => {
                lines.push(format!("skipped '{}': {reason}", entry.name));
            }
            Ok(RefreshOutcome::Rebuilt { chunks_inserted }) => {
                lines.push(format!(
                    "refreshed '{}': {} chunks",
                    entry.name, chunks_inserted
                ));
            }
            Err(e) => {
                failures += 1;
                tracing::warn!(source = %entry.name, error = %e, "refresh failed");
                let line = format!("failed '{}': {e}", entry.name);
                warnings.push(line.clone());
                lines.push(line);
            }
        }
    }

    if failures == entries.len() && matches!(args.selector, SourceSelector::Single(_)) {
        return Err(anyhow!(
            lines
                .last()
                .cloned()
                .unwrap_or_else(|| "refresh failed".into())
        ));
    }

    Ok(CmdOutput {
        stdout: lines.join("\n"),
        created_paths: created,
        removed_paths: Vec::new(),
        warnings,
        doctor_failed: None,
    })
}

enum RefreshOutcome {
    Skipped { reason: String },
    Rebuilt { chunks_inserted: usize },
}

fn refresh_one_source<D: ContextDeps>(
    entry: &SourceEntry,
    deps: &D,
    created: &mut Vec<PathBuf>,
) -> Result<RefreshOutcome> {
    let layout = SourceLayout::for_source(deps.home_dir(), &entry.name);

    // Resolve current HEAD sha for the source (None for path-only or
    // non-git directories; always triggers re-index).
    let current_head = match source_repo_root(entry) {
        Some(root) if root.exists() => deps
            .git()
            .head_sha(root)
            .map_err(|e| anyhow!("git head_sha({}): {e}", root.display()))?,
        _ => None,
    };
    let current_model = deps.embedder().model_hash();

    if layout.state.exists()
        && let Some(on_disk) =
            IndexState::load(&layout.state).map_err(|e| anyhow!("load state.json: {e}"))?
    {
        let model_matches = on_disk.embedder_model_hash == current_model;
        let head_matches = match (&on_disk.head_sha, &current_head) {
            (Some(a), Some(b)) => a == b,
            // If either side is None (path source or unavailable git),
            // we can't certify "unchanged" — fall through to re-index.
            _ => false,
        };
        if model_matches && head_matches {
            return Ok(RefreshOutcome::Skipped {
                reason: format!("HEAD {} unchanged", current_head.as_deref().unwrap_or("?")),
            });
        }
    }

    let success = index_one_source(entry, deps, created)?;
    Ok(RefreshOutcome::Rebuilt {
        chunks_inserted: success.chunks_inserted,
    })
}

// --- Query handler ----------------------------------------------------------

fn run_query<D: ContextDeps>(args: &QueryArgs, deps: &D) -> Result<CmdOutput> {
    if args.text.trim().is_empty() {
        return Err(anyhow!("query text must not be empty"));
    }
    let cfg = load_sources_or_err(deps)?;

    // If --source is given, verify it's registered and that its index db
    // exists. Otherwise we query across any source that has an index db on
    // disk (sources that were never indexed are silently skipped; warning
    // surfaced so the user isn't confused by empty results).
    let (source_name, db_path) = resolve_query_target(deps, &cfg, args.source.as_deref())?;
    if !db_path.exists() {
        return Err(anyhow!(
            "source '{source_name}' has no index at {}; run `quorum context index --source {source_name}` first",
            db_path.display()
        ));
    }

    // The index schema uses the `vec0` virtual table; without the sqlite-vec
    // auto-extension registered, opening the db succeeds but any query that
    // touches `chunks_vec` fails with `no such module: vec0`. IndexBuilder
    // registers the hook during `index`, but a fresh process that jumps
    // straight to `query` has never run it.
    ensure_vec_loaded();

    let conn = Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| anyhow!("open {}: {e}", db_path.display()))?;

    let k = args.k.unwrap_or(5).max(1);
    let filters = if args.source.is_some() {
        Filters {
            sources: vec![source_name.clone()],
            kinds: Vec::new(),
            exclude_source_paths: vec![],
        }
    } else {
        Filters::default()
    };
    let q = RetrievalQuery {
        text: args.text.clone(),
        identifiers: Vec::new(),
        structural_names: Vec::new(),
        filters,
        k,
        min_score: 0.0,
        reviewed_file_language: None,
    };
    let retriever = Retriever::new(&conn, deps.embedder(), deps.clock());
    let hits = retriever.query(q).map_err(|e| anyhow!("query: {e}"))?;

    let stdout = match args.format {
        QueryFormat::Json => render_query_json(&hits, args.explain)?,
        QueryFormat::Compact => render_query_compact(&hits, args.explain),
        QueryFormat::Table => render_query_table(&hits, args.explain),
    };
    Ok(CmdOutput {
        stdout,
        created_paths: Vec::new(),
        removed_paths: Vec::new(),
        warnings: Vec::new(),
        doctor_failed: None,
    })
}

fn resolve_query_target<D: ContextDeps>(
    deps: &D,
    cfg: &SourcesConfig,
    explicit: Option<&str>,
) -> Result<(String, PathBuf)> {
    if let Some(name) = explicit {
        let _entry = find_source(cfg, name)?;
        let layout = SourceLayout::for_source(deps.home_dir(), name);
        return Ok((name.to_string(), layout.db));
    }
    // No --source given: pick the first source that has an index db on disk.
    // This is deliberately simple for MVP — the retriever already filters
    // across sources inside a single db, but each source currently has its
    // own db. A cross-source query surface is a later task.
    for entry in &cfg.sources {
        let layout = SourceLayout::for_source(deps.home_dir(), &entry.name);
        if layout.db.exists() {
            return Ok((entry.name.clone(), layout.db));
        }
    }
    Err(anyhow!(
        "no indexed sources found; run `quorum context index --all` first"
    ))
}

fn render_query_table(hits: &[crate::context::retrieve::ScoredChunk], explain: bool) -> String {
    if hits.is_empty() {
        return "no hits".to_string();
    }
    let mut out = String::new();
    out.push_str("RANK  SOURCE         SCORE   QUALIFIED_NAME\n");
    for (i, h) in hits.iter().enumerate() {
        let qn = h.chunk.qualified_name.as_deref().unwrap_or("-");
        out.push_str(&format!(
            "{:>4}  {:<14} {:>6.3}  {}\n",
            i + 1,
            truncate(&h.chunk.source, 14),
            h.score,
            qn
        ));
        if explain {
            out.push_str(&format!(
                "      bm25_norm={:.3} vec_norm={:.3} id_boost={:.3} path_boost={:.3} recency_mul={:.3}\n",
                h.components.bm25_norm,
                h.components.vec_norm,
                h.components.id_boost,
                h.components.path_boost,
                h.components.recency_mul,
            ));
        }
    }
    out
}

fn render_query_compact(hits: &[crate::context::retrieve::ScoredChunk], explain: bool) -> String {
    if hits.is_empty() {
        return "no hits".to_string();
    }
    let mut out = String::new();
    for (i, h) in hits.iter().enumerate() {
        let qn = h.chunk.qualified_name.as_deref().unwrap_or("-");
        if explain {
            out.push_str(&format!(
                "{}\t{}\t{:.3}\t{qn}\tbm25={:.3}\tvec={:.3}\tid={:.3}\tpath={:.3}\trecency={:.3}\n",
                i + 1,
                h.chunk.source,
                h.score,
                h.components.bm25_norm,
                h.components.vec_norm,
                h.components.id_boost,
                h.components.path_boost,
                h.components.recency_mul,
            ));
        } else {
            out.push_str(&format!(
                "{}\t{}\t{:.3}\t{qn}\n",
                i + 1,
                h.chunk.source,
                h.score,
            ));
        }
    }
    out
}

fn render_query_json(
    hits: &[crate::context::retrieve::ScoredChunk],
    explain: bool,
) -> Result<String> {
    let items: Vec<serde_json::Value> = hits
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let mut v = serde_json::json!({
                "rank": i + 1,
                "source": h.chunk.source,
                "qualified_name": h.chunk.qualified_name,
                "score": h.score,
                "chunk_id": h.chunk.id,
            });
            if explain {
                v["breakdown"] = serde_json::json!({
                    "bm25_norm": h.components.bm25_norm,
                    "vec_norm": h.components.vec_norm,
                    "id_boost": h.components.id_boost,
                    "path_boost": h.components.path_boost,
                    "recency_mul": h.components.recency_mul,
                    "score": h.components.score,
                });
            }
            v
        })
        .collect();
    let out = serde_json::json!({ "hits": items });
    Ok(serde_json::to_string_pretty(&out)?)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

// --- Prune / Doctor -------------------------------------------------------

/// Root directory under which every per-source dir is allowed to live.
/// Both prune and doctor join with this and then verify the result stays
/// bounded — protects against malicious `sources.toml` entries whose `name`
/// would escape (e.g. `../evil`).
fn sources_root(home: &Path) -> PathBuf {
    home.join("sources")
}

/// True if `name` is a safe single directory component: non-empty, no path
/// separators, no ".." / ".", no NUL. Mirrors the validation we'd ideally
/// enforce in `SourcesConfig::append_source`, but is duplicated here so a
/// hand-edited sources.toml can't fool prune into deleting unrelated dirs.
fn is_safe_source_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    if name == "." || name == ".." {
        return false;
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return false;
    }
    // Control chars (\n, \t, ...) would be disastrous in a path anyway.
    if name.chars().any(|c| c.is_control()) {
        return false;
    }
    true
}

/// Enumerate on-disk per-source dirs beneath `<home>/sources/`. Returns the
/// absolute path of each immediate subdirectory. Non-existent root => empty.
fn on_disk_source_dirs(home: &Path) -> Result<Vec<(String, PathBuf)>> {
    let root = sources_root(home);
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let entries =
        std::fs::read_dir(&root).map_err(|e| anyhow!("read_dir({}): {e}", root.display()))?;
    for ent in entries {
        let ent = ent.map_err(|e| anyhow!("read_dir entry: {e}"))?;
        let ft = ent.file_type().map_err(|e| anyhow!("file_type: {e}"))?;
        if !ft.is_dir() {
            continue;
        }
        // Only keep structurally safe names — a dir literally named ".."
        // can't exist via the FS (read_dir never yields it as an entry),
        // but defense in depth keeps the "inside sources root" invariant
        // straightforward to reason about.
        if let Some(name) = ent.file_name().to_str() {
            if !is_safe_source_name(name) {
                continue;
            }
            out.push((name.to_string(), ent.path()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Return (registered, config) — `registered` contains only names that are
/// safe per `is_safe_source_name`. Unsafe names raise a warning on the
/// caller side.
fn registered_safe_names(cfg: &SourcesConfig) -> (Vec<String>, Vec<String>) {
    let mut safe = Vec::new();
    let mut unsafe_names = Vec::new();
    for e in &cfg.sources {
        if is_safe_source_name(&e.name) {
            safe.push(e.name.clone());
        } else {
            unsafe_names.push(e.name.clone());
        }
    }
    (safe, unsafe_names)
}

fn run_prune<D: ContextDeps>(args: &PruneArgs, deps: &D) -> Result<CmdOutput> {
    let sources_path = deps.home_dir().join("sources.toml");
    let cfg = if sources_path.exists() {
        SourcesConfig::load(&sources_path).map_err(|e| anyhow!("{e}"))?
    } else {
        // No config => every sources/<x>/ dir is orphan. Proceed without
        // erroring: prune is inherently idempotent and this is a valid
        // "clean up an abandoned install" case.
        SourcesConfig::from_str("")
            .unwrap_or_else(|_| SourcesConfig::from_str("[context]\n").expect("empty config"))
    };
    let (registered, unsafe_names) = registered_safe_names(&cfg);
    let root = sources_root(deps.home_dir());
    let root_canon = root.canonicalize().unwrap_or_else(|_| root.clone());

    let dirs = on_disk_source_dirs(deps.home_dir())?;
    let mut removed: Vec<PathBuf> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut lines: Vec<String> = Vec::new();

    for name in &unsafe_names {
        warnings.push(format!(
            "sources.toml: source name '{}' is unsafe and will be ignored by prune",
            name
        ));
    }

    for (name, path) in &dirs {
        if registered.contains(name) {
            continue;
        }
        // Double-check containment: the canonicalized dir must live under
        // the canonicalized sources root. This is the belt-and-suspenders
        // check that makes the "refuses to touch paths outside sources
        // root" contract hold even if someone replaces a dir with a
        // symlink pointing outside.
        let path_canon = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !path_canon.starts_with(&root_canon) {
            warnings.push(format!(
                "skipping '{}': resolved path {} is outside {}",
                name,
                path_canon.display(),
                root_canon.display()
            ));
            continue;
        }

        if args.dry_run {
            lines.push(format!("would remove '{}': {}", name, path.display()));
            removed.push(path.clone());
        } else {
            match std::fs::remove_dir_all(path) {
                Ok(()) => {
                    lines.push(format!("removed '{}': {}", name, path.display()));
                    removed.push(path.clone());
                }
                Err(e) => {
                    let msg = format!("failed to remove '{}': {e}", name);
                    warnings.push(msg.clone());
                    lines.push(msg);
                }
            }
        }
    }

    if lines.is_empty() {
        lines.push(if args.dry_run {
            "dry-run: nothing to prune".to_string()
        } else {
            "nothing to prune".to_string()
        });
    } else if args.dry_run {
        lines.insert(0, "dry-run: no files modified".to_string());
    }

    Ok(CmdOutput {
        stdout: lines.join("\n"),
        created_paths: Vec::new(),
        removed_paths: removed,
        warnings,
        doctor_failed: None,
    })
}

// --- Doctor checks ----------------------------------------------------------

fn check_sources_toml<D: ContextDeps>(deps: &D) -> (CheckResult, Option<SourcesConfig>) {
    let path = deps.home_dir().join("sources.toml");
    if !path.exists() {
        return (
            CheckResult {
                name: "sources_toml_exists_and_parses",
                scope: None,
                status: CheckStatus::Fail { fixable: false },
                detail: format!(
                    "{} does not exist; run `quorum context init`",
                    path.display()
                ),
            },
            None,
        );
    }
    match SourcesConfig::load(&path) {
        Ok(cfg) => (
            CheckResult {
                name: "sources_toml_exists_and_parses",
                scope: None,
                status: CheckStatus::Pass,
                detail: format!("{} ok ({} sources)", path.display(), cfg.sources.len()),
            },
            Some(cfg),
        ),
        Err(e) => (
            CheckResult {
                name: "sources_toml_exists_and_parses",
                scope: None,
                status: CheckStatus::Fail { fixable: false },
                detail: format!("parse error: {e}"),
            },
            None,
        ),
    }
}

fn check_per_source_dir(home: &Path, name: &str) -> CheckResult {
    let dir = SourceLayout::for_source(home, name).dir;
    if dir.exists() && dir.is_dir() {
        CheckResult {
            name: "per_source_dirs_present",
            scope: Some(name.to_string()),
            status: CheckStatus::Pass,
            detail: format!("{} present", dir.display()),
        }
    } else {
        CheckResult {
            name: "per_source_dirs_present",
            scope: Some(name.to_string()),
            status: CheckStatus::Fail { fixable: true },
            detail: format!("missing dir: {}", dir.display()),
        }
    }
}

fn check_chunks_jsonl(home: &Path, name: &str) -> CheckResult {
    let jsonl = SourceLayout::for_source(home, name).jsonl;
    if !jsonl.exists() {
        return CheckResult {
            name: "per_source_chunks_jsonl_readable",
            scope: Some(name.to_string()),
            status: CheckStatus::Fail { fixable: false },
            detail: format!("missing: {}", jsonl.display()),
        };
    }
    match std::fs::metadata(&jsonl) {
        Ok(meta) => {
            if meta.len() == 0 {
                CheckResult {
                    name: "per_source_chunks_jsonl_readable",
                    scope: Some(name.to_string()),
                    status: CheckStatus::Warn,
                    detail: format!("{} is empty", jsonl.display()),
                }
            } else if std::fs::File::open(&jsonl).is_ok() {
                CheckResult {
                    name: "per_source_chunks_jsonl_readable",
                    scope: Some(name.to_string()),
                    status: CheckStatus::Pass,
                    detail: format!("{} bytes", meta.len()),
                }
            } else {
                CheckResult {
                    name: "per_source_chunks_jsonl_readable",
                    scope: Some(name.to_string()),
                    status: CheckStatus::Fail { fixable: false },
                    detail: format!("cannot open {}", jsonl.display()),
                }
            }
        }
        Err(e) => CheckResult {
            name: "per_source_chunks_jsonl_readable",
            scope: Some(name.to_string()),
            status: CheckStatus::Fail { fixable: false },
            detail: format!("stat failed: {e}"),
        },
    }
}

fn db_chunk_count(db: &Path) -> Result<i64> {
    // Register the vec0 auto-extension hook even though this function only
    // queries `chunks`; doctor may grow checks that touch `chunks_vec` in
    // the future and we don't want a subtle "works in tests, fails in
    // production" split between the fresh-process and post-index cases.
    ensure_vec_loaded();
    let conn = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let n: i64 = conn.query_row("SELECT count(*) FROM chunks", [], |r| r.get(0))?;
    Ok(n)
}

fn check_index_db(home: &Path, name: &str) -> (CheckResult, Option<i64>) {
    let db = SourceLayout::for_source(home, name).db;
    if !db.exists() {
        return (
            CheckResult {
                name: "per_source_index_db_opens",
                scope: Some(name.to_string()),
                status: CheckStatus::Fail { fixable: true },
                detail: format!("missing: {}", db.display()),
            },
            None,
        );
    }
    match db_chunk_count(&db) {
        Ok(n) => (
            CheckResult {
                name: "per_source_index_db_opens",
                scope: Some(name.to_string()),
                status: CheckStatus::Pass,
                detail: format!("{n} rows in chunks"),
            },
            Some(n),
        ),
        Err(e) => (
            CheckResult {
                name: "per_source_index_db_opens",
                scope: Some(name.to_string()),
                status: CheckStatus::Fail { fixable: true },
                detail: format!("open/query failed: {e}"),
            },
            None,
        ),
    }
}

fn count_jsonl_lines(p: &Path) -> std::io::Result<usize> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(p)?;
    let r = BufReader::new(f);
    let mut n = 0usize;
    for line in r.lines() {
        let line = line?;
        if !line.trim().is_empty() {
            n += 1;
        }
    }
    Ok(n)
}

fn check_db_jsonl_agreement(home: &Path, name: &str, db_count: Option<i64>) -> CheckResult {
    let layout = SourceLayout::for_source(home, name);
    if db_count.is_none() || !layout.jsonl.exists() {
        return CheckResult {
            name: "per_source_index_db_matches_jsonl",
            scope: Some(name.to_string()),
            status: CheckStatus::Warn,
            detail: "skipped: db or jsonl unavailable".into(),
        };
    }
    let n_db = db_count.unwrap();
    let n_jsonl = match count_jsonl_lines(&layout.jsonl) {
        Ok(n) => n as i64,
        Err(e) => {
            return CheckResult {
                name: "per_source_index_db_matches_jsonl",
                scope: Some(name.to_string()),
                status: CheckStatus::Warn,
                detail: format!("jsonl read error: {e}"),
            };
        }
    };
    if n_db == n_jsonl {
        CheckResult {
            name: "per_source_index_db_matches_jsonl",
            scope: Some(name.to_string()),
            status: CheckStatus::Pass,
            detail: format!("{n_db} chunks match"),
        }
    } else {
        CheckResult {
            name: "per_source_index_db_matches_jsonl",
            scope: Some(name.to_string()),
            status: CheckStatus::Warn,
            detail: format!("db={n_db} jsonl={n_jsonl}"),
        }
    }
}

fn check_state_json<D: ContextDeps>(deps: &D, name: &str) -> CheckResult {
    let state_path = SourceLayout::for_source(deps.home_dir(), name).state;
    if !state_path.exists() {
        return CheckResult {
            name: "per_source_state_json_valid",
            scope: Some(name.to_string()),
            status: CheckStatus::Fail { fixable: true },
            detail: format!("missing: {}", state_path.display()),
        };
    }
    match IndexState::load(&state_path) {
        Ok(Some(s)) => {
            let expected = deps.embedder().model_hash();
            if s.embedder_model_hash == expected {
                CheckResult {
                    name: "per_source_state_json_valid",
                    scope: Some(name.to_string()),
                    status: CheckStatus::Pass,
                    detail: format!("schema v{}, hash match", s.schema_version),
                }
            } else {
                CheckResult {
                    name: "per_source_state_json_valid",
                    scope: Some(name.to_string()),
                    status: CheckStatus::Fail { fixable: true },
                    detail: format!(
                        "embedder model hash mismatch: on-disk={} expected={}",
                        s.embedder_model_hash, expected
                    ),
                }
            }
        }
        Ok(None) => CheckResult {
            name: "per_source_state_json_valid",
            scope: Some(name.to_string()),
            status: CheckStatus::Fail { fixable: true },
            detail: "empty state.json".to_string(),
        },
        Err(e) => CheckResult {
            name: "per_source_state_json_valid",
            scope: Some(name.to_string()),
            status: CheckStatus::Fail { fixable: false },
            detail: format!("parse error: {e}"),
        },
    }
}

fn check_orphan_dirs<D: ContextDeps>(deps: &D, cfg: Option<&SourcesConfig>) -> Result<CheckResult> {
    let registered: std::collections::HashSet<String> = cfg
        .map(|c| c.sources.iter().map(|s| s.name.clone()).collect())
        .unwrap_or_default();
    let dirs = on_disk_source_dirs(deps.home_dir())?;
    let orphans: Vec<String> = dirs
        .into_iter()
        .filter(|(name, _)| !registered.contains(name))
        .map(|(name, _)| name)
        .collect();
    Ok(if orphans.is_empty() {
        CheckResult {
            name: "orphan_source_dirs",
            scope: None,
            status: CheckStatus::Pass,
            detail: "no orphans".into(),
        }
    } else {
        CheckResult {
            name: "orphan_source_dirs",
            scope: None,
            status: CheckStatus::Warn,
            detail: format!("orphan dirs: {}", orphans.join(", ")),
        }
    })
}

fn run_doctor<D: ContextDeps>(args: &DoctorArgs, deps: &D) -> Result<CmdOutput> {
    let (toml_check, cfg) = check_sources_toml(deps);
    let mut checks: Vec<CheckResult> = vec![toml_check];

    if let Some(cfg) = &cfg {
        for entry in &cfg.sources {
            if !is_safe_source_name(&entry.name) {
                checks.push(CheckResult {
                    name: "per_source_dirs_present",
                    scope: Some(entry.name.clone()),
                    status: CheckStatus::Fail { fixable: false },
                    detail: "unsafe source name; skipping per-source checks".into(),
                });
                continue;
            }
            let dir_check = check_per_source_dir(deps.home_dir(), &entry.name);
            let dir_ok = matches!(dir_check.status, CheckStatus::Pass);
            checks.push(dir_check);

            if dir_ok {
                checks.push(check_chunks_jsonl(deps.home_dir(), &entry.name));
                let (db_check, db_count) = check_index_db(deps.home_dir(), &entry.name);
                checks.push(db_check);
                checks.push(check_db_jsonl_agreement(
                    deps.home_dir(),
                    &entry.name,
                    db_count,
                ));
                checks.push(check_state_json(deps, &entry.name));
            } else {
                // Still enumerate the names so the JSON schema contract
                // holds even when the per-source dir is missing.
                for missing_name in [
                    "per_source_chunks_jsonl_readable",
                    "per_source_index_db_opens",
                    "per_source_index_db_matches_jsonl",
                    "per_source_state_json_valid",
                ] {
                    checks.push(CheckResult {
                        name: missing_name,
                        scope: Some(entry.name.clone()),
                        status: CheckStatus::Fail { fixable: true },
                        detail: "skipped: source dir missing".into(),
                    });
                }
            }
        }
    }

    checks.push(check_orphan_dirs(deps, cfg.as_ref())?);

    // Repair pass.
    let mut created: Vec<PathBuf> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut repair_lines: Vec<String> = Vec::new();
    if args.repair {
        if let Some(cfg) = &cfg {
            for entry in &cfg.sources {
                if !is_safe_source_name(&entry.name) {
                    warnings.push(format!(
                        "skipping repair for unsafe source name '{}'",
                        entry.name
                    ));
                    continue;
                }
                if let Err(e) = repair_one_source(deps, entry, &mut created, &mut repair_lines) {
                    // Best-effort: note the failure and continue.
                    warnings.push(format!("repair '{}' failed: {e}", entry.name));
                    repair_lines.push(format!("failed '{}': {e}", entry.name));
                }
            }
        }

        // Re-run checks so the post-repair report reflects reality.
        let (toml_check2, cfg2) = check_sources_toml(deps);
        checks = vec![toml_check2];
        if let Some(cfg2) = &cfg2 {
            for entry in &cfg2.sources {
                if !is_safe_source_name(&entry.name) {
                    continue;
                }
                let dir_check = check_per_source_dir(deps.home_dir(), &entry.name);
                let dir_ok = matches!(dir_check.status, CheckStatus::Pass);
                checks.push(dir_check);
                if dir_ok {
                    checks.push(check_chunks_jsonl(deps.home_dir(), &entry.name));
                    let (db_check, db_count) = check_index_db(deps.home_dir(), &entry.name);
                    checks.push(db_check);
                    checks.push(check_db_jsonl_agreement(
                        deps.home_dir(),
                        &entry.name,
                        db_count,
                    ));
                    checks.push(check_state_json(deps, &entry.name));
                } else {
                    for missing_name in [
                        "per_source_chunks_jsonl_readable",
                        "per_source_index_db_opens",
                        "per_source_index_db_matches_jsonl",
                        "per_source_state_json_valid",
                    ] {
                        checks.push(CheckResult {
                            name: missing_name,
                            scope: Some(entry.name.clone()),
                            status: CheckStatus::Fail { fixable: true },
                            detail: "skipped: source dir missing".into(),
                        });
                    }
                }
            }
        }
        checks.push(check_orphan_dirs(deps, cfg2.as_ref())?);
    }

    let any_fail = checks
        .iter()
        .any(|c| matches!(c.status, CheckStatus::Fail { .. }));
    let stdout = match args.format {
        DoctorFormat::Json => render_doctor_json(&checks, !any_fail, &repair_lines)?,
        DoctorFormat::Compact => render_doctor_compact(&checks, &repair_lines),
        DoctorFormat::Table => render_doctor_table(&checks, !any_fail, &repair_lines),
    };

    Ok(CmdOutput {
        stdout,
        created_paths: created,
        removed_paths: Vec::new(),
        warnings,
        doctor_failed: Some(any_fail),
    })
}

fn repair_one_source<D: ContextDeps>(
    deps: &D,
    entry: &SourceEntry,
    created: &mut Vec<PathBuf>,
    lines: &mut Vec<String>,
) -> Result<()> {
    let layout = SourceLayout::for_source(deps.home_dir(), &entry.name);

    // 1. Missing source dir.
    if !layout.dir.exists() {
        ensure_dir(&layout.dir)?;
        created.push(layout.dir.clone());
        lines.push(format!("created dir for '{}'", entry.name));
    }

    // 2. Missing index.db but jsonl present => rebuild.
    let jsonl_exists = layout.jsonl.exists();
    let db_exists = layout.db.exists();
    let db_ok = db_exists && db_chunk_count(&layout.db).is_ok();

    // 3. State hash check.
    let needs_reembed = match IndexState::load(&layout.state) {
        Ok(Some(s)) => s.embedder_model_hash != deps.embedder().model_hash(),
        Ok(None) => true,
        Err(_) => true,
    };

    if (!db_ok || needs_reembed) && jsonl_exists {
        // Wipe broken db so rebuild gets a clean file.
        if db_exists && !db_ok {
            let _ = std::fs::remove_file(&layout.db);
        }
        let mut builder = IndexBuilder::new(&layout.db, deps.clock(), deps.embedder())
            .map_err(|e| anyhow!("open index db: {e}"))?;
        let _report = builder
            .rebuild_from_jsonl(&entry.name, &layout.jsonl)
            .map_err(|e| anyhow!("rebuild failed: {e}"))?;
        created.push(layout.db.clone());
        lines.push(format!("rebuilt index for '{}'", entry.name));

        // Write fresh state.json.
        let head_sha = match source_repo_root(entry) {
            Some(root) if root.exists() => deps.git().head_sha(root).unwrap_or(None),
            _ => None,
        };
        let state = IndexState::new(deps.embedder().model_hash())
            .with_head_sha(head_sha)
            .with_indexed_at(deps.clock().now());
        state
            .save(&layout.state)
            .map_err(|e| anyhow!("save state.json: {e}"))?;
        created.push(layout.state.clone());
    } else if !jsonl_exists && !db_ok {
        // Neither side exists — leave a note for the user.
        lines.push(format!(
            "cannot repair '{}': no chunks.jsonl to rebuild from",
            entry.name
        ));
    }
    Ok(())
}

fn render_doctor_json(checks: &[CheckResult], ok: bool, repair_lines: &[String]) -> Result<String> {
    let items: Vec<serde_json::Value> = checks
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "scope": c.scope,
                "status": c.status.as_str(),
                "fixable": c.status.fixable(),
                "detail": c.detail,
            })
        })
        .collect();
    let mut out = serde_json::json!({
        "ok": ok,
        "checks": items,
    });
    if !repair_lines.is_empty() {
        out["repair"] = serde_json::json!(repair_lines);
    }
    Ok(serde_json::to_string_pretty(&out)?)
}

fn render_doctor_table(checks: &[CheckResult], ok: bool, repair_lines: &[String]) -> String {
    let mut out = String::new();
    out.push_str("STATUS  CHECK                                    SCOPE      DETAIL\n");
    for c in checks {
        out.push_str(&format!(
            "{:<7} {:<40} {:<10} {}\n",
            c.status.as_str(),
            c.name,
            c.scope.as_deref().unwrap_or("-"),
            c.detail
        ));
    }
    out.push_str(&format!("\noverall: {}\n", if ok { "ok" } else { "fail" }));
    if !repair_lines.is_empty() {
        out.push_str("\nrepair log:\n");
        for l in repair_lines {
            out.push_str(&format!("  {l}\n"));
        }
    }
    out
}

fn render_doctor_compact(checks: &[CheckResult], repair_lines: &[String]) -> String {
    let mut out = String::new();
    for c in checks {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            c.status.as_str(),
            c.name,
            c.scope.as_deref().unwrap_or("-"),
            c.detail
        ));
    }
    for l in repair_lines {
        out.push_str(&format!("repair\t{l}\n"));
    }
    out
}
