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
use rusqlite::Connection;

use crate::context::config::{SourceEntry, SourceKind, SourceLocation, SourcesConfig};
use crate::context::extract::dispatch::{extract_source, ExtractConfig};
use crate::context::index::builder::IndexBuilder;
use crate::context::index::state::IndexState;
use crate::context::index::traits::{Clock, Embedder, HashEmbedder, SystemClock};
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
    Prune,
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
            ContextCmd::Prune => "prune",
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
        ContextCmd::Add(args) => run_add(args, deps),
        ContextCmd::List(args) => run_list(args, deps),
        ContextCmd::Index(args) => run_index(args, deps),
        ContextCmd::Refresh(args) => run_refresh(args, deps),
        ContextCmd::Query(args) => run_query(args, deps),
        other => Err(anyhow!("not yet implemented: context {}", other.name())),
    }
}

// --- Init handler -----------------------------------------------------------

fn run_init<D: ContextDeps>(deps: &D) -> Result<CmdOutput> {
    // `home_dir()` already points at the quorum state root (e.g. `~/.quorum`
    // in production, a tempdir in tests) — don't append `.quorum` again here.
    let sources_path = deps.home_dir().join("sources.toml");

    // Distinguish "already a regular file" (idempotent success) from "the
    // path exists but is a directory / symlink / device" (hard error). The
    // bare `.exists()` check can't tell them apart; without this, a stray
    // `mkdir sources.toml` under `~/.quorum` makes every future `init` and
    // `add` silently no-op instead of complaining.
    if sources_path.exists() {
        let meta = std::fs::symlink_metadata(&sources_path).map_err(|e| {
            anyhow!(
                "cannot stat {}: {e}",
                sources_path.display()
            )
        })?;
        if !meta.file_type().is_file() {
            return Err(anyhow!(
                "{} exists but is not a regular file; refusing to initialize over it",
                sources_path.display()
            ));
        }
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

// --- Add handler ------------------------------------------------------------

fn run_add<D: ContextDeps>(args: &AddArgs, deps: &D) -> Result<CmdOutput> {
    // Up-front validation: failing here means the on-disk file is never
    // touched, which is the cheapest way to satisfy the atomicity contract.
    let name = args.name.trim();
    if name.is_empty() {
        return Err(anyhow!("source name must not be empty"));
    }
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
    SourcesConfig::append_source(&sources_path, &entry)
        .map_err(|e| anyhow!("{e}"))?;

    Ok(CmdOutput {
        stdout: format!("added source '{}'", entry.name),
        created_paths: Vec::new(),
        warnings: Vec::new(),
    })
}

// --- List handler -----------------------------------------------------------

fn run_list<D: ContextDeps>(args: &ListArgs, deps: &D) -> Result<CmdOutput> {
    let sources_path = deps.home_dir().join("sources.toml");
    if !sources_path.exists() {
        let msg =
            "no sources registered; run `quorum context init` first".to_string();
        return Ok(CmdOutput {
            stdout: msg.clone(),
            created_paths: Vec::new(),
            warnings: vec![msg],
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
        warnings: Vec::new(),
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
        let weight = e.weight.map(|w| w.to_string()).unwrap_or_else(|| "-".into());
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
struct SourceLayout {
    dir: PathBuf,
    jsonl: PathBuf,
    db: PathBuf,
    state: PathBuf,
}

impl SourceLayout {
    fn for_source(home: &Path, name: &str) -> Self {
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
        warnings,
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
            lines.last().cloned().unwrap_or_else(|| "refresh failed".into())
        ));
    }

    Ok(CmdOutput {
        stdout: lines.join("\n"),
        created_paths: created,
        warnings,
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

    let conn = Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| anyhow!("open {}: {e}", db_path.display()))?;

    let k = args.k.unwrap_or(5).max(1);
    let filters = if args.source.is_some() {
        Filters {
            sources: vec![source_name.clone()],
            kinds: Vec::new(),
        }
    } else {
        Filters::default()
    };
    let q = RetrievalQuery {
        text: args.text.clone(),
        identifiers: Vec::new(),
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
        warnings: Vec::new(),
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

fn render_query_table(
    hits: &[crate::context::retrieve::ScoredChunk],
    explain: bool,
) -> String {
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

fn render_query_compact(
    hits: &[crate::context::retrieve::ScoredChunk],
    explain: bool,
) -> String {
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
