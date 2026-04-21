//! Source-level extraction dispatch.
//!
//! Walks a [`SourceEntry`]'s filesystem tree, applies tiered ignore globs,
//! routes each file to the appropriate language extractor, and collects both
//! the produced [`Chunk`]s and a [`Diagnostics`] summary. One bad file does
//! not abort the run; extractor errors are collected per-file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use glob::Pattern;
use walkdir::WalkDir;

use super::astgrep_hcl::extract_hcl;
use super::astgrep_py::extract_python;
use super::astgrep_rust::extract_rust;
use super::astgrep_ts::extract_typescript;
use super::markdown::{split_markdown, DocSubtype};
use crate::context::config::{SourceEntry, SourceLocation};
use crate::context::types::Chunk;

/// Placeholder commit SHA for `SourceLocation::Path` sources. Phase 3 will
/// wire a GitOps trait to resolve an actual SHA at extract time.
const UNVERSIONED_SHA: &str = "unversioned";

/// Maximum file size and tier preferences for extraction.
#[derive(Debug, Clone)]
pub struct ExtractConfig {
    /// Hard cap — files larger than this are skipped and logged.
    pub max_file_size_bytes: u64,
    /// Global ignore globs applied on top of per-source ignore.
    pub global_ignore: Vec<String>,
}

impl Default for ExtractConfig {
    fn default() -> Self {
        Self {
            max_file_size_bytes: 1024 * 1024,
            global_ignore: vec![
                "node_modules/".into(),
                "target/".into(),
                ".git/".into(),
                "dist/".into(),
                "build/".into(),
                ".venv/".into(),
                "__pycache__/".into(),
            ],
        }
    }
}

#[derive(Default, Debug, Clone)]
pub struct Diagnostics {
    pub total_files_scanned: usize,
    pub extracted_files: usize,
    pub ignored_count: usize,
    pub skipped_by_tier: SkipTiers,
    pub size_skipped: usize,
    pub unknown_extension_skipped: usize,
    pub extractor_errors: Vec<ExtractorError>,
    pub top_skipped_globs: Vec<(String, usize)>,
}

#[derive(Default, Debug, Clone)]
pub struct SkipTiers {
    pub per_source: usize,
    pub global: usize,
    pub gitignore: usize,
}

#[derive(Debug, Clone)]
pub struct ExtractorError {
    pub file_path: String,
    pub error: String,
}

/// Clock abstraction for tests.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

pub struct FixedClock(pub DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

impl FixedClock {
    pub fn epoch() -> Self {
        Self(DateTime::<Utc>::from_timestamp(0, 0).unwrap())
    }
}

#[derive(Debug)]
pub struct ExtractResult {
    pub chunks: Vec<Chunk>,
    pub diagnostics: Diagnostics,
}

/// Walk a source's filesystem tree, apply ignore tiers, dispatch per-extension,
/// and collect chunks + diagnostics.
pub fn extract_source(
    source: &SourceEntry,
    config: &ExtractConfig,
    clock: &dyn Clock,
) -> anyhow::Result<ExtractResult> {
    let root = match &source.location {
        SourceLocation::Path(p) => p.clone(),
        SourceLocation::Git { .. } => {
            anyhow::bail!("git sources not yet supported (MVP handles Path sources only)");
        }
    };

    if !root.exists() {
        anyhow::bail!(
            "source '{}': root path does not exist: {}",
            source.name,
            root.display()
        );
    }

    let per_source_patterns = compile_patterns(&source.ignore);
    let global_patterns = compile_patterns(&config.global_ignore);
    // MVP: .gitignore is not yet honored — Phase 3 will parse and apply it.

    let scan_roots: Vec<PathBuf> = if source.paths.is_empty() {
        vec![root.clone()]
    } else {
        source.paths.iter().map(|p| root.join(p)).collect()
    };

    let indexed_at = clock.now();
    let mut diagnostics = Diagnostics::default();
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut per_source_hit_counts: HashMap<String, usize> = HashMap::new();

    for scan_root in &scan_roots {
        if !scan_root.exists() {
            tracing::warn!(
                source = %source.name,
                path = %scan_root.display(),
                "scan path does not exist; skipping"
            );
            continue;
        }

        for entry in WalkDir::new(scan_root).follow_links(false) {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(
                        source = %source.name,
                        error = %e,
                        "walkdir error; skipping entry"
                    );
                    continue;
                }
            };

            if !entry.file_type().is_file() {
                continue;
            }

            let file_path = entry.path();
            let rel = match relative_forward_slash(&root, file_path) {
                Some(r) => r,
                None => {
                    // File lies outside the root (shouldn't happen). Skip.
                    continue;
                }
            };

            diagnostics.total_files_scanned += 1;

            // Tier 1: per-source ignore.
            if let Some(pat) = first_match(&per_source_patterns, &rel) {
                diagnostics.skipped_by_tier.per_source += 1;
                diagnostics.ignored_count += 1;
                *per_source_hit_counts.entry(pat.to_string()).or_insert(0) += 1;
                continue;
            }

            // Tier 2: global ignore.
            if first_match(&global_patterns, &rel).is_some() {
                diagnostics.skipped_by_tier.global += 1;
                diagnostics.ignored_count += 1;
                continue;
            }

            // Tier 3: .gitignore (not yet honored — MVP).

            // Size cap.
            let md = match entry.metadata() {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(
                        source = %source.name,
                        path = %file_path.display(),
                        error = %e,
                        "failed to stat file; skipping"
                    );
                    continue;
                }
            };
            if md.len() > config.max_file_size_bytes {
                tracing::warn!(
                    source = %source.name,
                    path = %file_path.display(),
                    size = md.len(),
                    cap = config.max_file_size_bytes,
                    "file exceeds size cap; skipping"
                );
                diagnostics.size_skipped += 1;
                continue;
            }

            // Dispatch.
            let kind = classify(file_path);
            let dispatched = match kind {
                FileKind::Unknown => {
                    diagnostics.unknown_extension_skipped += 1;
                    continue;
                }
                _ => kind,
            };

            let src_text = match std::fs::read_to_string(file_path) {
                Ok(t) => t,
                Err(e) => {
                    diagnostics.extractor_errors.push(ExtractorError {
                        file_path: rel.clone(),
                        error: format!("read error: {e}"),
                    });
                    continue;
                }
            };

            let result: anyhow::Result<Vec<Chunk>> = match dispatched {
                FileKind::Rust => extract_rust(
                    &src_text,
                    &rel,
                    &source.name,
                    UNVERSIONED_SHA,
                    indexed_at,
                ),
                FileKind::Typescript => extract_typescript(
                    &src_text,
                    &rel,
                    &source.name,
                    UNVERSIONED_SHA,
                    indexed_at,
                ),
                FileKind::Python => extract_python(
                    &src_text,
                    &rel,
                    &source.name,
                    UNVERSIONED_SHA,
                    indexed_at,
                ),
                FileKind::Hcl => extract_hcl(
                    &src_text,
                    &rel,
                    &source.name,
                    UNVERSIONED_SHA,
                    indexed_at,
                ),
                FileKind::Markdown => {
                    let subtype = classify_markdown(&rel);
                    Ok(split_markdown(
                        &src_text,
                        &rel,
                        &source.name,
                        subtype,
                        UNVERSIONED_SHA,
                        indexed_at,
                    ))
                }
                FileKind::Unknown => unreachable!(),
            };

            match result {
                Ok(mut produced) => {
                    diagnostics.extracted_files += 1;
                    chunks.append(&mut produced);
                }
                Err(e) => {
                    diagnostics.extractor_errors.push(ExtractorError {
                        file_path: rel.clone(),
                        error: format!("{e}"),
                    });
                }
            }
        }
    }

    // Top-5 per-source globs by hit count.
    let mut glob_vec: Vec<(String, usize)> = per_source_hit_counts.into_iter().collect();
    glob_vec.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    glob_vec.truncate(5);
    diagnostics.top_skipped_globs = glob_vec;

    Ok(ExtractResult {
        chunks,
        diagnostics,
    })
}

// --- internals --------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum FileKind {
    Rust,
    Typescript,
    Python,
    Hcl,
    Markdown,
    Unknown,
}

fn classify(path: &Path) -> FileKind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("rs") => FileKind::Rust,
        Some("ts" | "tsx" | "js" | "mjs" | "cjs" | "jsx") => FileKind::Typescript,
        Some("py") => FileKind::Python,
        Some("tf" | "tfvars") => FileKind::Hcl,
        Some("md" | "markdown") => FileKind::Markdown,
        _ => FileKind::Unknown,
    }
}

fn classify_markdown(rel_path: &str) -> DocSubtype {
    let lower = rel_path.to_ascii_lowercase();
    let file_name = Path::new(&lower)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    if stem.contains("readme") {
        DocSubtype::Readme
    } else if stem.contains("changelog") || stem.contains("history") {
        DocSubtype::Changelog
    } else if lower.contains("docs/adr/")
        || stem.contains("adr")
        || stem.contains("decision")
        || stem.contains("decisions")
    {
        DocSubtype::Adr
    } else {
        DocSubtype::Doc
    }
}

/// Compile ignore globs. Patterns ending in `/` are normalized to match any
/// file under that directory anywhere in the tree (e.g. `node_modules/` →
/// `**/node_modules/**`). A bare name with no glob char gets the same
/// treatment. Invalid patterns are logged and skipped.
fn compile_patterns(raw: &[String]) -> Vec<(String, Pattern)> {
    let mut out = Vec::with_capacity(raw.len());
    for p in raw {
        let normalized = normalize_pattern(p);
        match Pattern::new(&normalized) {
            Ok(pat) => out.push((p.clone(), pat)),
            Err(e) => {
                tracing::warn!(pattern = %p, error = %e, "invalid ignore glob; skipping");
            }
        }
    }
    out
}

fn normalize_pattern(p: &str) -> String {
    if p.ends_with('/') {
        // Directory prefix: match any path under it anywhere.
        let trimmed = p.trim_end_matches('/');
        format!("**/{trimmed}/**")
    } else {
        p.to_string()
    }
}

/// Test `rel` (forward-slash relative path) against each pattern, returning the
/// originating raw pattern string on the first match. Also matches when the
/// relative path itself starts with the prefix (handles both rooted `docs/**`
/// and nested `*/docs/**` shapes).
fn first_match<'a>(patterns: &'a [(String, Pattern)], rel: &str) -> Option<&'a str> {
    for (raw, pat) in patterns {
        if pat.matches(rel) {
            return Some(raw);
        }
        // Also try matching without a leading `**/` prefix so that
        // `**/docs/**` matches `docs/adr/001.md` at the root.
        if pat.as_str().starts_with("**/") {
            if let Ok(inner) = Pattern::new(&pat.as_str()[3..]) {
                if inner.matches(rel) {
                    return Some(raw);
                }
            }
        }
    }
    None
}

fn relative_forward_slash(root: &Path, file: &Path) -> Option<String> {
    let stripped = file.strip_prefix(root).ok()?;
    let mut out = String::new();
    for (i, comp) in stripped.components().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(comp.as_os_str().to_str()?);
    }
    Some(out)
}
