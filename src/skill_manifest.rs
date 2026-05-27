//! Skill manifest schema and two-tier loader.
//!
//! A skill manifest is a TOML file describing a review axis (security,
//! performance, correctness, ...) along with its prompts, calibration
//! namespace, severity cap, and capability mode. Manifests are loaded from
//! two directories:
//!
//! 1. **Bundled** — `skills/*.toml` shipped with the quorum binary.
//! 2. **User**   — `~/.quorum/skills/*.toml` provided by the user.
//!
//! On name collision the user manifest wins, mirroring the ast-grep two-tier
//! loader in `src/ast_grep.rs`. Calibration namespace collisions between
//! tiers are a hard reject to prevent accidental (or malicious) score
//! pollution.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::finding::Severity;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// The review axis this skill covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Axis {
    Correctness,
    Security,
    Performance,
    Testing,
    Architecture,
    Readability,
    Docs,
    MlOps,
    Scalability,
    Custom,
}

/// The runtime capability required by the skill.
///
/// v1 only supports `Pure` (prompt-only, no tool use). The remaining
/// variants are reserved for future issues and rejected at load time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityMode {
    Pure,
    Indexed,
    Toolful,
    BinaryAnalyzer,
    BinaryToolServer,
}

/// Trust tier assigned at load time based on where the manifest was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustTier {
    Bundled,
    User,
    Untrusted,
}

// ---------------------------------------------------------------------------
// Manifest schema
// ---------------------------------------------------------------------------

/// Provider-specific prompt override block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderPrompt {
    #[serde(rename = "override")]
    pub override_prompt: Option<String>,
}

/// The `[prompts]` table in a skill manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompts {
    pub primary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic: Option<ProviderPrompt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai: Option<ProviderPrompt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub google: Option<ProviderPrompt>,
}

/// A single checklist item embedded in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecklistItem {
    pub id: String,
    pub prompt: String,
}

/// The `[capability]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub mode: CapabilityMode,
}

/// The full TOML-deserializable skill manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillManifest {
    pub name: String,
    pub version: String,
    pub display_name: String,
    pub description: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_models: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration_namespace: Option<String>,
    pub axis: Axis,
    pub max_severity: Severity,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_findings: Option<u32>,

    pub capability: Capability,
    pub prompts: Prompts,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checklist: Vec<ChecklistItem>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ast_rules: Vec<String>,
}

impl SkillManifest {
    /// The effective calibration namespace: explicit `calibration_namespace`
    /// if set, otherwise falls back to `name`.
    pub fn effective_namespace(&self) -> &str {
        self.calibration_namespace.as_deref().unwrap_or(&self.name)
    }
}

// ---------------------------------------------------------------------------
// Loaded skill (manifest + metadata)
// ---------------------------------------------------------------------------

/// A fully loaded and validated skill manifest with provenance metadata.
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub manifest: SkillManifest,
    pub trust_tier: TrustTier,
    pub source_path: PathBuf,
    pub manifest_sha256: String,
}

// ---------------------------------------------------------------------------
// Canonical hashing
// ---------------------------------------------------------------------------

/// Serialize a `SkillManifest` to a canonical TOML form (sorted keys via
/// `toml::to_string`) and SHA-256 hash it. Two manifests with identical
/// logical content but differing whitespace produce identical hashes because
/// the `toml` crate's `to_string` always produces a deterministic output
/// from the same `Serialize` impl.
fn canonical_sha256(manifest: &SkillManifest) -> anyhow::Result<String> {
    // `toml::to_string` serializes in field-declaration order (which is
    // stable across runs for a given struct layout). This is sufficient for
    // content-identity: two SkillManifest values that are `PartialEq` will
    // produce identical TOML strings and therefore identical hashes.
    let canonical = toml::to_string(manifest)
        .context("failed to serialize manifest to canonical TOML for hashing")?;
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    Ok(hex::encode(digest))
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate a parsed manifest. Returns an error describing the first
/// violation found so the user gets an actionable message.
fn validate_manifest(manifest: &SkillManifest, path: &Path) -> anyhow::Result<()> {
    let ctx = || format!("in skill manifest {}", path.display());

    // Required non-empty fields.
    if manifest.name.trim().is_empty() {
        bail!("missing required field `name` {}", ctx());
    }
    if manifest.version.trim().is_empty() {
        bail!("missing required field `version` {}", ctx());
    }
    if !is_valid_semver(&manifest.version) {
        bail!(
            "field `version` is not valid semver (got {:?}) {}",
            manifest.version,
            ctx()
        );
    }
    if manifest.display_name.trim().is_empty() {
        bail!("missing required field `display_name` {}", ctx());
    }
    if manifest.description.trim().is_empty() {
        bail!("missing required field `description` {}", ctx());
    }
    if manifest.prompts.primary.trim().is_empty() {
        bail!("missing required field `prompts.primary` {}", ctx());
    }

    // Capability mode: only `pure` is supported in v1.
    if manifest.capability.mode != CapabilityMode::Pure {
        bail!(
            "capability mode {:?} is reserved for future use; \
             only `pure` is supported in v1 {}",
            manifest.capability.mode,
            ctx()
        );
    }

    // preferred_model, if set, must be non-empty.
    if let Some(ref model) = manifest.preferred_model
        && model.trim().is_empty()
    {
        bail!("field `preferred_model` is set but empty {}", ctx());
    }

    Ok(())
}

/// Minimal semver validation: `MAJOR.MINOR.PATCH` where each component is a
/// non-negative integer. We intentionally skip pre-release / build-metadata
/// parsing to avoid pulling in the `semver` crate for a validation-only use.
fn is_valid_semver(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.parse::<u64>().is_ok())
}

// ---------------------------------------------------------------------------
// Two-tier loader
// ---------------------------------------------------------------------------

/// Load skill manifests from bundled and user directories.
///
/// Scans `bundled_dir/skills/*.toml` then `user_dir/skills/*.toml`. On name
/// collision the user manifest wins (the bundled entry is replaced). This
/// mirrors the ast-grep two-tier loader pattern.
///
/// Calibration namespace collisions across tiers are a hard error: if a user
/// skill claims a namespace already owned by a bundled skill, the load fails
/// with an actionable message.
///
/// Malformed TOML files are skipped with a `tracing::warn` rather than
/// aborting the entire load.
pub fn load_skills(bundled_dir: &Path, user_dir: &Path) -> anyhow::Result<Vec<LoadedSkill>> {
    let mut skills_by_name: HashMap<String, LoadedSkill> = HashMap::new();
    // Track which calibration namespaces are claimed by bundled skills so we
    // can reject user-tier collisions.
    let mut bundled_namespaces: HashMap<String, String> = HashMap::new(); // ns -> skill name

    // Phase 1: bundled skills.
    load_from_dir(
        bundled_dir,
        TrustTier::Bundled,
        &mut skills_by_name,
        &mut bundled_namespaces,
        None, // no collision check against self
    )?;

    // Phase 2: user skills. Check namespace collisions against bundled.
    load_from_dir(
        user_dir,
        TrustTier::User,
        &mut skills_by_name,
        &mut HashMap::new(), // user namespaces tracked separately
        Some(&bundled_namespaces),
    )?;

    let mut skills: Vec<LoadedSkill> = skills_by_name.into_values().collect();
    skills.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    Ok(skills)
}

/// Maximum skill manifest file size in bytes (256 KiB). Defends against
/// accidental or malicious multi-megabyte files in user-writable dirs.
const MAX_MANIFEST_FILE_BYTES: u64 = 256 * 1024;

/// Read a skill manifest file with symlink and size guards.
///
/// Mirrors the `read_rule_file` pattern from `ast_grep.rs` (#120):
/// on Unix, opens with `O_NOFOLLOW | O_NONBLOCK` so symlinks and FIFOs
/// are rejected atomically at open time (no TOCTOU). On non-Unix
/// platforms, falls back to `symlink_metadata` pre-check.
fn read_manifest_file(path: &Path) -> std::io::Result<String> {
    use std::fs::OpenOptions;
    use std::io::Read;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }

    #[cfg(not(unix))]
    {
        // Fallback: pre-check with symlink_metadata (has a TOCTOU window,
        // but acceptable on platforms that lack O_NOFOLLOW).
        let meta = std::fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "skill manifest path is a symlink",
            ));
        }
    }

    let file = opts.open(path)?;

    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "skill manifest path is not a regular file",
        ));
    }
    if meta.len() > MAX_MANIFEST_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "skill manifest size {} exceeds cap {}",
                meta.len(),
                MAX_MANIFEST_FILE_BYTES
            ),
        ));
    }

    let mut content = String::new();
    file.take(MAX_MANIFEST_FILE_BYTES + 1)
        .read_to_string(&mut content)?;
    Ok(content)
}

/// Scan a single directory for `*.toml` skill manifests.
fn load_from_dir(
    base_dir: &Path,
    tier: TrustTier,
    skills: &mut HashMap<String, LoadedSkill>,
    own_namespaces: &mut HashMap<String, String>,
    bundled_namespaces: Option<&HashMap<String, String>>,
) -> anyhow::Result<()> {
    let dir = base_dir.join("skills");

    // Symlink guard on the skills directory itself — mirrors ast-grep #120.
    // `symlink_metadata` does NOT follow symlinks, unlike `is_dir()`.
    let dir_meta = match std::fs::symlink_metadata(&dir) {
        Ok(m) => m,
        Err(_) => return Ok(()), // not present is fine
    };
    if dir_meta.file_type().is_symlink() {
        tracing::warn!(
            path = %dir.display(),
            "skill_manifest: skipping symlinked skills directory"
        );
        return Ok(());
    }
    if !dir_meta.file_type().is_dir() {
        return Ok(());
    }

    let mut entries: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()).map(|e| e == "toml") == Some(true))
            .collect(),
        Err(e) => {
            tracing::warn!(
                path = %dir.display(),
                error = %e,
                "skill_manifest: failed to read skills directory"
            );
            return Ok(());
        }
    };
    entries.sort();

    for path in entries {
        let toml_str = match read_manifest_file(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "skill_manifest: failed to read skill file; skipping"
                );
                continue;
            }
        };

        let manifest: SkillManifest = match toml::from_str(&toml_str) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "skill_manifest: malformed TOML; skipping"
                );
                continue;
            }
        };

        if let Err(e) = validate_manifest(&manifest, &path) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "skill_manifest: validation failed; skipping"
            );
            continue;
        }

        let ns = manifest.effective_namespace().to_owned();

        // Namespace collision: user skill must not claim a bundled namespace.
        if let Some(bundled_ns) = bundled_namespaces
            && let Some(bundled_skill) = bundled_ns.get(&ns)
        {
            bail!(
                "user skill {:?} (at {}) uses calibration namespace {:?} \
                 which is already claimed by bundled skill {:?}; \
                 choose a different `calibration_namespace` to avoid \
                 polluting bundled calibration data",
                manifest.name,
                path.display(),
                ns,
                bundled_skill,
            );
        }

        let sha = canonical_sha256(&manifest)?;

        own_namespaces.insert(ns, manifest.name.clone());

        skills.insert(
            manifest.name.clone(),
            LoadedSkill {
                manifest,
                trust_tier: tier.clone(),
                source_path: path,
                manifest_sha256: sha,
            },
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Helper: write a minimal valid skill TOML and return the parent dir.
    fn write_skill(dir: &Path, filename: &str, toml_content: &str) {
        let skills_dir = dir.join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join(filename), toml_content).unwrap();
    }

    fn minimal_toml(name: &str) -> String {
        format!(
            r#"
name = "{name}"
version = "1.0.0"
display_name = "Test Skill"
description = "A test skill for unit tests."
axis = "security"
max_severity = "critical"

[capability]
mode = "pure"

[prompts]
primary = "Review the code for security issues."
"#
        )
    }

    // ── Loading valid bundled + user skill set, user wins on collision ──

    #[test]
    fn load_bundled_and_user_skills_merged() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(bundled.path(), "security.toml", &minimal_toml("security"));
        write_skill(
            user.path(),
            "performance.toml",
            &minimal_toml("performance"),
        );

        let skills = load_skills(bundled.path(), user.path()).unwrap();
        assert_eq!(skills.len(), 2);
        let names: Vec<&str> = skills.iter().map(|s| s.manifest.name.as_str()).collect();
        assert!(names.contains(&"security"));
        assert!(names.contains(&"performance"));
    }

    #[test]
    fn user_skill_overrides_bundled_on_name_collision() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        // Bundled skill with namespace "alpha".
        let bundled_toml = r#"
name = "security"
version = "1.0.0"
display_name = "Bundled Security"
description = "Bundled version."
axis = "security"
max_severity = "high"
calibration_namespace = "alpha"

[capability]
mode = "pure"

[prompts]
primary = "Bundled prompt."
"#;
        // User skill with SAME name but DIFFERENT namespace to avoid collision.
        let user_toml = r#"
name = "security"
version = "2.0.0"
display_name = "User Security"
description = "User version."
axis = "security"
max_severity = "critical"
calibration_namespace = "user-security"

[capability]
mode = "pure"

[prompts]
primary = "User prompt."
"#;
        write_skill(bundled.path(), "security.toml", bundled_toml);
        write_skill(user.path(), "security.toml", user_toml);

        let skills = load_skills(bundled.path(), user.path()).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].manifest.display_name, "User Security");
        assert_eq!(skills[0].manifest.version, "2.0.0");
        assert_eq!(skills[0].trust_tier, TrustTier::User);
    }

    // ── Missing required field fails with actionable error ──

    #[test]
    fn missing_name_fails() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        let toml = r#"
version = "1.0.0"
display_name = "Test"
description = "Desc."
axis = "security"
max_severity = "high"

[capability]
mode = "pure"

[prompts]
primary = "prompt"
"#;
        write_skill(bundled.path(), "bad.toml", toml);
        let skills = load_skills(bundled.path(), user.path()).unwrap();
        // Malformed/invalid manifests are skipped with a warning, not hard errors.
        assert!(skills.is_empty());
    }

    #[test]
    fn missing_prompts_primary_fails() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        let toml = r#"
name = "test"
version = "1.0.0"
display_name = "Test"
description = "Desc."
axis = "security"
max_severity = "high"

[capability]
mode = "pure"

[prompts]
primary = ""
"#;
        write_skill(bundled.path(), "bad.toml", toml);
        let skills = load_skills(bundled.path(), user.path()).unwrap();
        assert!(skills.is_empty());
    }

    // ── Unknown axis value fails ──

    #[test]
    fn unknown_axis_value_fails_parse() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        let toml = r#"
name = "test"
version = "1.0.0"
display_name = "Test"
description = "Desc."
axis = "telepathy"
max_severity = "high"

[capability]
mode = "pure"

[prompts]
primary = "prompt"
"#;
        write_skill(bundled.path(), "bad.toml", toml);
        let skills = load_skills(bundled.path(), user.path()).unwrap();
        assert!(
            skills.is_empty(),
            "unknown axis should fail deserialization"
        );
    }

    // ── Non-pure capability mode fails ──

    #[test]
    fn non_pure_capability_mode_rejected() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        let toml = r#"
name = "test"
version = "1.0.0"
display_name = "Test"
description = "Desc."
axis = "security"
max_severity = "high"

[capability]
mode = "indexed"

[prompts]
primary = "prompt"
"#;
        write_skill(bundled.path(), "bad.toml", toml);
        let skills = load_skills(bundled.path(), user.path()).unwrap();
        assert!(
            skills.is_empty(),
            "non-pure capability mode should be rejected as reserved"
        );
    }

    // ── AST rules field parses correctly ──

    #[test]
    fn ast_rules_field_parses() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        let toml = r#"
name = "security"
version = "1.0.0"
display_name = "Security"
description = "Desc."
axis = "security"
max_severity = "critical"
ast_rules = ["sql-template-injection", "eval-non-literal"]

[capability]
mode = "pure"

[prompts]
primary = "prompt"
"#;
        write_skill(bundled.path(), "security.toml", toml);
        let skills = load_skills(bundled.path(), user.path()).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(
            skills[0].manifest.ast_rules,
            vec!["sql-template-injection", "eval-non-literal"]
        );
    }

    // ── Calibration namespace collision: user skill using bundled namespace ──

    #[test]
    fn namespace_collision_hard_reject() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        // Bundled skill claims namespace "security" (via default = name).
        write_skill(bundled.path(), "security.toml", &minimal_toml("security"));
        // User skill with a DIFFERENT name but same effective namespace.
        let user_toml = r#"
name = "my-security"
version = "1.0.0"
display_name = "My Security"
description = "Desc."
axis = "security"
max_severity = "high"
calibration_namespace = "security"

[capability]
mode = "pure"

[prompts]
primary = "prompt"
"#;
        write_skill(user.path(), "my-security.toml", user_toml);
        let result = load_skills(bundled.path(), user.path());
        assert!(result.is_err(), "namespace collision must be a hard error");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("calibration namespace"),
            "error should mention calibration namespace: {err_msg}"
        );
        assert!(
            err_msg.contains("security"),
            "error should mention the colliding namespace: {err_msg}"
        );
    }

    // ── Manifest canonicalization: identical content, different whitespace ──

    #[test]
    fn canonical_sha256_identical_for_equivalent_toml() {
        let toml_compact = r#"name = "sec"
version = "1.0.0"
display_name = "Sec"
description = "D"
axis = "security"
max_severity = "critical"

[capability]
mode = "pure"

[prompts]
primary = "p"
"#;
        let toml_spacey = r#"
name    =   "sec"
version   =    "1.0.0"
display_name    =    "Sec"
description   =    "D"
axis   =   "security"
max_severity   =   "critical"

[capability]
mode   =   "pure"

[prompts]
primary   =   "p"
"#;
        let m1: SkillManifest = toml::from_str(toml_compact).unwrap();
        let m2: SkillManifest = toml::from_str(toml_spacey).unwrap();
        assert_eq!(m1, m2, "parsed manifests should be equal");

        let h1 = canonical_sha256(&m1).unwrap();
        let h2 = canonical_sha256(&m2).unwrap();
        assert_eq!(
            h1, h2,
            "canonical SHA-256 must be identical for equivalent TOML"
        );
        assert_eq!(h1.len(), 64, "SHA-256 hex digest is 64 chars");
    }

    // ── Empty skills directories produce empty vec ──

    #[test]
    fn empty_dirs_produce_empty_vec() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();
        let skills = load_skills(bundled.path(), user.path()).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn nonexistent_dirs_produce_empty_vec() {
        let skills = load_skills(Path::new("/nonexistent/a"), Path::new("/nonexistent/b")).unwrap();
        assert!(skills.is_empty());
    }

    // ── Malformed TOML: skip with warning, don't abort ──

    #[test]
    fn malformed_toml_skipped_good_skill_loaded() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(bundled.path(), "bad.toml", "this is not {{ valid toml");
        write_skill(bundled.path(), "good.toml", &minimal_toml("good-skill"));

        let skills = load_skills(bundled.path(), user.path()).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].manifest.name, "good-skill");
    }

    // ── Full schema round-trip ──

    #[test]
    fn full_schema_round_trip() {
        let toml_str = r#"
name = "security"
version = "1.0.0"
display_name = "Security"
description = "Comprehensive security review."
preferred_model = "claude-opus-4-7"
fallback_models = ["gpt-5.4"]
calibration_namespace = "security"
axis = "security"
max_severity = "critical"
target_findings = 10
ast_rules = ["sql-template-injection", "eval-non-literal"]

[capability]
mode = "pure"

[prompts]
primary = "Review the code for security issues."

[prompts.anthropic]
override = "Anthropic-specific prompt."

[prompts.google]
override = "Google-specific prompt."

[[checklist]]
id = "input-validation"
prompt = "Are all external inputs validated before use?"

[[checklist]]
id = "auth-check"
prompt = "Are authorization checks in place?"
"#;
        let manifest: SkillManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.name, "security");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.preferred_model.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(
            manifest.fallback_models.as_deref(),
            Some(&["gpt-5.4".to_string()][..])
        );
        assert_eq!(manifest.calibration_namespace.as_deref(), Some("security"));
        assert_eq!(manifest.axis, Axis::Security);
        assert_eq!(manifest.max_severity, Severity::Critical);
        assert_eq!(manifest.target_findings, Some(10));
        assert_eq!(manifest.capability.mode, CapabilityMode::Pure);
        assert_eq!(manifest.checklist.len(), 2);
        assert_eq!(manifest.checklist[0].id, "input-validation");
        assert_eq!(manifest.ast_rules.len(), 2);
        assert_eq!(
            manifest
                .prompts
                .anthropic
                .as_ref()
                .unwrap()
                .override_prompt
                .as_deref(),
            Some("Anthropic-specific prompt.")
        );
        assert!(manifest.prompts.openai.is_none());

        // Validate passes.
        validate_manifest(&manifest, Path::new("test.toml")).unwrap();
    }

    // ── effective_namespace defaults to name ──

    #[test]
    fn effective_namespace_defaults_to_name() {
        let toml_str = &minimal_toml("my-skill");
        let manifest: SkillManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.effective_namespace(), "my-skill");
    }

    #[test]
    fn effective_namespace_uses_explicit_when_set() {
        let toml_str = r#"
name = "my-skill"
version = "1.0.0"
display_name = "My Skill"
description = "Desc."
calibration_namespace = "custom-ns"
axis = "security"
max_severity = "high"

[capability]
mode = "pure"

[prompts]
primary = "prompt"
"#;
        let manifest: SkillManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.effective_namespace(), "custom-ns");
    }

    // ── Trust tier assignment ──

    #[test]
    fn trust_tier_assigned_correctly() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(
            bundled.path(),
            "bundled.toml",
            &minimal_toml("bundled-skill"),
        );
        let user_toml = r#"
name = "user-skill"
version = "1.0.0"
display_name = "User Skill"
description = "Desc."
axis = "performance"
max_severity = "medium"
calibration_namespace = "user-perf"

[capability]
mode = "pure"

[prompts]
primary = "prompt"
"#;
        write_skill(user.path(), "user.toml", user_toml);

        let skills = load_skills(bundled.path(), user.path()).unwrap();
        let bundled_skill = skills.iter().find(|s| s.manifest.name == "bundled-skill");
        let user_skill = skills.iter().find(|s| s.manifest.name == "user-skill");
        assert_eq!(bundled_skill.unwrap().trust_tier, TrustTier::Bundled);
        assert_eq!(user_skill.unwrap().trust_tier, TrustTier::User);
    }

    // ── Version validation ──

    #[test]
    fn invalid_version_rejected() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        let toml = r#"
name = "test"
version = "not-semver"
display_name = "Test"
description = "Desc."
axis = "security"
max_severity = "high"

[capability]
mode = "pure"

[prompts]
primary = "prompt"
"#;
        write_skill(bundled.path(), "bad.toml", toml);
        let skills = load_skills(bundled.path(), user.path()).unwrap();
        assert!(skills.is_empty(), "invalid semver should be rejected");
    }

    // ── Semver validation unit tests ──

    #[test]
    fn semver_validation() {
        assert!(is_valid_semver("1.0.0"));
        assert!(is_valid_semver("0.1.0"));
        assert!(is_valid_semver("123.456.789"));
        assert!(!is_valid_semver("1.0"));
        assert!(!is_valid_semver("1.0.0.0"));
        assert!(!is_valid_semver("not-semver"));
        assert!(!is_valid_semver(""));
        assert!(!is_valid_semver("1..0"));
        assert!(!is_valid_semver(".1.0"));
    }

    // ── Deterministic ordering ──

    #[test]
    fn skills_returned_in_sorted_order() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(bundled.path(), "zzz.toml", &minimal_toml("zzz"));

        let toml_aaa = r#"
name = "aaa"
version = "1.0.0"
display_name = "AAA"
description = "Desc."
axis = "performance"
max_severity = "low"
calibration_namespace = "aaa-ns"

[capability]
mode = "pure"

[prompts]
primary = "prompt"
"#;
        write_skill(bundled.path(), "aaa.toml", toml_aaa);

        let skills = load_skills(bundled.path(), user.path()).unwrap();
        let names: Vec<&str> = skills.iter().map(|s| s.manifest.name.as_str()).collect();
        assert_eq!(names, vec!["aaa", "zzz"]);
    }

    // ── sha256 is populated ──

    #[test]
    fn loaded_skill_has_sha256() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(bundled.path(), "test.toml", &minimal_toml("test"));
        let skills = load_skills(bundled.path(), user.path()).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(
            skills[0].manifest_sha256.len(),
            64,
            "SHA-256 hex digest should be 64 chars"
        );
    }

    // ── All axis variants parse ──

    #[test]
    fn all_axis_variants_deserialize() {
        let axes = [
            "correctness",
            "security",
            "performance",
            "testing",
            "architecture",
            "readability",
            "docs",
            "ml-ops",
            "scalability",
            "custom",
        ];
        for axis_str in axes {
            let toml_str = format!(
                r#"
name = "test-{axis_str}"
version = "1.0.0"
display_name = "Test"
description = "Desc."
axis = "{axis_str}"
max_severity = "info"

[capability]
mode = "pure"

[prompts]
primary = "prompt"
"#
            );
            let result: Result<SkillManifest, _> = toml::from_str(&toml_str);
            assert!(
                result.is_ok(),
                "axis variant {axis_str:?} should deserialize: {:?}",
                result.err()
            );
        }
    }

    // ── All severity variants work as max_severity ──

    #[test]
    fn all_severity_variants_deserialize_as_max_severity() {
        for sev in ["critical", "high", "medium", "low", "info"] {
            let toml_str = format!(
                r#"
name = "test-{sev}"
version = "1.0.0"
display_name = "Test"
description = "Desc."
axis = "security"
max_severity = "{sev}"

[capability]
mode = "pure"

[prompts]
primary = "prompt"
"#
            );
            let result: Result<SkillManifest, _> = toml::from_str(&toml_str);
            assert!(
                result.is_ok(),
                "severity {sev:?} should deserialize: {:?}",
                result.err()
            );
        }
    }

    // ── Capability mode variants ──

    #[test]
    fn all_capability_modes_deserialize() {
        let modes = [
            "pure",
            "indexed",
            "toolful",
            "binary-analyzer",
            "binary-tool-server",
        ];
        for mode in modes {
            let toml_str = format!(
                r#"
name = "test"
version = "1.0.0"
display_name = "Test"
description = "Desc."
axis = "security"
max_severity = "info"

[capability]
mode = "{mode}"

[prompts]
primary = "prompt"
"#
            );
            let result: Result<SkillManifest, _> = toml::from_str(&toml_str);
            assert!(
                result.is_ok(),
                "capability mode {mode:?} should deserialize: {:?}",
                result.err()
            );
        }
    }

    // ── Empty preferred_model rejected ──

    #[test]
    fn empty_preferred_model_rejected() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        let toml = r#"
name = "test"
version = "1.0.0"
display_name = "Test"
description = "Desc."
axis = "security"
max_severity = "high"
preferred_model = ""

[capability]
mode = "pure"

[prompts]
primary = "prompt"
"#;
        write_skill(bundled.path(), "bad.toml", toml);
        let skills = load_skills(bundled.path(), user.path()).unwrap();
        assert!(
            skills.is_empty(),
            "empty preferred_model should be rejected"
        );
    }

    // ── Source path is recorded ──

    #[test]
    fn source_path_recorded() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(bundled.path(), "test.toml", &minimal_toml("test"));
        let skills = load_skills(bundled.path(), user.path()).unwrap();
        assert!(
            skills[0].source_path.ends_with("skills/test.toml"),
            "source_path should end with skills/test.toml: {:?}",
            skills[0].source_path
        );
    }
}
