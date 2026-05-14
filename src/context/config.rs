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
    pub provides: Vec<String>,
    pub include_for: Vec<String>,
    pub exclude_for: Vec<String>,
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
    pub multi_source: MultiSourceConfig,
}

#[derive(Debug, Clone)]
pub struct MultiSourceConfig {
    pub enabled: bool,
    pub max_sources_queried: u32,
    pub per_source_cap: u32,
    pub current_repo_reserved: u32,
    pub current_repo_boost: f32,
    pub dep_manifest_boost: f32,
    pub lang_match_boost: f32,
}

impl Default for MultiSourceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_sources_queried: 10,
            per_source_cap: 3,
            current_repo_reserved: 2,
            current_repo_boost: 1.3,
            dep_manifest_boost: 1.2,
            lang_match_boost: 1.1,
        }
    }
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            auto_inject: true,
            inject_budget_tokens: 1500,
            inject_min_score: 0.80,
            inject_max_chunks: 4,
            rerank_recency_halflife_days: 90,
            rerank_recency_floor: 0.25,
            max_source_size_mb: 200,
            ignore: Vec::new(),
            multi_source: MultiSourceConfig::default(),
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
    #[serde(default)]
    provides: Vec<String>,
    #[serde(default)]
    include_for: Vec<String>,
    #[serde(default)]
    exclude_for: Vec<String>,
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
    #[serde(default)]
    multi_source: Option<RawMultiSource>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawMultiSource {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    max_sources_queried: Option<u32>,
    #[serde(default)]
    per_source_cap: Option<u32>,
    #[serde(default)]
    current_repo_reserved: Option<u32>,
    #[serde(default)]
    current_repo_boost: Option<f32>,
    #[serde(default)]
    dep_manifest_boost: Option<f32>,
    #[serde(default)]
    lang_match_boost: Option<f32>,
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
        // Defense-in-depth (#135): single source of truth for source-name
        // validity lives in `crate::cli::validate_source_name`. The CLI
        // gates `--name` at parse time and `run_add` re-checks; this is the
        // last line of defense for any caller bypassing both (e.g. a
        // future programmatic config writer or a hand-edited entry round-
        // tripping through `from_str`).
        crate::cli::validate_source_name(entry.name.trim())
            .map_err(|e| ConfigError::Invalid(format!("source name invalid: {e}")))?;
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
    if !entry.provides.is_empty() {
        out.push_str(&format!("provides = {}\n", tq_array(&entry.provides)));
    }
    if !entry.include_for.is_empty() {
        out.push_str(&format!("include_for = {}\n", tq_array(&entry.include_for)));
    }
    if !entry.exclude_for.is_empty() {
        out.push_str(&format!("exclude_for = {}\n", tq_array(&entry.exclude_for)));
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
                provides: rs.provides,
                include_for: rs.include_for,
                exclude_for: rs.exclude_for,
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
        max_source_size_mb: raw
            .max_source_size_mb
            .unwrap_or(defaults.max_source_size_mb),
        ignore: raw.ignore,
        multi_source: build_multi_source(raw.multi_source.unwrap_or_default()),
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

    fn validate_boost(name: &str, v: f32) -> Result<(), ConfigError> {
        if !v.is_finite() || v < 0.0 {
            return Err(ConfigError::Invalid(format!(
                "{name} must be a finite non-negative number, got {v}"
            )));
        }
        Ok(())
    }
    validate_boost("current_repo_boost", ctx.multi_source.current_repo_boost)?;
    validate_boost("dep_manifest_boost", ctx.multi_source.dep_manifest_boost)?;
    validate_boost("lang_match_boost", ctx.multi_source.lang_match_boost)?;

    Ok(ctx)
}

fn build_multi_source(raw: RawMultiSource) -> MultiSourceConfig {
    let defaults = MultiSourceConfig::default();
    MultiSourceConfig {
        enabled: raw.enabled.unwrap_or(defaults.enabled),
        max_sources_queried: raw
            .max_sources_queried
            .unwrap_or(defaults.max_sources_queried),
        per_source_cap: raw.per_source_cap.unwrap_or(defaults.per_source_cap),
        current_repo_reserved: raw
            .current_repo_reserved
            .unwrap_or(defaults.current_repo_reserved),
        current_repo_boost: raw
            .current_repo_boost
            .unwrap_or(defaults.current_repo_boost),
        dep_manifest_boost: raw
            .dep_manifest_boost
            .unwrap_or(defaults.dep_manifest_boost),
        lang_match_boost: raw.lang_match_boost.unwrap_or(defaults.lang_match_boost),
    }
}
