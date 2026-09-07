use crate::ast_grep::RuleMetadata;
#[cfg(test)]
use crate::finding::PrecisionTier;
use crate::finding::{Finding, JudgeRequirement, JudgeVerdict};
use crate::prompt_sanitize::pick_fence_for;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

const CACHE_TTL_DAYS: i64 = 7;

/// A hash of the source the judge was shown, computed once per file.
///
/// Quorum's own review of the #538 fix caught the naive version: the key took
/// `&str` and hashed the whole file for every finding, which is O(findings x
/// file size). On `src/calibrator.rs` -- 245 KB, 31 findings -- that is 7.6 MB
/// of SHA-256 to answer 31 cache lookups. The source component is identical
/// across those lookups, so it is hashed once here.
pub struct SourceDigest(String);

impl SourceDigest {
    pub fn of(source_code: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(source_code.as_bytes());
        let hash = hasher.finalize();
        let mut hex = String::with_capacity(hash.len() * 2);
        for byte in hash {
            use std::fmt::Write;
            let _ = write!(hex, "{byte:02x}");
        }
        Self(hex)
    }
}

/// Compute a deterministic cache key from a rule ID, the source the judge saw,
/// the finding's location, and the evidence snippet. Null byte separators
/// prevent prefix collisions between the parts.
///
/// #538: the key used to be `(rule_id, evidence)` only. But `build_judge_prompt`
/// sends the whole file and asks the model to decide "based on the surrounding
/// code context" -- so the thing being decided on was not in the key, and a
/// verdict earned in one context was replayed in every other context with the
/// same evidence string. `let _ = tx.send(());` judged fp in a shutdown path
/// came back fp where dropping that error loses data. Evidence strings for
/// speculative rules are short and repetitive by construction, so collisions
/// were the normal case, and the 7-day TTL kept each wrong answer for a week.
///
/// Including the source makes this a per-(file version, finding) memo. An edit
/// invalidates that file's verdicts, which is correct: the judge's answer
/// depends on the file. The case the cache was built for -- re-reviewing an
/// unchanged file, as the daemon does -- still hits.
///
/// The line range is in the key for the same reason the source is. Quorum's
/// review of the first version of this fix pointed out that two findings of
/// one rule with byte-identical evidence in the same file would still share a
/// verdict -- and the judge is told each finding's line range, so its answer
/// can legitimately differ between them (`let _ = tx.send(())` in a shutdown
/// path versus in a write path).
pub fn verdict_cache_key(
    rule_id: &str,
    source_digest: &SourceDigest,
    line_start: u32,
    line_end: u32,
    evidence: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(rule_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(source_digest.0.as_bytes());
    hasher.update(b"\0");
    hasher.update(format!("{line_start}-{line_end}").as_bytes());
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
    use fs2::FileExt;
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.lock_exclusive()?;
    let json = serde_json::to_string(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let result = writeln!(file, "{json}");
    let _ = file.unlock();
    result
}

// ---------------------------------------------------------------------------
// LLM judge client
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct JudgeResponseItem {
    #[serde(default)]
    pub index: Option<usize>,
    pub rule_id: String,
    pub verdict: String,
    pub confidence: f32,
    pub reason: String,
}

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn extract_json_array(response: &str) -> Option<&str> {
    let trimmed = response.trim();
    let end = trimmed.rfind(']')?;
    let mut search_from = 0;
    while let Some(rel) = trimmed[search_from..].find('[') {
        let abs_start = search_from + rel;
        if abs_start > end {
            return None;
        }
        let candidate = &trimmed[abs_start..=end];
        if serde_json::from_str::<serde_json::Value>(candidate).is_ok() {
            return Some(candidate);
        }
        search_from = abs_start + 1;
    }
    None
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
        "You are a code review judge. Each finding below was emitted by a \
         SPECULATIVE pattern rule -- a rule that matches syntax and cannot see \
         intent. Most of what these rules emit is noise.\n\n\
         For each finding, answer one question: does the surrounding code show \
         a concrete way this goes wrong at runtime?\n\n\
         \x20 \"tp\"  -- you can name the input or state that makes it fail.\n\
         \x20 \"fp\"  -- the pattern matched but the context makes it safe or \
         intended.\n\
         \x20 \"uncertain\" -- the file does not contain enough to tell.\n\n\
         Do not answer tp merely because the pattern is a real category of \
         bug. The default answer is fp; tp must be earned by evidence in this \
         file.\n\n",
    );
    let fence = pick_fence_for(source_code);
    prompt.push_str("Source code:\n");
    prompt.push_str(&fence);
    prompt.push('\n');
    prompt.push_str(source_code);
    prompt.push('\n');
    prompt.push_str(&fence);
    prompt.push_str("\n\nFindings to judge:\n");

    let items: Vec<serde_json::Value> = findings
        .iter()
        .enumerate()
        .map(|(i, (rule_id, title, start, end, evidence))| {
            serde_json::json!({
                "index": i,
                "rule_id": rule_id,
                "title": title,
                "lines": format!("{}-{}", start, end),
                "evidence": evidence,
            })
        })
        .collect();
    prompt.push_str(&serde_json::to_string_pretty(&items).unwrap_or_default());

    prompt.push_str(
        "\n\nRespond with ONLY a JSON array. Each element must include the index field: \
         {\"index\": N, \"rule_id\": \"...\", \"verdict\": \"tp\"|\"fp\"|\"uncertain\", \
         \"confidence\": 0.0-1.0, \"reason\": \"...\"}\n",
    );
    prompt
}

// ---------------------------------------------------------------------------
// Judge orchestrator
// ---------------------------------------------------------------------------

/// Findings per judge call.
///
/// #533: `judge_findings` used to send a file's entire finding set in one
/// call, and `judge_completion` caps the response at 2048 tokens. Measured on
/// `src/calibrator.rs`: 31 findings in, `finish_reason: "length"`, zero
/// verdicts out -- and `judge: required` then withheld all 31, so the judge
/// deleted findings on exactly the files that had the most.
///
/// A verdict runs about 65 tokens of `{index, rule_id, verdict, confidence,
/// reason}`, so 12 leaves roughly 2.5x headroom in the 2048-token budget.
/// ponytail: a fixed size rather than a token estimate -- an estimator would
/// need its own calibration, and the failure it prevents is now visible in the
/// output rather than silent. If reasons get more verbose, this is the knob.
pub const JUDGE_BATCH_SIZE: usize = 12;

/// Aggregate counters for a single `judge_findings` invocation.
#[derive(Debug, Default)]
pub struct JudgeResult {
    pub approved: u32,
    pub rejected: u32,
    pub uncertain: u32,
    pub skipped: u32,
    /// Findings dropped because their rule declares `judge: required` and no
    /// judge ran at all. Fixable by the user: pass `--judge`.
    pub withheld_no_judge: u32,
    /// Findings dropped because a judge ran and returned no verdict for them --
    /// the call failed, the response would not parse, or the model omitted the
    /// item. NOT fixable by passing `--judge`; it already was.
    ///
    /// #533: both states used to be one counter, so a user whose judge had just
    /// errored was told to "run with --judge to evaluate them". A judge that
    /// returns nothing on a token cap is indistinguishable from a judge that
    /// examined and abstained unless the output says which happened.
    pub withheld_judge_failed: u32,
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
#[allow(clippy::too_many_arguments)]
pub async fn judge_findings<J: JudgeLlm>(
    findings: &mut Vec<Finding>,
    source_code: &str,
    metadata: &HashMap<String, RuleMetadata>,
    cache: &HashMap<String, CacheEntry>,
    cache_path: &Path,
    llm: Option<&J>,
) -> JudgeResult {
    let start = std::time::Instant::now();
    let mut result = JudgeResult::default();
    let source_digest = SourceDigest::of(source_code);

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
        let key = verdict_cache_key(rule_id, &source_digest, f.line_start, f.line_end, evidence);

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

    // Phase 2: LLM calls for uncached findings, in bounded batches (#533).
    //
    // With no LLM the loop simply does not run, and verdicts stay `None`.
    // That is deliberate: marking them `Uncertain` conflates two very
    // different states -- "the judge looked and could not rule it out" (worth
    // keeping) and "no judge ever looked" (not worth showing for a rule that
    // declares it requires one). `enforce_judge_required` below drops the
    // latter and the caller reports which state produced the count.
    if !to_judge.is_empty()
        && let Some(llm) = llm
    {
        for batch in to_judge.chunks(JUDGE_BATCH_SIZE) {
            judge_one_batch(
                batch,
                findings,
                source_code,
                &source_digest,
                cache_path,
                llm,
                &mut result,
            )
            .await;
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

    // Attribute the withholding to the state that caused it. Only the caller
    // knows which happened, so `enforce_judge_required` stays a plain count.
    let withheld = enforce_judge_required(findings, metadata);
    if llm.is_some() {
        result.withheld_judge_failed = withheld;
    } else {
        result.withheld_no_judge = withheld;
    }

    result.latency_ms = start.elapsed().as_millis() as u64;
    result
}

/// Judge one bounded batch of findings, writing verdicts and cache entries.
///
/// Split out of `judge_findings` when batching arrived (#533): the per-batch
/// work is the same, only now it runs more than once, and `judge_findings` was
/// already at complexity 28 before adding a loop around it.
///
/// Findings this batch does not resolve are deliberately left at `None`. If
/// the call returned nothing, returned malformed JSON, or simply omitted an
/// item, no judgment occurred -- which is the same state as having no judge at
/// all, and must not be recorded as `Uncertain`. `Uncertain` means the judge
/// looked and could not decide; only a real verdict may set it.
///
/// A batch that fails does not affect the batches that succeeded: their
/// verdicts are already written to `findings` and to the cache.
async fn judge_one_batch<J: JudgeLlm>(
    batch: &[usize],
    findings: &mut [Finding],
    source_code: &str,
    source_digest: &SourceDigest,
    cache_path: &Path,
    llm: &J,
    result: &mut JudgeResult,
) {
    let items: Vec<_> = batch
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

    let Some(response) = llm.call(&prompt).await else {
        tracing::warn!(
            batch_size = batch.len(),
            "judge LLM returned no response for this batch"
        );
        return;
    };

    let Some(json_str) = extract_json_array(&response) else {
        tracing::warn!(
            response_len = response.len(),
            response_prefix = %truncate_chars(&response, 200),
            "judge: no JSON array found in LLM response"
        );
        return;
    };

    let verdicts = match serde_json::from_str::<Vec<JudgeResponseItem>>(json_str) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                error = %e,
                json_prefix = %truncate_chars(json_str, 200),
                "judge: failed to parse JSON response"
            );
            return;
        }
    };

    let mut used = vec![false; batch.len()];
    for v in &verdicts {
        let batch_pos = v
            .index
            .filter(|&idx| {
                idx < batch.len()
                    && !used[idx]
                    && findings[batch[idx]].rule_id.as_deref() == Some(&v.rule_id)
            })
            .or_else(|| {
                batch.iter().enumerate().position(|(pos, &i)| {
                    !used[pos] && findings[i].rule_id.as_deref() == Some(&v.rule_id)
                })
            });
        let Some(pos) = batch_pos else { continue };

        used[pos] = true;
        let i = batch[pos];
        let verdict = parse_verdict(&v.verdict);
        let confidence = v.confidence.clamp(0.0, 1.0);
        findings[i].judge_verdict = Some(verdict.clone());
        findings[i].judge_confidence = Some(confidence);

        let evidence = findings[i]
            .evidence
            .first()
            .map(|s| s.as_str())
            .unwrap_or("");
        let canonical_rule_id = findings[i].rule_id.as_deref().unwrap_or("");
        let key = verdict_cache_key(
            canonical_rule_id,
            source_digest,
            findings[i].line_start,
            findings[i].line_end,
            evidence,
        );
        if let Err(e) = write_cache_entry(
            cache_path,
            &CacheEntry {
                cache_key: key,
                rule_id: canonical_rule_id.to_string(),
                verdict: verdict.clone(),
                confidence,
                reason: v.reason.clone(),
                timestamp: Utc::now(),
            },
        ) {
            tracing::warn!(
                path = %cache_path.display(),
                error = %e,
                "failed to persist judge cache entry"
            );
        }

        match verdict {
            JudgeVerdict::Approved => result.approved += 1,
            JudgeVerdict::Rejected => result.rejected += 1,
            _ => result.uncertain += 1,
        }
    }
}

/// Enforce `judge: required`, dropping findings from rules that declare they
/// need judgment but did not get it. Returns the number withheld for lack of
/// a judgment (as opposed to an explicit rejection).
///
/// A finding is dropped only on an explicit `Rejected` verdict, or when no
/// judgment happened at all. `Uncertain` is deliberately kept: the judge
/// looked and did not rule it out, which is different from never looking.
///
/// This used to live at the tail of [`judge_findings`], which runs only when
/// `--judge` is passed (#520). So a rule declaring that it requires judgment
/// emitted completely unjudged on every default review -- the guard sat
/// downstream of the very flag whose absence it exists to compensate for.
/// That is how the speculative rules reached users raw: `missing-await` at
/// ~40 findings per async Python file, `jinja-loop-variable-scoping` at 0/8
/// precision, `string-byte-slice-broad` 0/28, `discarded-result` 3/46. Those
/// rules are not bad; they were running without their mandatory filter.
///
/// It is now a free function so the no-judge path in `pipeline::review_file`
/// can call it too. Callers must invoke it exactly once per finding set --
/// it is idempotent on the findings but the returned count is not additive
/// across calls.
pub fn enforce_judge_required(
    findings: &mut Vec<Finding>,
    metadata: &HashMap<String, RuleMetadata>,
) -> u32 {
    let mut withheld_unjudged = 0usize;
    findings.retain(|f| {
        let meta = f
            .rule_id
            .as_ref()
            .and_then(|rid| metadata.get(rid.as_str()))
            .cloned()
            .unwrap_or_default();
        if meta.judge != JudgeRequirement::Required {
            return true;
        }
        match &f.judge_verdict {
            Some(JudgeVerdict::Rejected) => false,
            None => {
                withheld_unjudged += 1;
                false
            }
            _ => true,
        }
    });
    withheld_unjudged as u32
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

    /// Answers every finding in whatever batch it is handed, and records how
    /// big each batch was. Lets a test assert on chunking without hardcoding
    /// the response.
    #[derive(Default)]
    struct EchoJudge {
        batches: std::sync::Mutex<Vec<usize>>,
    }

    impl JudgeLlm for EchoJudge {
        async fn call(&self, prompt: &str) -> Option<String> {
            let start = prompt.find("Findings to judge:\n")? + "Findings to judge:\n".len();
            let arr = &prompt[start..];
            let end = arr.rfind(']')?;
            let items: Vec<serde_json::Value> = serde_json::from_str(&arr[..=end]).ok()?;
            self.batches.lock().unwrap().push(items.len());
            let out: Vec<serde_json::Value> = items
                .iter()
                .enumerate()
                .map(|(i, it)| {
                    serde_json::json!({
                        "index": i,
                        "rule_id": it["rule_id"],
                        "verdict": "tp",
                        "confidence": 0.9,
                        "reason": "ok",
                    })
                })
                .collect();
            Some(serde_json::to_string(&out).unwrap())
        }
    }

    #[test]
    fn cache_key_deterministic() {
        let d = SourceDigest::of("src");
        let k1 = verdict_cache_key(
            "ast-grep:python/bare-except-pass",
            &d,
            1,
            2,
            "except:\n    pass",
        );
        let k2 = verdict_cache_key(
            "ast-grep:python/bare-except-pass",
            &d,
            1,
            2,
            "except:\n    pass",
        );
        assert_eq!(k1, k2);
    }

    /// Quorum's review of the first #538 fix: two findings of one rule with
    /// byte-identical evidence in the SAME file still shared a verdict. The
    /// judge is told each finding's line range, so its answer can legitimately
    /// differ between them -- `let _ = tx.send(())` in a shutdown path versus
    /// in a write path -- and one cached answer would have covered both.
    #[test]
    fn cache_key_differs_for_the_same_evidence_at_different_lines() {
        let d =
            SourceDigest::of("fn a() { let _ = tx.send(()); }\nfn b() { let _ = tx.send(()); }");
        let k1 = verdict_cache_key("ast-grep:rust/r", &d, 1, 1, "let _ = tx.send(());");
        let k2 = verdict_cache_key("ast-grep:rust/r", &d, 2, 2, "let _ = tx.send(());");
        assert_ne!(k1, k2);
    }

    /// #538: the judge decides on surrounding context, so the context must be
    /// in the key. Identical evidence in two different files is the normal
    /// case for speculative rules, not the edge case.
    #[test]
    fn cache_key_differs_when_the_surrounding_source_differs() {
        let shutdown = "fn stop(tx: Sender<()>) { let _ = tx.send(()); }";
        let hot_path = "fn persist(tx: Sender<Row>) { let _ = tx.send(row); }";
        let k1 = verdict_cache_key(
            "ast-grep:rust/discarded-result",
            &SourceDigest::of(shutdown),
            1,
            1,
            "let _ = tx.send(());",
        );
        let k2 = verdict_cache_key(
            "ast-grep:rust/discarded-result",
            &SourceDigest::of(hot_path),
            1,
            1,
            "let _ = tx.send(());",
        );
        assert_ne!(
            k1, k2,
            "a verdict earned in one file must not be replayed in another"
        );
    }

    #[test]
    fn cache_key_differs_for_different_evidence() {
        let d = SourceDigest::of("src");
        let k1 = verdict_cache_key("rule-a", &d, 1, 1, "code1");
        let k2 = verdict_cache_key("rule-a", &d, 1, 1, "code2");
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_differs_for_different_rules() {
        let d = SourceDigest::of("src");
        let k1 = verdict_cache_key("rule-a", &d, 1, 1, "code");
        let k2 = verdict_cache_key("rule-b", &d, 1, 1, "code");
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
    fn judge_prompt_states_the_bar_instead_of_asking_neutrally() {
        // #520 part 2. The original wording -- "determine if it is a true
        // positive (tp), false positive (fp), or uncertain based on the
        // surrounding code context" -- sets no bar, and an LLM asked neutrally
        // about a plausible finding says yes. Measured over 187 labelled
        // findings (docs/judge-eval-520.md): the judge approved 14 of 15
        // `discarded-result` findings a human had already recorded as false.
        //
        // Naming the speculative provenance and making fp the default answer
        // moved survivor precision from 12% to 100% on that rule, and cost
        // nothing in recall -- all 11 constructed true positives still passed.
        // This test exists so a neutral rewrite fails loudly rather than
        // quietly restoring an approve-everything judge.
        let findings = vec![(
            "ast-grep:rust/some-rule".to_string(),
            "some-rule: something might be wrong".to_string(),
            1u32,
            2u32,
            "let _ = x();".to_string(),
        )];
        let prompt = build_judge_prompt("fn main() {}", &findings);
        assert!(
            prompt.to_lowercase().contains("speculative"),
            "prompt must tell the judge these findings come from a speculative \
             pattern rule; without that it treats them as vetted"
        );
        assert!(
            prompt.contains("default answer is fp"),
            "prompt must make fp the default so tp has to be earned by \
             evidence in the file"
        );
    }

    #[test]
    fn build_judge_prompt_escapes_special_characters() {
        let findings = vec![(
            "test-rule".to_string(),
            "title with \"quotes\" and\nnewlines".to_string(),
            1u32,
            5u32,
            "evidence with \\backslash and \"double quotes\"".to_string(),
        )];
        let prompt = build_judge_prompt("fn main() {}", &findings);
        let json_start = prompt.find('[').expect("should contain JSON array");
        let json_end = prompt.rfind(']').expect("should end with JSON array");
        let json_str = &prompt[json_start..=json_end];
        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(json_str).expect("findings must be valid JSON");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["title"], "title with \"quotes\" and\nnewlines");
        assert_eq!(
            parsed[0]["evidence"],
            "evidence with \\backslash and \"double quotes\""
        );
    }

    #[test]
    fn build_judge_prompt_round_trips_values() {
        let findings = vec![
            (
                "rule-a".to_string(),
                "simple title".to_string(),
                10u32,
                20u32,
                "some evidence".to_string(),
            ),
            (
                "rule-b".to_string(),
                "title with {braces} and [brackets]".to_string(),
                30u32,
                40u32,
                "evidence\twith\ttabs".to_string(),
            ),
        ];
        let prompt = build_judge_prompt("let x = 1;", &findings);
        let json_start = prompt.find('[').unwrap();
        let json_end = prompt.rfind(']').unwrap();
        let json_str = &prompt[json_start..=json_end];
        let parsed: Vec<serde_json::Value> = serde_json::from_str(json_str).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["rule_id"], "rule-a");
        assert_eq!(parsed[0]["index"], 0);
        assert_eq!(parsed[1]["rule_id"], "rule-b");
        assert_eq!(parsed[1]["index"], 1);
        assert_eq!(parsed[1]["title"], "title with {braces} and [brackets]");
    }

    #[test]
    fn build_judge_prompt_uses_dynamic_fence_for_source_with_backticks() {
        let source = "let x = \"```\"; let y = \"````\";";
        let prompt = build_judge_prompt(source, &[]);
        assert!(
            !prompt.contains("Source code:\n```\n"),
            "prompt must not use a 3-backtick fence when source contains ```; got:\n{prompt}"
        );
        let fence_start = prompt.find("Source code:\n").unwrap() + "Source code:\n".len();
        let fence_end = prompt[fence_start..].find('\n').unwrap();
        let fence = &prompt[fence_start..fence_start + fence_end];
        assert!(
            fence.len() >= 5,
            "fence must be longer than the longest backtick run (4) in the source; got: {fence}"
        );
        assert!(
            prompt.contains(source),
            "source code must still appear in the prompt"
        );
    }

    #[test]
    fn build_judge_prompt_empty_findings_valid_json() {
        let prompt = build_judge_prompt("fn main() {}", &[]);
        let json_start = prompt.find('[').expect("should contain JSON array");
        let json_end = prompt.rfind(']').expect("should end with JSON array");
        let json_str = &prompt[json_start..=json_end];
        let parsed: Vec<serde_json::Value> = serde_json::from_str(json_str).unwrap();
        assert!(parsed.is_empty());
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

    #[tokio::test]
    async fn judge_skips_high_precision_rules() {
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
                skip_test_files: false,
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
            None::<&MockJudge>,
        )
        .await;
        assert_eq!(result.skipped, 1);
        assert_eq!(findings[0].judge_verdict, Some(JudgeVerdict::Skipped));
    }

    #[tokio::test]
    async fn judge_approves_with_mock_llm() {
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
                skip_test_files: false,
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");
        let mock = MockJudge {
            response: Some(
                r#"[{"rule_id":"ast-grep:python/broad-exception-catch","verdict":"tp","confidence":0.85,"reason":"valid"}]"#.into(),
            ),
        };
        let result = judge_findings(
            &mut findings,
            "source",
            &metadata,
            &HashMap::new(),
            &cache_path,
            Some(&mock),
        )
        .await;
        assert_eq!(result.approved, 1);
        assert_eq!(result.calls, 1);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].judge_verdict, Some(JudgeVerdict::Approved));
        assert_eq!(findings[0].judge_confidence, Some(0.85));
        // Verify cache was written
        let loaded = load_cache(&cache_path).unwrap();
        assert_eq!(loaded.len(), 1);
    }

    #[tokio::test]
    async fn judge_drops_required_rejected() {
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
                skip_test_files: false,
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");
        let mock = MockJudge {
            response: Some(
                r#"[{"rule_id":"ast-grep:python/broad-exception-catch","verdict":"fp","confidence":0.92,"reason":"intentional top-level handler"}]"#.into(),
            ),
        };
        let result = judge_findings(
            &mut findings,
            "source",
            &metadata,
            &HashMap::new(),
            &cache_path,
            Some(&mock),
        )
        .await;
        assert_eq!(result.rejected, 1);
        assert!(
            findings.is_empty(),
            "Required+Rejected finding should be dropped"
        );
    }

    #[tokio::test]
    async fn judge_uses_cache_hit() {
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
                skip_test_files: false,
            },
        );
        // #538: the key now includes the source the judge saw, so it must be
        // built from the same string judge_findings is called with below.
        // Built from the same (source, location, evidence) the lookup will
        // use, now that the key carries all three (#538).
        let key = verdict_cache_key(
            "ast-grep:python/test",
            &SourceDigest::of("source"),
            findings[0].line_start,
            findings[0].line_end,
            "test code",
        );
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
            None::<&MockJudge>,
        )
        .await;
        assert_eq!(result.cache_hits, 1);
        assert_eq!(result.approved, 1);
        assert_eq!(result.calls, 0);
    }

    #[tokio::test]
    async fn judge_flow_end_to_end_with_cache() {
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
                skip_test_files: false,
            },
        );
        metadata.insert(
            "ast-grep:typescript/as-any-cast".into(),
            RuleMetadata {
                precision: PrecisionTier::High,
                judge: JudgeRequirement::Skip,
                skip_test_files: false,
            },
        );

        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");
        let cache = HashMap::new();

        // Mock LLM: always approve
        let mock = MockJudge {
            response: Some(r#"[{"rule_id":"ast-grep:python/broad-exception-catch","verdict":"tp","confidence":0.85,"reason":"valid"}]"#.into()),
        };

        let result = judge_findings(
            &mut findings,
            "source code here",
            &metadata,
            &cache,
            &cache_path,
            Some(&mock),
        )
        .await;

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

    #[tokio::test]
    async fn judge_drops_required_rejected_findings() {
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
                skip_test_files: false,
            },
        );
        metadata.insert(
            "ast-grep:typescript/as-any-cast".into(),
            RuleMetadata {
                precision: PrecisionTier::High,
                judge: JudgeRequirement::Skip,
                skip_test_files: false,
            },
        );

        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");
        let cache = HashMap::new();

        // Mock LLM: rejects the speculative finding
        let mock = MockJudge {
            response: Some(r#"[{"rule_id":"ast-grep:python/broad-exception-catch","verdict":"fp","confidence":0.92,"reason":"intentional"}]"#.into()),
        };

        let result = judge_findings(
            &mut findings,
            "code",
            &metadata,
            &cache,
            &cache_path,
            Some(&mock),
        )
        .await;

        assert_eq!(result.rejected, 1);
        assert_eq!(result.skipped, 1);
        assert_eq!(findings.len(), 1); // speculative finding was DROPPED
        assert_eq!(findings[0].title, "as-any-cast: as any cast"); // only the high-precision one remains
    }

    #[tokio::test]
    async fn judge_handles_llm_failure_gracefully() {
        let judge = MockJudge { response: None };

        let mut findings = vec![{
            let mut f = FindingBuilder::new()
                .source(Source::Linter("ast-grep".into()))
                .rule_id("ast-grep:rust/discarded-result")
                .evidence("let _ = foo()")
                .build();
            f.precision_tier = Some(PrecisionTier::Speculative);
            f
        }];
        let mut metadata = HashMap::new();
        metadata.insert(
            "ast-grep:rust/discarded-result".into(),
            RuleMetadata {
                precision: PrecisionTier::Speculative,
                judge: JudgeRequirement::Required,
                skip_test_files: false,
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");

        let result = judge_findings(
            &mut findings,
            "fn main() { let _ = foo(); }",
            &metadata,
            &HashMap::new(),
            &cache_path,
            Some(&judge),
        )
        .await;

        // These findings are `judge: required`. A failed call is not a verdict,
        // so they are withheld rather than emitted as if examined. This test
        // previously asserted `Uncertain`, which is the conflation quorum
        // flagged when reviewing the enforcement change.
        assert_eq!(result.calls, 1, "the call is still attempted and counted");
        assert_eq!(result.uncertain, 0, "a failed call yields no verdict");
        assert_eq!(result.withheld_judge_failed, 1);
        assert!(findings.is_empty(), "required findings must not survive");
    }

    #[tokio::test]
    async fn judge_handles_partial_llm_response() {
        let judge = MockJudge {
            response: Some(
                r#"[{"rule_id":"ast-grep:python/missing-await","verdict":"tp","confidence":0.9,"reason":"ok"}]"#
                    .into(),
            ),
        };

        let mut findings = vec![
            {
                let mut f = FindingBuilder::new()
                    .source(Source::Linter("ast-grep".into()))
                    .rule_id("ast-grep:python/missing-await")
                    .evidence("await missing")
                    .build();
                f.precision_tier = Some(PrecisionTier::Speculative);
                f
            },
            {
                let mut f = FindingBuilder::new()
                    .source(Source::Linter("ast-grep".into()))
                    .rule_id("ast-grep:python/logging-debug-leak")
                    .evidence("logger.debug(secret)")
                    .build();
                f.precision_tier = Some(PrecisionTier::Speculative);
                f
            },
        ];
        let mut metadata = HashMap::new();
        metadata.insert(
            "ast-grep:python/missing-await".into(),
            RuleMetadata {
                precision: PrecisionTier::Speculative,
                judge: JudgeRequirement::Required,
                skip_test_files: false,
            },
        );
        metadata.insert(
            "ast-grep:python/logging-debug-leak".into(),
            RuleMetadata {
                precision: PrecisionTier::Speculative,
                judge: JudgeRequirement::Required,
                skip_test_files: false,
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");

        let result = judge_findings(
            &mut findings,
            "source",
            &metadata,
            &HashMap::new(),
            &cache_path,
            Some(&judge),
        )
        .await;

        // The judge returned a verdict for one finding and omitted the other.
        // The answered one is approved; the omitted one received no judgment at
        // all, so for a `judge: required` rule it is withheld rather than
        // emitted as `Uncertain`.
        assert_eq!(result.approved, 1);
        assert_eq!(result.uncertain, 0, "an omitted item is not a verdict");
        assert_eq!(result.withheld_judge_failed, 1);
        assert_eq!(findings.len(), 1, "only the judged finding survives");
    }

    #[tokio::test]
    async fn judge_handles_malformed_json_response() {
        let judge = MockJudge {
            response: Some("not valid json at all {{{".into()),
        };

        let mut findings = vec![{
            let mut f = FindingBuilder::new()
                .source(Source::Linter("ast-grep".into()))
                .rule_id("ast-grep:python/missing-await")
                .evidence("await missing")
                .build();
            f.precision_tier = Some(PrecisionTier::Speculative);
            f
        }];
        let mut metadata = HashMap::new();
        metadata.insert(
            "ast-grep:python/missing-await".into(),
            RuleMetadata {
                precision: PrecisionTier::Speculative,
                judge: JudgeRequirement::Required,
                skip_test_files: false,
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");

        let result = judge_findings(
            &mut findings,
            "async def foo(): bar()",
            &metadata,
            &HashMap::new(),
            &cache_path,
            Some(&judge),
        )
        .await;

        // Malformed JSON degrades gracefully -- no panic, call counted -- but it
        // is not a verdict. For a `judge: required` rule the finding is
        // withheld, not emitted with a fabricated `Uncertain`.
        assert_eq!(result.calls, 1, "malformed JSON must not panic");
        assert_eq!(result.uncertain, 0);
        assert_eq!(result.withheld_judge_failed, 1);
        assert!(findings.is_empty());
    }

    fn speculative_finding(rule: &str) -> Finding {
        let mut f = FindingBuilder::new()
            .title("speculative")
            .severity(crate::finding::Severity::Medium)
            .source(Source::Linter("ast-grep".into()))
            .rule_id(rule)
            .evidence("ev")
            .lines(1, 1)
            .build();
        f.precision_tier = Some(crate::finding::PrecisionTier::Speculative);
        f
    }

    fn required_meta(rule: &str) -> HashMap<String, RuleMetadata> {
        let mut m = HashMap::new();
        m.insert(
            rule.to_string(),
            RuleMetadata {
                precision: crate::finding::PrecisionTier::Speculative,
                judge: crate::finding::JudgeRequirement::Required,
                skip_test_files: false,
            },
        );
        m
    }

    /// The bug this enforcement fixes. `judge: required` used to drop a finding
    /// only on an explicit `Rejected`; with no judge configured the verdict
    /// stayed `None` and the finding was KEPT, so rules declaring that they
    /// require judgment shipped completely unjudged. That is how
    /// `jinja-loop-variable-scoping` reached 0/8 precision and `missing-await`
    /// produced ~40 findings on one Python diff.
    #[tokio::test]
    async fn required_rules_are_withheld_when_no_judge_runs() {
        let rule = "ast-grep:yaml/jinja-loop-variable-scoping";
        let mut findings = vec![speculative_finding(rule)];
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");

        let result = judge_findings(
            &mut findings,
            "src",
            &required_meta(rule),
            &HashMap::new(),
            &cache_path,
            None::<&MockJudge>, // no judge configured -- the default
        )
        .await;

        assert!(
            findings.is_empty(),
            "a judge:required finding must not survive an unjudged run"
        );
        assert_eq!(
            result.withheld_no_judge, 1,
            "withholding must be counted so it can be reported, not silent"
        );
    }

    /// The second half of the enforcement, found by quorum reviewing the first
    /// half. When a judge IS configured but its call fails or omits an item,
    /// #533: a batch big enough to overrun `max_tokens` must be split.
    ///
    /// `judge_findings` sent every one of a file's findings in a single call,
    /// and `judge_completion` caps the response at 2048 tokens. Measured on
    /// `src/calibrator.rs`: 31 findings, `finish_reason: "length"`, and not one
    /// verdict came back. Under `judge: required` all 31 were then withheld --
    /// so the judge silently deleted the findings on exactly the files with
    /// the most of them.
    #[tokio::test]
    async fn large_finding_sets_are_split_into_bounded_batches() {
        let rule = "ast-grep:python/some-speculative";
        let n = JUDGE_BATCH_SIZE * 2 + 3;
        let mut findings: Vec<Finding> = (0..n)
            .map(|i| {
                let mut f = speculative_finding(rule);
                f.evidence = vec![format!("evidence number {i}")];
                f
            })
            .collect();
        let judge = EchoJudge::default();
        let dir = tempfile::tempdir().unwrap();

        let result = judge_findings(
            &mut findings,
            "src",
            &required_meta(rule),
            &HashMap::new(),
            &dir.path().join("cache.jsonl"),
            Some(&judge),
        )
        .await;

        let batches = judge.batches.lock().unwrap().clone();
        assert!(
            batches.iter().all(|&b| b <= JUDGE_BATCH_SIZE),
            "no batch may exceed the size the response budget can answer: {batches:?}"
        );
        assert_eq!(
            batches.iter().sum::<usize>(),
            n,
            "every finding must be sent"
        );
        assert_eq!(
            result.calls,
            batches.len() as u32,
            "calls must be counted per batch"
        );
        assert_eq!(
            result.approved as usize, n,
            "every finding must come back judged"
        );
        assert_eq!(
            result.withheld_judge_failed, 0,
            "nothing should be withheld when the judge answered"
        );
    }

    /// #533: one failed batch must not take the batches that succeeded.
    #[tokio::test]
    async fn a_failed_batch_does_not_discard_the_ones_that_answered() {
        let rule = "ast-grep:python/some-speculative";
        // Answers the first batch, then goes quiet -- the shape of a truncated
        // or rate-limited call partway through a large file.
        struct FlakyJudge {
            seen: std::sync::Mutex<usize>,
        }
        impl JudgeLlm for FlakyJudge {
            async fn call(&self, prompt: &str) -> Option<String> {
                // Scoped so the guard is not live across the await below.
                let first = {
                    let mut seen = self.seen.lock().unwrap();
                    *seen += 1;
                    *seen == 1
                };
                if !first {
                    return None;
                }
                EchoJudge::default().call(prompt).await
            }
        }

        let n = JUDGE_BATCH_SIZE + 2;
        let mut findings: Vec<Finding> = (0..n)
            .map(|i| {
                let mut f = speculative_finding(rule);
                f.evidence = vec![format!("evidence number {i}")];
                f
            })
            .collect();
        let judge = FlakyJudge {
            seen: std::sync::Mutex::new(0),
        };
        let dir = tempfile::tempdir().unwrap();

        let result = judge_findings(
            &mut findings,
            "src",
            &required_meta(rule),
            &HashMap::new(),
            &dir.path().join("cache.jsonl"),
            Some(&judge),
        )
        .await;

        assert_eq!(
            result.approved as usize, JUDGE_BATCH_SIZE,
            "the batch that answered must keep its verdicts"
        );
        assert_eq!(
            result.withheld_judge_failed, 2,
            "only the findings in the failed batch are withheld"
        );
        assert_eq!(findings.len(), JUDGE_BATCH_SIZE);
    }

    /// #533: "no judge ran" and "the judge ran and failed" are different
    /// states and must be counted separately.
    ///
    /// Both used to land in `withheld_unjudged`, so the summary line told a
    /// user whose judge had just errored to "run with --judge to evaluate
    /// them" -- advice they had already taken. Same shape as a scanner that
    /// cannot run reporting an empty file.
    #[tokio::test]
    async fn a_judge_that_failed_is_not_reported_as_a_judge_that_never_ran() {
        let rule = "ast-grep:python/some-speculative";
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");

        let mut no_judge = vec![speculative_finding(rule)];
        let ran_none = judge_findings(
            &mut no_judge,
            "src",
            &required_meta(rule),
            &HashMap::new(),
            &cache_path,
            None::<&MockJudge>,
        )
        .await;

        let mut failed = vec![speculative_finding(rule)];
        let errored = judge_findings(
            &mut failed,
            "src",
            &required_meta(rule),
            &HashMap::new(),
            &cache_path,
            Some(&MockJudge { response: None }),
        )
        .await;

        assert_eq!(
            (ran_none.withheld_no_judge, ran_none.withheld_judge_failed),
            (1, 0)
        );
        assert_eq!(
            (errored.withheld_no_judge, errored.withheld_judge_failed),
            (0, 1)
        );
    }

    /// the old code marked the finding `Uncertain` -- which the retain keeps --
    /// so a required rule still emitted unjudged. `Uncertain` must mean "the
    /// judge looked and could not decide", never "the call failed".
    #[tokio::test]
    async fn required_rules_are_withheld_when_the_judge_call_fails() {
        let rule = "ast-grep:python/some-speculative";
        let mut findings = vec![speculative_finding(rule)];
        let judge = MockJudge { response: None }; // configured, but returns nothing
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");

        let result = judge_findings(
            &mut findings,
            "src",
            &required_meta(rule),
            &HashMap::new(),
            &cache_path,
            Some(&judge),
        )
        .await;

        assert!(
            findings.is_empty(),
            "a failed judge call must not be treated as an Uncertain verdict"
        );
        assert_eq!(result.withheld_judge_failed, 1);
        assert_eq!(
            result.uncertain, 0,
            "no real verdict was returned, so nothing is genuinely Uncertain"
        );
    }

    /// Rules that do not require a judge are unaffected: this must not become a
    /// blanket suppression of speculative findings.
    #[tokio::test]
    async fn optional_rules_survive_an_unjudged_run() {
        let rule = "ast-grep:python/some-optional";
        let mut findings = vec![speculative_finding(rule)];
        let mut meta = HashMap::new();
        meta.insert(
            rule.to_string(),
            RuleMetadata {
                precision: PrecisionTier::Speculative,
                judge: JudgeRequirement::Optional,
                skip_test_files: false,
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");

        let result = judge_findings(
            &mut findings,
            "src",
            &meta,
            &HashMap::new(),
            &cache_path,
            None::<&MockJudge>,
        )
        .await;

        assert_eq!(findings.len(), 1, "judge:optional must still emit");
        assert_eq!(result.withheld_no_judge, 0);
    }
}
