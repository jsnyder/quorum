//! `.quorum/sources.toml` loader.
//!
//! Parses external-source definitions and the `[context]` tuning block used by
//! the context injection feature. Validates mutual exclusion of git/path,
//! uniqueness of source names, and bounded numeric ranges.

use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Rust,
    Typescript,
    Javascript,
    Python,
    Go,
    Terraform,
    Service,
    Docs,
}

impl SourceKind {
    /// Canonical snake_case identifier used in `sources.toml` and in all
    /// machine-readable outputs (`list --json`, etc.). Kept in sync with the
    /// `Deserialize` impl on `RawKind`.
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceKind::Rust => "rust",
            SourceKind::Typescript => "typescript",
            SourceKind::Javascript => "javascript",
            SourceKind::Python => "python",
            SourceKind::Go => "go",
            SourceKind::Terraform => "terraform",
            SourceKind::Service => "service",
            SourceKind::Docs => "docs",
        }
    }

    /// Parse a user-supplied kind string. Accepts a few common aliases
    /// (`ts` -> `typescript`, `js` -> `javascript`, `py` -> `python`,
    /// `tf` -> `terraform`) to match CLI ergonomics from the task plan.
    pub fn parse_cli(s: &str) -> Option<SourceKind> {
        Some(match s.trim() {
            "rust" | "rs" => SourceKind::Rust,
            "typescript" | "ts" => SourceKind::Typescript,
            "javascript" | "js" => SourceKind::Javascript,
            "python" | "py" => SourceKind::Python,
            "go" => SourceKind::Go,
            "terraform" | "tf" => SourceKind::Terraform,
            "service" => SourceKind::Service,
            "docs" => SourceKind::Docs,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceLocation {
    Git { url: String, rev: Option<String> },
    Path(PathBuf),
}

#[derive(Debug, Clone)]
pub struct SourceEntry {
    pub name: String,
    pub kind: SourceKind,
    pub location: SourceLocation,
    pub paths: Vec<PathBuf>,
    pub weight: Option<i32>,
    pub ignore: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SourcesConfig {
    pub sources: Vec<SourceEntry>,
    pub context: ContextConfig,
}

#[derive(Debug, Clone)]
pub struct ContextConfig {
    pub auto_inject: bool,
    pub inject_budget_tokens: u32,
    pub inject_min_score: f32,
    pub inject_max_chunks: u32,
    pub rerank_recency_halflife_days: u32,
    pub rerank_recency_floor: f32,
    pub max_source_size_mb: u32,
    pub ignore: Vec<String>,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            auto_inject: true,
            inject_budget_tokens: 1500,
            inject_min_score: 0.65,
            inject_max_chunks: 4,
            rerank_recency_halflife_days: 90,
            rerank_recency_floor: 0.25,
            max_source_size_mb: 200,
            ignore: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read {path}: {source}", path = path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid config: {0}")]
    Invalid(String),
}

// --- Raw TOML shapes --------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default, rename = "source")]
    source: Vec<RawSource>,
    #[serde(default)]
    context: Option<RawContext>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    name: String,
    kind: RawKind,
    #[serde(default)]
    git: Option<String>,
    #[serde(default)]
    rev: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    weight: Option<i32>,
    #[serde(default)]
    ignore: Vec<String>,
}

// Custom kind wrapper so we can emit a friendlier "unknown kind" message
// without relying on serde's default-variant phrasing.
#[derive(Debug)]
struct RawKind(SourceKind);

impl<'de> Deserialize<'de> for RawKind {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(de)?;
        let kind = match s.as_str() {
            "rust" => SourceKind::Rust,
            "typescript" => SourceKind::Typescript,
            "javascript" => SourceKind::Javascript,
            "python" => SourceKind::Python,
            "go" => SourceKind::Go,
            "terraform" => SourceKind::Terraform,
            "service" => SourceKind::Service,
            "docs" => SourceKind::Docs,
            other => {
                return Err(serde::de::Error::custom(format!(
                    "unknown kind '{other}' (expected one of: rust, typescript, javascript, python, go, terraform, service, docs)"
                )));
            }
        };
        Ok(RawKind(kind))
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawContext {
    #[serde(default)]
    auto_inject: Option<bool>,
    #[serde(default)]
    inject_budget_tokens: Option<u32>,
    #[serde(default)]
    inject_min_score: Option<f32>,
    #[serde(default)]
    inject_max_chunks: Option<u32>,
    #[serde(default)]
    rerank_recency_halflife_days: Option<u32>,
    #[serde(default)]
    rerank_recency_floor: Option<f32>,
    #[serde(default)]
    max_source_size_mb: Option<u32>,
    #[serde(default)]
    ignore: Vec<String>,
}

// --- Public API -------------------------------------------------------------

impl SourcesConfig {
    pub fn from_str(toml_text: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(toml_text)?;
        Self::from_raw(raw)
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_str(&text)
    }

    /// Append a new `[[source]]` block to `sources.toml`.
    ///
    /// Validates first (name non-empty, location non-empty, duplicate-name
    /// check against the on-disk file), then writes atomically using a
    /// sibling tempfile + rename. On any failure the on-disk file is
    /// byte-identical to before the call.
    ///
    /// The writer is surgical: it re-reads the existing text and appends a
    /// freshly-rendered fragment rather than re-serializing the whole
    /// config. This preserves any hand edits, comments, and formatting in
    /// the `[context]` block and existing `[[source]]` entries.
    pub fn append_source(path: &Path, entry: &SourceEntry) -> Result<(), ConfigError> {
        if entry.name.trim().is_empty() {
            return Err(ConfigError::Invalid("source name must not be empty".into()));
        }
        match &entry.location {
            SourceLocation::Path(p) => {
                if p.as_os_str().is_empty() {
                    return Err(ConfigError::Invalid(format!(
                        "source '{}': path must not be empty",
                        entry.name
                    )));
                }
            }
            SourceLocation::Git { url, .. } => {
                if url.trim().is_empty() {
                    return Err(ConfigError::Invalid(format!(
                        "source '{}': git url must not be empty",
                        entry.name
                    )));
                }
            }
        }

        // Re-parse to check duplicate name — single source of truth for
        // uniqueness is the on-disk file, not an in-memory cache.
        let existing_text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let existing = Self::from_str(&existing_text)?;
        if existing.sources.iter().any(|e| e.name == entry.name) {
            return Err(ConfigError::Invalid(format!(
                "duplicate source name: {}",
                entry.name
            )));
        }

        let fragment = render_source_fragment(entry);
        let mut new_text = existing_text;
        if !new_text.ends_with('\n') {
            new_text.push('\n');
        }
        new_text.push_str(&fragment);

        // Atomic write: tmp sibling + rename. On POSIX rename is atomic
        // within the same filesystem, so a crash mid-write leaves the
        // original untouched.
        let parent = path.parent().ok_or_else(|| {
            ConfigError::Invalid(format!(
                "sources.toml path has no parent: {}",
                path.display()
            ))
        })?;
        // Compose pid + monotonic-nanos so concurrent or rapid sequential
        // calls in the same process don't collide on the tempfile name.
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp = parent.join(format!(
            ".sources.toml.tmp-{}-{}",
            std::process::id(),
            suffix
        ));
        std::fs::write(&tmp, new_text.as_bytes()).map_err(|source| ConfigError::Io {
            path: tmp.clone(),
            source,
        })?;
        std::fs::rename(&tmp, path).map_err(|source| {
            // Best-effort cleanup; swallow the secondary error.
            let _ = std::fs::remove_file(&tmp);
            ConfigError::Io {
                path: path.to_path_buf(),
                source,
            }
        })?;
        Ok(())
    }

    /// Write a minimal `sources.toml` containing only the `[context]` block
    /// populated with defaults. Creates parent directories as needed.
    ///
    /// Used by `quorum context init`. No-op on callers: this always writes a
    /// fresh file, so callers should guard against clobbering an existing one.
    pub fn write_default(path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let body = default_sources_toml();
        std::fs::write(path, body).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// Render the bundled default `sources.toml` as a string. Exposed for tests
/// and `init` command templating; contains a `[context]` block with the
/// numeric defaults from `ContextConfig::default()` and a comment header.
pub fn default_sources_toml() -> String {
    let d = ContextConfig::default();
    // Hand-rolled TOML: the raw parse structs are Deserialize-only, and
    // adding Serialize here would ripple through a lot of test fixtures.
    // Keeping the writer local is cheaper and keeps key ordering stable.
    format!(
        "# quorum context sources\n\
         # External sources to extract context from. Add entries with:\n\
         #   quorum context add <name> --kind <kind> (--git <url> | --path <dir>)\n\
         \n\
         [context]\n\
         auto_inject = {auto_inject}\n\
         inject_budget_tokens = {inject_budget_tokens}\n\
         inject_min_score = {inject_min_score}\n\
         inject_max_chunks = {inject_max_chunks}\n\
         rerank_recency_halflife_days = {rerank_recency_halflife_days}\n\
         rerank_recency_floor = {rerank_recency_floor}\n\
         max_source_size_mb = {max_source_size_mb}\n",
        auto_inject = d.auto_inject,
        inject_budget_tokens = d.inject_budget_tokens,
        inject_min_score = format_finite_f32(d.inject_min_score),
        inject_max_chunks = d.inject_max_chunks,
        rerank_recency_halflife_days = d.rerank_recency_halflife_days,
        rerank_recency_floor = format_finite_f32(d.rerank_recency_floor),
        max_source_size_mb = d.max_source_size_mb,
    )
}

/// Render a single `[[source]]` TOML block. Uses `toml::Value` escaping for
/// strings so exotic names/urls (quotes, backslashes) round-trip correctly.
/// Only emits optional fields when present — mirroring what a hand-written
/// file would look like.
fn render_source_fragment(entry: &SourceEntry) -> String {
    fn tq(s: &str) -> String {
        // Basic TOML string escape via serde: cheaper than hand-rolling.
        toml::Value::String(s.to_string()).to_string()
    }
    fn tq_array(items: &[String]) -> String {
        let parts: Vec<String> = items.iter().map(|s| tq(s)).collect();
        format!("[{}]", parts.join(", "))
    }

    let mut out = String::new();
    out.push_str("\n[[source]]\n");
    out.push_str(&format!("name = {}\n", tq(&entry.name)));
    out.push_str(&format!("kind = {}\n", tq(entry.kind.as_str())));
    match &entry.location {
        SourceLocation::Path(p) => {
            out.push_str(&format!("path = {}\n", tq(&p.display().to_string())));
        }
        SourceLocation::Git { url, rev } => {
            out.push_str(&format!("git = {}\n", tq(url)));
            if let Some(r) = rev {
                out.push_str(&format!("rev = {}\n", tq(r)));
            }
        }
    }
    if let Some(w) = entry.weight {
        out.push_str(&format!("weight = {w}\n"));
    }
    if !entry.ignore.is_empty() {
        out.push_str(&format!("ignore = {}\n", tq_array(&entry.ignore)));
    }
    out
}

fn format_finite_f32(v: f32) -> String {
    // Ensure TOML always sees a decimal point so the value round-trips as a
    // float (and not an integer) through the raw parser.
    let s = format!("{v}");
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{s}.0")
    }
}

impl SourcesConfig {
    fn from_raw(raw: RawConfig) -> Result<Self, ConfigError> {
        let mut sources = Vec::with_capacity(raw.source.len());
        let mut seen = HashSet::new();

        for rs in raw.source {
            if !seen.insert(rs.name.clone()) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate source name: {}",
                    rs.name
                )));
            }

            let git_opt = rs.git.as_deref().map(str::trim).filter(|s| !s.is_empty());
            let path_opt = rs.path.as_deref().map(str::trim).filter(|s| !s.is_empty());

            let location = match (git_opt, path_opt) {
                (Some(url), None) => SourceLocation::Git {
                    url: url.to_string(),
                    rev: rs.rev.clone(),
                },
                (None, Some(p)) => {
                    if rs.rev.is_some() {
                        return Err(ConfigError::Invalid(format!(
                            "source '{}': `rev` only applies to git sources, not path sources",
                            rs.name
                        )));
                    }
                    SourceLocation::Path(PathBuf::from(p))
                }
                (Some(_), Some(_)) | (None, None) => {
                    return Err(ConfigError::Invalid(format!(
                        "source '{}': must specify exactly one non-empty `git` or `path`",
                        rs.name
                    )));
                }
            };

            sources.push(SourceEntry {
                name: rs.name,
                kind: rs.kind.0,
                location,
                paths: rs.paths.into_iter().map(PathBuf::from).collect(),
                weight: rs.weight,
                ignore: rs.ignore,
            });
        }

        let context = build_context(raw.context.unwrap_or_default())?;

        Ok(SourcesConfig { sources, context })
    }
}

fn build_context(raw: RawContext) -> Result<ContextConfig, ConfigError> {
    let defaults = ContextConfig::default();
    let ctx = ContextConfig {
        auto_inject: raw.auto_inject.unwrap_or(defaults.auto_inject),
        inject_budget_tokens: raw
            .inject_budget_tokens
            .unwrap_or(defaults.inject_budget_tokens),
        inject_min_score: raw.inject_min_score.unwrap_or(defaults.inject_min_score),
        inject_max_chunks: raw.inject_max_chunks.unwrap_or(defaults.inject_max_chunks),
        rerank_recency_halflife_days: raw
            .rerank_recency_halflife_days
            .unwrap_or(defaults.rerank_recency_halflife_days),
        rerank_recency_floor: raw
            .rerank_recency_floor
            .unwrap_or(defaults.rerank_recency_floor),
        max_source_size_mb: raw.max_source_size_mb.unwrap_or(defaults.max_source_size_mb),
        ignore: raw.ignore,
    };

    if !ctx.inject_min_score.is_finite() {
        return Err(ConfigError::Invalid(format!(
            "inject_min_score must be finite, got {}",
            ctx.inject_min_score
        )));
    }
    if !(0.0..=1.0).contains(&ctx.inject_min_score) {
        return Err(ConfigError::Invalid(format!(
            "inject_min_score must be in [0.0, 1.0], got {}",
            ctx.inject_min_score
        )));
    }
    if !ctx.rerank_recency_floor.is_finite() {
        return Err(ConfigError::Invalid(format!(
            "rerank_recency_floor must be finite, got {}",
            ctx.rerank_recency_floor
        )));
    }
    if !(0.0..=1.0).contains(&ctx.rerank_recency_floor) {
        return Err(ConfigError::Invalid(format!(
            "rerank_recency_floor must be in [0.0, 1.0], got {}",
            ctx.rerank_recency_floor
        )));
    }
    if ctx.inject_budget_tokens == 0 {
        return Err(ConfigError::Invalid(
            "inject_budget_tokens must be greater than 0".into(),
        ));
    }
    if ctx.inject_max_chunks == 0 {
        return Err(ConfigError::Invalid(
            "inject_max_chunks must be greater than 0".into(),
        ));
    }
    if ctx.rerank_recency_halflife_days == 0 {
        return Err(ConfigError::Invalid(
            "rerank_recency_halflife_days must be greater than 0".into(),
        ));
    }
    if ctx.max_source_size_mb == 0 {
        return Err(ConfigError::Invalid(
            "max_source_size_mb must be greater than 0".into(),
        ));
    }

    Ok(ctx)
}
