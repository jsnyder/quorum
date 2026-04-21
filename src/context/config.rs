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

            let location = match (rs.git.as_deref(), rs.path.as_deref()) {
                (Some(url), None) => SourceLocation::Git {
                    url: url.to_string(),
                    rev: rs.rev.clone(),
                },
                (None, Some(p)) => SourceLocation::Path(PathBuf::from(p)),
                (Some(_), Some(_)) | (None, None) => {
                    return Err(ConfigError::Invalid(format!(
                        "source '{}': must specify exactly one of 'git' or 'path'",
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

    if !(0.0..=1.0).contains(&ctx.inject_min_score) {
        return Err(ConfigError::Invalid(format!(
            "inject_min_score must be in [0.0, 1.0], got {}",
            ctx.inject_min_score
        )));
    }
    if !(0.0..=1.0).contains(&ctx.rerank_recency_floor) {
        return Err(ConfigError::Invalid(format!(
            "rerank_recency_floor must be in [0.0, 1.0], got {}",
            ctx.rerank_recency_floor
        )));
    }

    Ok(ctx)
}
