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

use crate::context::config::{SourceEntry, SourceKind, SourceLocation, SourcesConfig};
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
        ContextCmd::Add(args) => run_add(args, deps),
        ContextCmd::List(args) => run_list(args, deps),
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
            SourceLocation::Git {
                url: url.trim().to_string(),
                rev: rev.as_ref().map(|r| r.trim().to_string()),
            }
        }
    };

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
