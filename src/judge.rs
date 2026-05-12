use crate::ast_grep::RuleMetadata;
#[cfg(test)]
use crate::finding::PrecisionTier;
use crate::finding::{Finding, JudgeRequirement, JudgeVerdict};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

const CACHE_TTL_DAYS: i64 = 7;

/// Compute a deterministic cache key from a rule ID and its evidence snippet.
/// The null byte separator prevents prefix collisions between rule and evidence.
pub fn verdict_cache_key(rule_id: &str, evidence: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(rule_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(evidence.as_bytes());
    let hash = hasher.finalize();
    let mut hex = String::with_capacity(hash.len() * 2);
    for byte in hash {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub cache_key: String,
    pub rule_id: String,
    pub verdict: JudgeVerdict,
    pub confidence: f32,
    pub reason: String,
    pub timestamp: DateTime<Utc>,
}

/// Load the verdict cache from a JSONL file, discarding entries older than
/// `CACHE_TTL_DAYS`. Returns an empty map if the file does not exist.
pub fn load_cache(path: &Path) -> std::io::Result<HashMap<String, CacheEntry>> {
    let mut map = HashMap::new();
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(map),
        Err(e) => return Err(e),
    };
    let cutoff = Utc::now() - chrono::Duration::days(CACHE_TTL_DAYS);
    for line in content.lines() {
        if let Ok(entry) = serde_json::from_str::<CacheEntry>(line)
            && entry.timestamp > cutoff
        {
            map.insert(entry.cache_key.clone(), entry);
        }
    }
    Ok(map)
}

/// Append a single verdict cache entry to the JSONL file, creating parent
/// directories as needed.
pub fn write_cache_entry(path: &Path, entry: &CacheEntry) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let json = serde_json::to_string(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writeln!(file, "{json}")
}

// ---------------------------------------------------------------------------
// LLM judge client
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct JudgeResponseItem {
    pub rule_id: String,
    pub verdict: String,
    pub confidence: f32,
    pub reason: String,
}

/// Map wire-format verdict strings to the `JudgeVerdict` enum.
pub fn parse_verdict(s: &str) -> JudgeVerdict {
    match s {
        "tp" => JudgeVerdict::Approved,
        "fp" => JudgeVerdict::Rejected,
        "uncertain" => JudgeVerdict::Uncertain,
        _ => JudgeVerdict::Uncertain,
    }
}

pub trait JudgeLlm: Send + Sync {
    async fn call(&self, prompt: &str) -> Option<String>;
}

const JUDGE_SYSTEM_PROMPT: &str =
    "You are a code review judge. Respond with ONLY a JSON array, no other text.";

pub struct OpenAiJudge {
    client: std::sync::Arc<crate::llm_client::OpenAiClient>,
    model: String,
}

impl OpenAiJudge {
    pub fn new(client: std::sync::Arc<crate::llm_client::OpenAiClient>, model: String) -> Self {
        Self { client, model }
    }
}

impl JudgeLlm for OpenAiJudge {
    async fn call(&self, prompt: &str) -> Option<String> {
        match self
            .client
            .judge_completion(&self.model, prompt, JUDGE_SYSTEM_PROMPT)
            .await
        {
            Ok(response) => Some(response.content),
            Err(e) => {
                tracing::warn!(model = %self.model, error = %e, "judge LLM call failed");
                None
            }
        }
    }
}

/// Build the LLM prompt for judging a batch of AST-detected findings.
///
/// Each finding tuple is `(rule_id, title, line_start, line_end, evidence)`.
pub fn build_judge_prompt(
    source_code: &str,
    findings: &[(String, String, u32, u32, String)],
) -> String {
    let mut prompt = String::from(
        "You are a code review judge. For each AST-detected finding below, \
         determine if it is a true positive (tp), false positive (fp), or \
         uncertain based on the surrounding code context.\n\n",
    );
    prompt.push_str("Source code:\n```\n");
    prompt.push_str(source_code);
    prompt.push_str("\n```\n\nFindings to judge:\n");

    for (i, (rule_id, title, start, end, evidence)) in findings.iter().enumerate() {
        prompt.push_str(&format!(
            "{}. rule_id=\"{}\", title=\"{}\", lines {}-{}, evidence=\"{}\"\n",
            i + 1,
            rule_id,
            title,
            start,
            end,
            evidence
        ));
    }

    prompt.push_str(
        "\nRespond with ONLY a JSON array. Each element: \
         {\"rule_id\": \"...\", \"verdict\": \"tp\"|\"fp\"|\"uncertain\", \
         \"confidence\": 0.0-1.0, \"reason\": \"...\"}\n",
    );
    prompt
}

// ---------------------------------------------------------------------------
// Judge orchestrator
// ---------------------------------------------------------------------------

/// Aggregate counters for a single `judge_findings` invocation.
#[derive(Debug, Default)]
pub struct JudgeResult {
    pub approved: u32,
    pub rejected: u32,
    pub uncertain: u32,
    pub skipped: u32,
    pub cache_hits: u32,
    pub calls: u32,
    pub latency_ms: u64,
}

/// Judge AST findings against their rule metadata.
///
/// - Findings with `judge: Skip` pass through unchanged.
/// - Findings with `judge: Required` that are rejected get dropped.
/// - Findings with `judge: Optional` that are rejected get confidence
///   clamped to 0.05.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn judge_findings(
    findings: &mut Vec<Finding>,
    source_code: &str,
    metadata: &HashMap<String, RuleMetadata>,
    cache: &HashMap<String, CacheEntry>,
    cache_path: &Path,
    llm_call: Option<&dyn Fn(&str) -> Option<String>>,
) -> JudgeResult {
    let start = std::time::Instant::now();
    let mut result = JudgeResult::default();

    // Phase 1: Check cache and categorize
    let mut to_judge: Vec<usize> = Vec::new();
    for (i, f) in findings.iter_mut().enumerate() {
        let meta = f
            .rule_id
            .as_ref()
            .and_then(|rid| metadata.get(rid.as_str()))
            .cloned()
            .unwrap_or_default();

        if meta.judge == JudgeRequirement::Skip {
            f.judge_verdict = Some(JudgeVerdict::Skipped);
            result.skipped += 1;
            continue;
        }

        let evidence = f.evidence.first().map(|s| s.as_str()).unwrap_or("");
        let rule_id = f.rule_id.as_deref().unwrap_or("");
        let key = verdict_cache_key(rule_id, evidence);

        if let Some(cached) = cache.get(&key) {
            f.judge_verdict = Some(cached.verdict.clone());
            f.judge_confidence = Some(cached.confidence);
            result.cache_hits += 1;
            match &cached.verdict {
                JudgeVerdict::Approved => result.approved += 1,
                JudgeVerdict::Rejected => result.rejected += 1,
                JudgeVerdict::Uncertain => result.uncertain += 1,
                JudgeVerdict::Skipped => result.skipped += 1,
            }
            continue;
        }
        to_judge.push(i);
    }

    // Phase 2: Batch LLM call for uncached findings
    if !to_judge.is_empty() {
        if let Some(llm) = llm_call {
            let items: Vec<_> = to_judge
                .iter()
                .map(|&i| {
                    let f = &findings[i];
                    (
                        f.rule_id.clone().unwrap_or_default(),
                        f.title.clone(),
                        f.line_start,
                        f.line_end,
                        f.evidence.first().cloned().unwrap_or_default(),
                    )
                })
                .collect();

            let prompt = build_judge_prompt(source_code, &items);
            result.calls += 1;

            if let Some(response) = llm(&prompt)
                && let Ok(verdicts) = serde_json::from_str::<Vec<JudgeResponseItem>>(&response)
            {
                for v in &verdicts {
                    if let Some(pos) = to_judge
                        .iter()
                        .position(|&i| findings[i].rule_id.as_deref() == Some(&v.rule_id))
                    {
                        let i = to_judge.swap_remove(pos);
                        let verdict = parse_verdict(&v.verdict);
                        let confidence = v.confidence.clamp(0.0, 1.0);
                        findings[i].judge_verdict = Some(verdict.clone());
                        findings[i].judge_confidence = Some(confidence);

                        let evidence = findings[i]
                            .evidence
                            .first()
                            .map(|s| s.as_str())
                            .unwrap_or("");
                        let key = verdict_cache_key(&v.rule_id, evidence);
                        let _ = write_cache_entry(
                            cache_path,
                            &CacheEntry {
                                cache_key: key,
                                rule_id: v.rule_id.clone(),
                                verdict: verdict.clone(),
                                confidence,
                                reason: v.reason.clone(),
                                timestamp: Utc::now(),
                            },
                        );

                        match verdict {
                            JudgeVerdict::Approved => result.approved += 1,
                            JudgeVerdict::Rejected => result.rejected += 1,
                            _ => result.uncertain += 1,
                        }
                    }
                }
            }

            // Any remaining unjudged findings (LLM didn't return verdict): mark uncertain
            for &i in &to_judge {
                if findings[i].judge_verdict.is_none() {
                    findings[i].judge_verdict = Some(JudgeVerdict::Uncertain);
                    result.uncertain += 1;
                }
            }
        } else {
            // No LLM available: mark all as uncertain
            for &i in &to_judge {
                findings[i].judge_verdict = Some(JudgeVerdict::Uncertain);
                result.uncertain += 1;
            }
        }
    }

    // Phase 3: Clamp confidence for Optional+Rejected, then drop Required+Rejected
    for f in findings.iter_mut() {
        let meta = f
            .rule_id
            .as_ref()
            .and_then(|rid| metadata.get(rid.as_str()))
            .cloned()
            .unwrap_or_default();

        if meta.judge == JudgeRequirement::Optional
            && f.judge_verdict == Some(JudgeVerdict::Rejected)
        {
            f.judge_confidence = Some(0.05);
        }
    }

    findings.retain(|f| {
        let meta = f
            .rule_id
            .as_ref()
            .and_then(|rid| metadata.get(rid.as_str()))
            .cloned()
            .unwrap_or_default();

        !(meta.judge == JudgeRequirement::Required
            && f.judge_verdict == Some(JudgeVerdict::Rejected))
    });

    result.latency_ms = start.elapsed().as_millis() as u64;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::{FindingBuilder, Source};

    struct MockJudge {
        response: Option<String>,
    }

    impl JudgeLlm for MockJudge {
        async fn call(&self, _prompt: &str) -> Option<String> {
            self.response.clone()
        }
    }

    #[test]
    fn cache_key_deterministic() {
        let k1 = verdict_cache_key("ast-grep:python/bare-except-pass", "except:\n    pass");
        let k2 = verdict_cache_key("ast-grep:python/bare-except-pass", "except:\n    pass");
        assert_eq!(k1, k2);
    }

    #[test]
    fn cache_key_differs_for_different_evidence() {
        let k1 = verdict_cache_key("rule-a", "code1");
        let k2 = verdict_cache_key("rule-a", "code2");
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_differs_for_different_rules() {
        let k1 = verdict_cache_key("rule-a", "code");
        let k2 = verdict_cache_key("rule-b", "code");
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("judge_cache.jsonl");
        let entry = CacheEntry {
            cache_key: "abc123".into(),
            rule_id: "ast-grep:python/test".into(),
            verdict: JudgeVerdict::Approved,
            confidence: 0.85,
            reason: "looks good".into(),
            timestamp: Utc::now(),
        };
        write_cache_entry(&cache_path, &entry).unwrap();
        let loaded = load_cache(&cache_path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded["abc123"].verdict, JudgeVerdict::Approved);
    }

    #[test]
    fn cache_ttl_expires_old_entries() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("judge_cache.jsonl");
        let old_entry = CacheEntry {
            cache_key: "old".into(),
            rule_id: "rule".into(),
            verdict: JudgeVerdict::Approved,
            confidence: 0.9,
            reason: String::new(),
            timestamp: Utc::now() - chrono::Duration::days(8),
        };
        write_cache_entry(&cache_path, &old_entry).unwrap();
        let loaded = load_cache(&cache_path).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn parse_verdict_mapping() {
        assert_eq!(parse_verdict("tp"), JudgeVerdict::Approved);
        assert_eq!(parse_verdict("fp"), JudgeVerdict::Rejected);
        assert_eq!(parse_verdict("uncertain"), JudgeVerdict::Uncertain);
        assert_eq!(parse_verdict("garbage"), JudgeVerdict::Uncertain);
    }

    #[test]
    fn judge_response_deserializes() {
        let json = r#"[
            {"rule_id": "ast-grep:python/test", "verdict": "tp", "confidence": 0.85, "reason": "valid"},
            {"rule_id": "ast-grep:python/other", "verdict": "fp", "confidence": 0.92, "reason": "safe"}
        ]"#;
        let verdicts: Vec<JudgeResponseItem> = serde_json::from_str(json).unwrap();
        assert_eq!(verdicts.len(), 2);
        assert_eq!(verdicts[0].verdict, "tp");
        assert_eq!(verdicts[1].verdict, "fp");
    }

    #[test]
    fn judge_skips_high_precision_rules() {
        let mut findings = vec![{
            let mut f = FindingBuilder::new()
                .source(Source::Linter("ast-grep".into()))
                .rule_id("ast-grep:typescript/as-any-cast")
                .build();
            f.precision_tier = Some(PrecisionTier::High);
            f
        }];
        let mut metadata = HashMap::new();
        metadata.insert(
            "ast-grep:typescript/as-any-cast".into(),
            RuleMetadata {
                precision: PrecisionTier::High,
                judge: JudgeRequirement::Skip,
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");
        let result = judge_findings(
            &mut findings,
            "code",
            &metadata,
            &HashMap::new(),
            &cache_path,
            None,
        );
        assert_eq!(result.skipped, 1);
        assert_eq!(findings[0].judge_verdict, Some(JudgeVerdict::Skipped));
    }

    #[test]
    fn judge_approves_with_mock_llm() {
        let mut findings = vec![{
            let mut f = FindingBuilder::new()
                .source(Source::Linter("ast-grep".into()))
                .rule_id("ast-grep:python/broad-exception-catch")
                .evidence("except Exception as e:")
                .build();
            f.precision_tier = Some(PrecisionTier::Speculative);
            f
        }];
        let mut metadata = HashMap::new();
        metadata.insert(
            "ast-grep:python/broad-exception-catch".into(),
            RuleMetadata {
                precision: PrecisionTier::Speculative,
                judge: JudgeRequirement::Required,
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");
        let mock_llm = |_prompt: &str| -> Option<String> {
            Some(
                r#"[{"rule_id":"ast-grep:python/broad-exception-catch","verdict":"tp","confidence":0.85,"reason":"valid"}]"#.into(),
            )
        };
        let result = judge_findings(
            &mut findings,
            "source",
            &metadata,
            &HashMap::new(),
            &cache_path,
            Some(&mock_llm),
        );
        assert_eq!(result.approved, 1);
        assert_eq!(result.calls, 1);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].judge_verdict, Some(JudgeVerdict::Approved));
        assert_eq!(findings[0].judge_confidence, Some(0.85));
        // Verify cache was written
        let loaded = load_cache(&cache_path).unwrap();
        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn judge_drops_required_rejected() {
        let mut findings = vec![{
            let mut f = FindingBuilder::new()
                .source(Source::Linter("ast-grep".into()))
                .rule_id("ast-grep:python/broad-exception-catch")
                .evidence("except Exception:")
                .build();
            f.precision_tier = Some(PrecisionTier::Speculative);
            f
        }];
        let mut metadata = HashMap::new();
        metadata.insert(
            "ast-grep:python/broad-exception-catch".into(),
            RuleMetadata {
                precision: PrecisionTier::Speculative,
                judge: JudgeRequirement::Required,
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");
        let mock_llm = |_prompt: &str| -> Option<String> {
            Some(
                r#"[{"rule_id":"ast-grep:python/broad-exception-catch","verdict":"fp","confidence":0.92,"reason":"intentional top-level handler"}]"#.into(),
            )
        };
        let result = judge_findings(
            &mut findings,
            "source",
            &metadata,
            &HashMap::new(),
            &cache_path,
            Some(&mock_llm),
        );
        assert_eq!(result.rejected, 1);
        assert!(
            findings.is_empty(),
            "Required+Rejected finding should be dropped"
        );
    }

    #[test]
    fn judge_uses_cache_hit() {
        let mut findings = vec![{
            let mut f = FindingBuilder::new()
                .source(Source::Linter("ast-grep".into()))
                .rule_id("ast-grep:python/test")
                .evidence("test code")
                .build();
            f.precision_tier = Some(PrecisionTier::Speculative);
            f
        }];
        let mut metadata = HashMap::new();
        metadata.insert(
            "ast-grep:python/test".into(),
            RuleMetadata {
                precision: PrecisionTier::Speculative,
                judge: JudgeRequirement::Required,
            },
        );
        let key = verdict_cache_key("ast-grep:python/test", "test code");
        let mut cache = HashMap::new();
        cache.insert(
            key,
            CacheEntry {
                cache_key: String::new(),
                rule_id: "ast-grep:python/test".into(),
                verdict: JudgeVerdict::Approved,
                confidence: 0.9,
                reason: "cached".into(),
                timestamp: Utc::now(),
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");
        let result = judge_findings(
            &mut findings,
            "source",
            &metadata,
            &cache,
            &cache_path,
            None,
        );
        assert_eq!(result.cache_hits, 1);
        assert_eq!(result.approved, 1);
        assert_eq!(result.calls, 0);
    }

    #[test]
    fn judge_flow_end_to_end_with_cache() {
        let mut findings = vec![
            {
                let mut f = FindingBuilder::new()
                    .title("broad-exception-catch: Catching broad Exception")
                    .source(Source::Linter("ast-grep".into()))
                    .rule_id("ast-grep:python/broad-exception-catch")
                    .evidence("except Exception as e:")
                    .build();
                f.precision_tier = Some(PrecisionTier::Speculative);
                f
            },
            {
                let mut f = FindingBuilder::new()
                    .title("as-any-cast: as any cast")
                    .source(Source::Linter("ast-grep".into()))
                    .rule_id("ast-grep:typescript/as-any-cast")
                    .evidence("foo as any")
                    .build();
                f.precision_tier = Some(PrecisionTier::High);
                f
            },
        ];

        let mut metadata = HashMap::new();
        metadata.insert(
            "ast-grep:python/broad-exception-catch".into(),
            RuleMetadata {
                precision: PrecisionTier::Speculative,
                judge: JudgeRequirement::Required,
            },
        );
        metadata.insert(
            "ast-grep:typescript/as-any-cast".into(),
            RuleMetadata {
                precision: PrecisionTier::High,
                judge: JudgeRequirement::Skip,
            },
        );

        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");
        let cache = HashMap::new();

        // Mock LLM: always approve
        let mock_llm = |_prompt: &str| -> Option<String> {
            Some(r#"[{"rule_id":"ast-grep:python/broad-exception-catch","verdict":"tp","confidence":0.85,"reason":"valid"}]"#.into())
        };

        let result = judge_findings(
            &mut findings,
            "source code here",
            &metadata,
            &cache,
            &cache_path,
            Some(&mock_llm),
        );

        assert_eq!(result.approved, 1);
        assert_eq!(result.skipped, 1);
        assert_eq!(findings.len(), 2); // none dropped (approved)
        assert_eq!(findings[0].judge_verdict, Some(JudgeVerdict::Approved));
        assert_eq!(findings[0].judge_confidence, Some(0.85));
        assert_eq!(findings[1].judge_verdict, Some(JudgeVerdict::Skipped)); // skipped

        // Verify cache was written
        let loaded_cache = load_cache(&cache_path).unwrap();
        assert_eq!(loaded_cache.len(), 1);
    }

    #[test]
    fn judge_drops_required_rejected_findings() {
        let mut findings = vec![
            {
                let mut f = FindingBuilder::new()
                    .title("broad-exception-catch: Catching broad Exception")
                    .source(Source::Linter("ast-grep".into()))
                    .rule_id("ast-grep:python/broad-exception-catch")
                    .evidence("except Exception as e:")
                    .build();
                f.precision_tier = Some(PrecisionTier::Speculative);
                f
            },
            {
                let mut f = FindingBuilder::new()
                    .title("as-any-cast: as any cast")
                    .source(Source::Linter("ast-grep".into()))
                    .rule_id("ast-grep:typescript/as-any-cast")
                    .evidence("foo as any")
                    .build();
                f.precision_tier = Some(PrecisionTier::High);
                f
            },
        ];

        let mut metadata = HashMap::new();
        metadata.insert(
            "ast-grep:python/broad-exception-catch".into(),
            RuleMetadata {
                precision: PrecisionTier::Speculative,
                judge: JudgeRequirement::Required,
            },
        );
        metadata.insert(
            "ast-grep:typescript/as-any-cast".into(),
            RuleMetadata {
                precision: PrecisionTier::High,
                judge: JudgeRequirement::Skip,
            },
        );

        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");
        let cache = HashMap::new();

        // Mock LLM: rejects the speculative finding
        let mock_llm = |_prompt: &str| -> Option<String> {
            Some(r#"[{"rule_id":"ast-grep:python/broad-exception-catch","verdict":"fp","confidence":0.92,"reason":"intentional"}]"#.into())
        };

        let result = judge_findings(
            &mut findings,
            "code",
            &metadata,
            &cache,
            &cache_path,
            Some(&mock_llm),
        );

        assert_eq!(result.rejected, 1);
        assert_eq!(result.skipped, 1);
        assert_eq!(findings.len(), 1); // speculative finding was DROPPED
        assert_eq!(findings[0].title, "as-any-cast: as any cast"); // only the high-precision one remains
    }
}
