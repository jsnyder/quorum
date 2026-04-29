/// Feedback storage: JSONL file for recording TP/FP verdicts on findings.
///
/// Verdict on-disk schema (backward-compatible):
/// - Unit variants serialize as bare strings: "tp", "fp", "partial", "wontfix".
/// - Struct variant `ContextMisleading` serializes as an externally-tagged
///   object: `{"context_misleading": {"blamed_chunk_ids": ["c1", "c2"]}}`.
///
/// Legacy entries without the struct variant continue to deserialize unchanged.

use std::path::PathBuf;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Tp,
    Fp,
    Partial,
    Wontfix,
    /// Recorded when the injected retrieval context misled the reviewer.
    /// `blamed_chunk_ids` may be empty (defaults to "last-injected" downstream).
    ContextMisleading { blamed_chunk_ids: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Human,
    PostFix,                // verdict recorded after applying a fix (strongest signal)
    AutoCalibrate(String),  // model name used for auto-calibration
    /// Verdict from another review agent (pal, third-opinion, gemini, reviewdog, ...).
    /// Calibrator weight: 0.7x (see calibrator.rs). `confidence` is stored but
    /// IGNORED by the calibrator in v1 — reserved for future confidence-aware weighting.
    /// `#[serde(default)]` on optional fields protects forward-compat: a
    /// future release that adds a new optional field can still deserialize
    /// old `{"external":{...}}` rows. `agent` deliberately has no default —
    /// it's the External grouping key (see analytics::compute_tier_stats),
    /// `normalize_agent` rejects empty strings, and silently allowing
    /// `agent: ""` would create a phantom blank-agent bucket in stats.
    External {
        agent: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        confidence: Option<f32>,
    },
    Unknown,
}

impl Default for Provenance {
    fn default() -> Self {
        Provenance::Unknown
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackEntry {
    pub file_path: String,
    pub finding_title: String,
    pub finding_category: String,
    pub verdict: Verdict,
    pub reason: String,
    pub model: Option<String>,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub provenance: Provenance,
}

/// Input for recording a verdict from an external review agent.
///
/// Use `FeedbackStore::record_external` instead of constructing a `FeedbackEntry`
/// directly — it handles agent-name normalization, confidence clamping, and
/// timestamp assignment. See issue #32.
#[derive(Debug, Clone)]
pub struct ExternalVerdictInput {
    pub file_path: String,
    pub finding_title: String,
    pub finding_category: Option<String>,
    pub verdict: Verdict,
    pub reason: String,
    pub agent: String,
    pub agent_model: Option<String>,
    pub confidence: Option<f32>,
}

/// Clamp confidence to [0,1], mapping NaN/±Inf to None.
/// `f32::clamp` is NOT NaN-safe — this wraps it with an `is_finite` gate
/// (quorum self-review 2026-04-24).
pub(crate) fn clamp_confidence(c: Option<f32>) -> Option<f32> {
    c.filter(|x| x.is_finite()).map(|x| x.clamp(0.0, 1.0))
}

/// Drain a `read_dir`-style iterator into (paths, errors), surfacing
/// per-entry I/O errors as file-level `DrainError`s instead of dropping
/// them on the floor (issue #103).
///
/// Production caller: `read.map(|r| r.map(|e| e.path()))` so the helper
/// stays decoupled from `std::fs::DirEntry` (which has a private
/// constructor — un-fakeable in unit tests). Caller is responsible for
/// downstream filtering (extension, is_dir) and sorting.
pub(crate) fn drain_inbox_entries<I>(entries: I) -> (Vec<PathBuf>, Vec<DrainError>)
where
    I: IntoIterator<Item = std::io::Result<PathBuf>>,
{
    let mut paths = Vec::new();
    let mut errors = Vec::new();
    for entry in entries {
        match entry {
            Ok(p) => paths.push(p),
            Err(e) => errors.push(DrainError {
                file: PathBuf::from("<read_dir-iteration>"),
                line: 0,
                message: format!("read_dir entry failed: {e}"),
            }),
        }
    }
    (paths, errors)
}

/// Normalize an agent name: trim + lowercase. Returns Err for empty-after-trim.
pub(crate) fn normalize_agent(raw: &str) -> anyhow::Result<String> {
    let t = raw.trim();
    if t.is_empty() {
        anyhow::bail!("agent name cannot be empty after normalization");
    }
    Ok(t.to_lowercase())
}

/// Summary of a single `drain_inbox` call. `processed_dir_total_bytes` is
/// the CUMULATIVE size of `processed_dir` after drain (drives the 50MB warning),
/// NOT the per-run size (quorum self-review 2026-04-24).
#[derive(Debug, Clone, Default)]
pub struct DrainReport {
    pub drained_files: usize,
    pub entries: usize,
    pub errors: Vec<DrainError>,
    pub processed_dir_total_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct DrainError {
    pub file: PathBuf,
    /// Line number (1-indexed). `0` = file-level error (read/rename failure).
    pub line: usize,
    pub message: String,
}

/// Wire format that agents drop into `~/.quorum/inbox/*.jsonl`.
/// Structurally mirrors `ExternalVerdictInput` — kept as a separate type so
/// the on-disk schema can evolve independently of the in-memory DTO.
#[derive(Debug, Deserialize)]
struct ExternalVerdictInputWire {
    file_path: String,
    finding_title: String,
    finding_category: Option<String>,
    verdict: Verdict,
    reason: String,
    agent: String,
    agent_model: Option<String>,
    confidence: Option<f32>,
}

impl From<ExternalVerdictInputWire> for ExternalVerdictInput {
    fn from(w: ExternalVerdictInputWire) -> Self {
        Self {
            file_path: w.file_path,
            finding_title: w.finding_title,
            finding_category: w.finding_category,
            verdict: w.verdict,
            reason: w.reason,
            agent: w.agent,
            agent_model: w.agent_model,
            confidence: w.confidence,
        }
    }
}

/// Rename `src` to `dst`. Returns `Ok(true)` on success, `Ok(false)` if the
/// source disappeared between enumeration and rename (benign multi-process
/// race — another process claimed it first). Any other IO error propagates.
/// Extracted so the ENOENT arm is directly unit-testable.
pub(crate) fn rename_or_tolerate_race(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> std::io::Result<bool> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Issue #101: NotFound is only the "another process already
            // moved it" race signal when the source has actually vanished.
            // If src is still present, the NotFound came from somewhere
            // else (missing dst parent, missing intermediate dir, etc.) —
            // propagate so the misconfiguration surfaces.
            if src.exists() {
                Err(e)
            } else {
                Ok(false)
            }
        }
        Err(e) => Err(e),
    }
}

/// Maximum bytes read from a single inbox file. External agents have no
/// reason to drop multi-MB feedback; cap protects against symlink-to-/dev/zero
/// and runaway file growth. Mirrors `MAX_RULE_FILE_BYTES` in `src/ast_grep.rs`
/// (#120).
const MAX_INBOX_FILE_BYTES: u64 = 1024 * 1024;

/// Classify an inbox entry via `symlink_metadata` (portable; does not follow
/// symlinks on Unix or Windows). Returns `Ok(())` for small regular files,
/// `Err(reason)` otherwise. The caller surfaces the reason to operators via
/// `DrainError`. Symlinks, FIFOs, sockets, directories, and oversized files
/// are all rejected. Mirrors the inline-cfg pattern of `read_rule_file` in
/// `src/ast_grep.rs` (#120).
fn classify_inbox_entry(path: &std::path::Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| format!("stat failed: {e}"))?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        return Err("symlink".into());
    }
    if !ft.is_file() {
        return Err("not a regular file".into());
    }
    if meta.len() > MAX_INBOX_FILE_BYTES {
        return Err(format!(
            "size {} exceeds cap {MAX_INBOX_FILE_BYTES}",
            meta.len()
        ));
    }
    Ok(())
}

/// Open an inbox file safely. On Unix, applies `O_NOFOLLOW` (refuse symlinks
/// at the syscall boundary — defends against TOCTOU between iteration-time
/// classify and the post-rename read) plus `O_NONBLOCK` (so a FIFO at this
/// path errors `EWOULDBLOCK` instead of hanging the drain loop). Always
/// validates regular-file via fstat after open, caps size, and reads via
/// `.take(MAX+1)` to defend against inodes that lie about size
/// (proc/sysfs/network FS). On non-Unix, the OS-level symlink/FIFO guards
/// are unavailable, so the post-classify TOCTOU window is wider; the
/// portable `is_file()` + size-cap checks still bound the damage.
fn read_inbox_file(path: &std::path::Path) -> std::io::Result<String> {
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
    let file = opts.open(path)?;

    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        // FIFO, socket, char/block device, or (on non-Unix) a symlink target.
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    if meta.len() > MAX_INBOX_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("exceeds size cap of {MAX_INBOX_FILE_BYTES} bytes"),
        ));
    }
    let mut buf = String::new();
    file.take(MAX_INBOX_FILE_BYTES + 1).read_to_string(&mut buf)?;
    if buf.len() as u64 > MAX_INBOX_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "exceeds size cap during read",
        ));
    }
    Ok(buf)
}

/// Per-call summary of `load_all_with_stats` (issue #92).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct LoadStats {
    /// Number of valid entries returned.
    pub kept: usize,
    /// Number of unparseable lines skipped (corruption / schema regression).
    pub skipped: usize,
}

pub struct FeedbackStore {
    path: PathBuf,
}

impl FeedbackStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Read-only access to the path this store was constructed with. Used by
    /// `mcp::handler::QuorumHandler` so the read side (precedent loading) and
    /// the write side (pipeline-level recordings via `PipelineConfig`) stay
    /// pinned to the same file (issue #93).
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn record(&self, entry: &FeedbackEntry) -> anyhow::Result<()> {
        use anyhow::Context;
        use fs2::FileExt;
        use std::io::Write;
        // Issue #100: OpenOptions::create(true) only creates the file, not
        // its parent. Direct callers (tests, daemon, future paths) that bypass
        // run_feedback's pre-create step would otherwise hit ENOENT.
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create feedback parent dir: {}", parent.display())
            })?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("Failed to open feedback file: {}", self.path.display()))?;
        // Issue #91: take an advisory exclusive lock around the append. POSIX
        // O_APPEND atomicity covers single-syscall writes today, but write_all
        // can issue multiple syscalls under partial-write conditions, and a
        // future refactor could introduce buffering that breaks per-line
        // atomicity. The lock is portable (POSIX flock + Windows LockFileEx
        // via fs2) and cheap. Released by closing the file when this function
        // returns; explicit unlock makes the intent obvious and gives us a
        // chance to surface unlock failures (rare but possible on NFS).
        FileExt::lock_exclusive(&file)
            .with_context(|| format!("Failed to lock feedback file: {}", self.path.display()))?;
        let mut buf = serde_json::to_string(entry)?;
        buf.push('\n');
        let write_result = file.write_all(buf.as_bytes());
        // Always attempt unlock, even if the write failed. Ignore unlock
        // errors when the write itself errored — the original error is more
        // informative.
        let unlock_result = FileExt::unlock(&file);
        write_result?;
        unlock_result.with_context(|| {
            format!("Failed to unlock feedback file: {}", self.path.display())
        })?;
        Ok(())
    }

    pub fn load_all(&self) -> anyhow::Result<Vec<FeedbackEntry>> {
        let (entries, stats) = self.load_all_with_stats()?;
        // Issue #92: surface malformed-row counts so corruption stops being
        // invisible. The dashboard calls `count()` / `load_all()` heavily;
        // emitting one warn per call is acceptable noise — corruption is
        // typically rare and operators want to see it.
        if stats.skipped > 0 {
            tracing::warn!(
                target: "quorum::feedback",
                kept = stats.kept,
                skipped = stats.skipped,
                path = %self.path.display(),
                "feedback log contained {} unparseable line(s); skipping. Run `quorum feedback verify` to inspect.",
                stats.skipped
            );
        }
        Ok(entries)
    }

    /// Load feedback entries and return per-line parse statistics.
    ///
    /// Issue #92: kept `pub(crate)` because the only callers are tests and
    /// future stats-health surfacing within this crate. The public API
    /// remains `load_all` (entries) + the structured `tracing::warn!` event.
    ///
    /// Acquires a shared advisory lock to pair with the exclusive lock taken
    /// by `record()` (issue #91). Without this, a reader can see a partial
    /// line that is mid-append and silently skip it as malformed —
    /// reintroducing the same observability gap #92 closed for completed
    /// writes. Quorum self-review on the v0.17.1 hardening branch.
    pub(crate) fn load_all_with_stats(&self) -> anyhow::Result<(Vec<FeedbackEntry>, LoadStats)> {
        use anyhow::Context;
        use fs2::FileExt;
        use std::io::Read;
        let mut file = match std::fs::OpenOptions::new().read(true).open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((vec![], LoadStats::default()));
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("Failed to open feedback file: {}", self.path.display())
                });
            }
        };
        FileExt::lock_shared(&file).with_context(|| {
            format!("Failed to lock feedback file for read: {}", self.path.display())
        })?;
        let mut content = String::new();
        let read_result = file.read_to_string(&mut content);
        let unlock_result = FileExt::unlock(&file);
        read_result.with_context(|| {
            format!("Failed to read feedback file: {}", self.path.display())
        })?;
        unlock_result.with_context(|| {
            format!("Failed to unlock feedback file: {}", self.path.display())
        })?;
        let mut entries = Vec::new();
        let mut skipped = 0usize;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(line) {
                Ok(entry) => entries.push(entry),
                Err(_) => skipped += 1,
            }
        }
        let kept = entries.len();
        Ok((entries, LoadStats { kept, skipped }))
    }

    pub fn query_by_verdict(&self, verdict: &Verdict) -> anyhow::Result<Vec<FeedbackEntry>> {
        Ok(self
            .load_all()?
            .into_iter()
            .filter(|e| e.verdict == *verdict)
            .collect())
    }

    pub fn count(&self) -> anyhow::Result<usize> {
        Ok(self.load_all()?.len())
    }

    /// Drain all `*.jsonl` files from `inbox_dir` into this store as External
    /// verdicts. Claim-then-ingest: atomically rename each file into an
    /// `<inbox>/processing/` claim-path FIRST (making ownership exclusive so
    /// concurrent quorum processes don't double-ingest), then parse and
    /// `record_external` each line, then archive to `<processed_dir>/` with
    /// a ULID suffix. Non-ENOENT errors leave the file in `processing/` for
    /// operator inspection — never silent duplicate writes (quorum self-review
    /// 2026-04-24).
    ///
    /// Malformed lines land in `DrainReport.errors` and are skipped; the rest
    /// of the file still drains. ENOENT on any rename is treated as a benign
    /// race (another process got it first).
    ///
    /// `processed_dir_total_bytes` in the returned report is cumulative
    /// (drives the 50MB warning threshold).
    ///
    /// **Trust boundary (2026-04-29, mirrors #120):** the inbox dir is
    /// writable by any local process (compromised dependency, IDE plugin,
    /// supply-chain actor). Files are classified at iteration time via
    /// `symlink_metadata` — symlinks, FIFOs, sockets, directories, and
    /// oversized files (>1 MiB) are rejected with a `report.errors`
    /// entry prefixed `rejected:` and **left in `inbox/`** (fail-closed;
    /// never silently flow into `processing/` or `processed/`). Reads use
    /// `O_NOFOLLOW | O_NONBLOCK` as defense-in-depth against TOCTOU.
    pub fn drain_inbox(
        &self,
        inbox_dir: &std::path::Path,
        processed_dir: &std::path::Path,
    ) -> anyhow::Result<DrainReport> {
        use std::io::ErrorKind;
        let mut report = DrainReport::default();

        // Fast path: ENOENT → zero work. Empty dir yields an empty iterator.
        let read = match std::fs::read_dir(inbox_dir) {
            Ok(r) => r,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(report),
            Err(e) => return Err(e.into()),
        };

        // Issue #103: surface per-entry iteration errors via report.errors
        // instead of silently filter-mapping them to nothing. A permission
        // glitch on one entry must not invisibly strand subsequent ingestion.
        let (entries, iter_errors) =
            drain_inbox_entries(read.map(|r| r.map(|e| e.path())));
        report.errors.extend(iter_errors);
        let candidates: Vec<PathBuf> = entries
            .into_iter()
            .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
            .collect();

        // Classify each candidate via symlink_metadata. Reject (with a
        // report.errors entry) anything that isn't a small regular file.
        // Rejection happens BEFORE the claim-rename, so rejected files
        // stay in inbox/ for operator inspection — they never reach
        // processing/. Mirrors the #120 architecture for ast_grep rules.
        let mut files: Vec<PathBuf> = Vec::new();
        for p in candidates {
            match classify_inbox_entry(&p) {
                Ok(()) => files.push(p),
                Err(reason) => {
                    report.errors.push(DrainError {
                        file: p,
                        line: 0,
                        message: format!("rejected: {reason}"),
                    });
                }
            }
        }
        files.sort();

        if files.is_empty() {
            return Ok(report);
        }

        // Claim-then-ingest: create processing/ (sibling of files in inbox)
        // and processed/ lazily.
        let processing_dir = inbox_dir.join("processing");
        std::fs::create_dir_all(&processing_dir)?;
        std::fs::create_dir_all(processed_dir)?;

        for file in files {
            // STEP A: CLAIM. Atomic rename into processing/. ENOENT → another
            // process already claimed it; skip.
            let fname = file.file_name().and_then(|n| n.to_str()).unwrap_or("unknown.jsonl");
            let claim_ulid = ulid::Ulid::new().to_string();
            let claimed = processing_dir.join(format!("{fname}.{claim_ulid}.jsonl"));
            match rename_or_tolerate_race(&file, &claimed) {
                Ok(true) => { /* we exclusively own the file now */ }
                Ok(false) => continue,
                Err(e) => {
                    report.errors.push(DrainError {
                        file: file.clone(),
                        line: 0,
                        message: format!("claim rename failed: {e}"),
                    });
                    continue;
                }
            }

            // STEP B: INGEST from the claimed path. read_inbox_file uses
            // O_NOFOLLOW|O_NONBLOCK + size cap as defense-in-depth against
            // TOCTOU between iteration-time classify and now.
            let content = match read_inbox_file(&claimed) {
                Ok(c) => c,
                Err(e) => {
                    report.errors.push(DrainError {
                        file: claimed.clone(),
                        line: 0,
                        message: format!("read failed: {e}"),
                    });
                    // Leave in processing/ for operator inspection.
                    continue;
                }
            };
            for (idx, line) in content.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<ExternalVerdictInputWire>(line) {
                    Ok(wire) => {
                        let input: ExternalVerdictInput = wire.into();
                        if let Err(e) = self.record_external(input) {
                            report.errors.push(DrainError {
                                file: claimed.clone(),
                                line: idx + 1,
                                message: format!("record failed: {e}"),
                            });
                        } else {
                            report.entries += 1;
                        }
                    }
                    Err(e) => {
                        report.errors.push(DrainError {
                            file: claimed.clone(),
                            line: idx + 1,
                            message: format!("parse failed: {e}"),
                        });
                    }
                }
            }

            // STEP C: ARCHIVE. Move from processing/ to processed/.
            let archive_ulid = ulid::Ulid::new().to_string();
            let target = processed_dir.join(format!("{fname}.{archive_ulid}.jsonl"));
            match rename_or_tolerate_race(&claimed, &target) {
                Ok(true) => report.drained_files += 1,
                Ok(false) => { /* unlikely — someone unlinked processing/ under us */ }
                Err(e) => {
                    report.errors.push(DrainError {
                        file: claimed.clone(),
                        line: 0,
                        message: format!(
                            "archive rename failed: {e}; file left in processing/"
                        ),
                    });
                    // File stays in processing/ — operator resolves manually.
                }
            }
        }

        // Size-warning threshold (cumulative total of processed_dir). Only
        // walk the directory when this drain actually archived something —
        // a no-op drain shouldn't pay an O(processed_files) syscall cost on
        // every `quorum stats`/`review` invocation.
        if report.drained_files > 0 {
            const WARN_BYTES: u64 = 50 * 1024 * 1024;
            if let Ok(entries) = std::fs::read_dir(processed_dir) {
                // Issue #103 asymmetry: this site deliberately keeps
                // `filter_map(|e| e.ok())`. The size warning is best-effort —
                // the cost of reporting iteration / metadata errors here is
                // operator noise on a cosmetic counter (under-reporting bytes
                // by one entry), not data loss. The drain-listing site above
                // has different stakes (a stranded file blocks all subsequent
                // ingestion), which is why the helper extraction lives there.
                let total: u64 = entries
                    .filter_map(|e| e.ok())
                    .filter_map(|e| e.metadata().ok())
                    .map(|m| m.len())
                    .sum();
                report.processed_dir_total_bytes = total;
                if total > WARN_BYTES {
                    tracing::warn!(
                        processed_dir = %processed_dir.display(),
                        total_mb = total / 1024 / 1024,
                        "quorum inbox processed/ is large; consider manual cleanup"
                    );
                }
            }
        }

        Ok(report)
    }

    /// Record a verdict from an external review agent (pal, third-opinion, etc.).
    /// Normalizes `agent` (trim + lowercase), NaN-safe clamps `confidence` to
    /// [0,1], defaults missing `finding_category` to `"unknown"`, rejects
    /// `Verdict::ContextMisleading` (retrieval signals need blamed_chunk_ids
    /// an external agent can't credibly produce), and sets
    /// `provenance = Provenance::External {..}`. See issue #32.
    pub fn record_external(&self, input: ExternalVerdictInput) -> anyhow::Result<()> {
        // Trust-boundary: External may only record tp/fp/partial.
        // - ContextMisleading needs blamed_chunk_ids tied to our injected
        //   context, which an external agent cannot credibly produce.
        // - Wontfix is an accepted-debt verdict; that judgment belongs to
        //   the project owner, not a third-party reviewer.
        // The single guard here is the chokepoint for all three ingestion
        // paths (CLI / MCP / inbox drain) so the policy stays uniform.
        if matches!(input.verdict, Verdict::ContextMisleading { .. }) {
            anyhow::bail!(
                "context_misleading verdicts are not accepted from External agents \
                 (they cannot identify blamed chunks in our injected context)"
            );
        }
        if matches!(input.verdict, Verdict::Wontfix) {
            anyhow::bail!(
                "wontfix verdicts are not accepted from External agents \
                 (accepted-debt judgment belongs to the project owner)"
            );
        }
        let agent = normalize_agent(&input.agent)?;
        let confidence = clamp_confidence(input.confidence);
        let category = input
            .finding_category
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let entry = FeedbackEntry {
            file_path: input.file_path,
            finding_title: input.finding_title,
            finding_category: category,
            verdict: input.verdict,
            reason: input.reason,
            model: None,
            timestamp: Utc::now(),
            provenance: Provenance::External {
                agent,
                model: input.agent_model,
                confidence,
            },
        };
        self.record(&entry)
    }

    /// Record a `ContextMisleading` verdict — reviewer determined the injected
    /// retrieval context was wrong or misleading. `blamed_chunk_ids` may be
    /// empty; callers (e.g. the CLI in task 8.2) supply a sensible default.
    pub fn record_context_misleading(
        &self,
        file: impl Into<String>,
        finding_title: impl Into<String>,
        finding_category: impl Into<String>,
        blamed_chunk_ids: Vec<String>,
        reason: impl Into<String>,
    ) -> anyhow::Result<()> {
        let entry = FeedbackEntry {
            file_path: file.into(),
            finding_title: finding_title.into(),
            finding_category: finding_category.into(),
            verdict: Verdict::ContextMisleading { blamed_chunk_ids },
            reason: reason.into(),
            model: None,
            timestamp: Utc::now(),
            provenance: Provenance::Human,
        };
        self.record(&entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store() -> (FeedbackStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        (FeedbackStore::new(path), dir)
    }

    /// One well-formed `ExternalVerdictInputWire` line for inbox-hardening tests.
    /// Surrounding tests assert on rejection semantics, not content, so any
    /// valid wire shape works.
    fn valid_external_jsonl_line() -> String {
        serde_json::to_string(&serde_json::json!({
            "file_path": "src/a.rs",
            "finding_title": "Bug",
            "finding_category": "security",
            "verdict": "tp",
            "reason": "r",
            "agent": "pal",
            "agent_model": null,
            "confidence": null
        })).unwrap()
    }

    // Issue #93: handler tests need to inspect the path a handler-owned
    // store was constructed with, so PipelineConfig assembly can target the
    // same file. Pin the accessor's contract so the indirection used by
    // `handle_review` is stable.
    #[test]
    fn feedback_store_exposes_path_accessor() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("issue93-accessor.jsonl");
        let store = FeedbackStore::new(p.clone());
        assert_eq!(store.path(), p.as_path());
    }

    // --- Issue #103: drain_inbox surfaces read_dir iteration errors -------
    //
    // Pre-fix, `drain_inbox` used `read.filter_map(|e| e.ok())`, silently
    // dropping any I/O / permission error from `read_dir`. Combined with
    // the claim-then-ingest semantics, a single permission-denied entry
    // could strand all subsequent ingestion of that file forever with no
    // observable signal.
    //
    // The fix extracts a `drain_inbox_entries` helper that returns errors
    // alongside successful paths, so they can be folded into
    // `DrainReport.errors`. The helper takes `Iterator<Item = io::Result<PathBuf>>`
    // (not `DirEntry`) so tests can inject synthetic Err values — DirEntry
    // has a private constructor.

    #[test]
    fn drain_inbox_entries_surfaces_iterator_errors() {
        use std::io;
        let entries: Vec<io::Result<PathBuf>> = vec![
            Ok(PathBuf::from("/tmp/a.jsonl")),
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "EACCES")),
            Ok(PathBuf::from("/tmp/b.jsonl")),
        ];
        let (paths, errors) = drain_inbox_entries(entries);
        assert_eq!(
            paths,
            vec![PathBuf::from("/tmp/a.jsonl"), PathBuf::from("/tmp/b.jsonl")],
            "ok paths must still be drained"
        );
        assert_eq!(errors.len(), 1, "iteration error must be surfaced; got: {:?}", errors);
        assert_eq!(errors[0].line, 0, "iteration errors are file-level (line 0)");
    }

    #[test]
    fn drain_inbox_entries_returns_paths_in_iterator_order() {
        // Sanity: the helper does not reorder. (Sorting by name is the
        // production caller's job, AFTER extension filtering.)
        use std::io;
        let entries: Vec<io::Result<PathBuf>> = vec![
            Ok(PathBuf::from("/tmp/z.jsonl")),
            Ok(PathBuf::from("/tmp/a.jsonl")),
        ];
        let (paths, errors) = drain_inbox_entries(entries);
        assert!(errors.is_empty());
        assert_eq!(
            paths,
            vec![PathBuf::from("/tmp/z.jsonl"), PathBuf::from("/tmp/a.jsonl")],
        );
    }

    fn sample_entry(verdict: Verdict) -> FeedbackEntry {
        FeedbackEntry {
            file_path: "src/auth.rs".into(),
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            verdict,
            reason: "Fixed with parameterized query".into(),
            model: Some("gpt-5.4".into()),
            timestamp: Utc::now(),
            provenance: Provenance::Unknown,
        }
    }

    #[test]
    fn record_and_load_single_entry() {
        let (store, _dir) = test_store();
        let entry = sample_entry(Verdict::Tp);
        store.record(&entry).unwrap();
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].finding_title, "SQL injection");
        assert_eq!(all[0].verdict, Verdict::Tp);
    }

    #[test]
    fn record_multiple_entries() {
        let (store, _dir) = test_store();
        store.record(&sample_entry(Verdict::Tp)).unwrap();
        store.record(&sample_entry(Verdict::Fp)).unwrap();
        store.record(&sample_entry(Verdict::Partial)).unwrap();
        assert_eq!(store.count().unwrap(), 3);
    }

    #[test]
    fn query_by_verdict() {
        let (store, _dir) = test_store();
        store.record(&sample_entry(Verdict::Tp)).unwrap();
        store.record(&sample_entry(Verdict::Fp)).unwrap();
        store.record(&sample_entry(Verdict::Tp)).unwrap();
        let tps = store.query_by_verdict(&Verdict::Tp).unwrap();
        assert_eq!(tps.len(), 2);
        let fps = store.query_by_verdict(&Verdict::Fp).unwrap();
        assert_eq!(fps.len(), 1);
    }

    #[test]
    fn load_empty_file() {
        let (store, _dir) = test_store();
        let all = store.load_all().unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn count_empty() {
        let (store, _dir) = test_store();
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn entries_persist_across_instances() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");

        let store1 = FeedbackStore::new(path.clone());
        store1.record(&sample_entry(Verdict::Tp)).unwrap();
        drop(store1);

        let store2 = FeedbackStore::new(path);
        assert_eq!(store2.count().unwrap(), 1);
    }

    #[test]
    fn feedback_entry_has_provenance_field() {
        let entry = FeedbackEntry {
            file_path: "test.rs".into(),
            finding_title: "Bug".into(),
            finding_category: "security".into(),
            verdict: Verdict::Tp,
            reason: "Real bug".into(),
            model: Some("gpt-5.4".into()),
            timestamp: Utc::now(),
            provenance: Provenance::Human,
        };
        assert_eq!(entry.provenance, Provenance::Human);
    }

    // Issue #100: FeedbackStore::record must create the parent directory if it
    // doesn't exist. Direct callers (tests, daemon, future entry points) that
    // bypass `run_feedback`'s pre-create step would otherwise hit ENOENT on a
    // fresh install or alternate QUORUM_HOME.
    #[test]
    fn record_creates_missing_parent_directory() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("missing").join("nested").join("feedback.jsonl");
        assert!(
            !path.parent().unwrap().exists(),
            "precondition: parent dir must not pre-exist"
        );
        let store = FeedbackStore::new(path.clone());
        store
            .record(&sample_entry(Verdict::Tp))
            .expect("record must succeed even if parent dir is missing");
        assert!(path.exists(), "feedback file must be created");
        assert_eq!(store.load_all().unwrap().len(), 1);
    }

    #[test]
    fn record_appends_without_truncating() {
        let (store, _dir) = test_store();
        store.record(&sample_entry(Verdict::Tp)).unwrap();
        store.record(&sample_entry(Verdict::Fp)).unwrap();
        assert_eq!(store.load_all().unwrap().len(), 2);
    }

    #[test]
    fn record_returns_err_on_unwritable_parent() {
        let dir = TempDir::new().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"i am a file not a dir").unwrap();
        let path = blocker.join("subdir").join("feedback.jsonl");
        let store = FeedbackStore::new(path);
        let err = store
            .record(&sample_entry(Verdict::Tp))
            .expect_err("record must fail when parent cannot be created");
        let has_io_cause = err.chain().any(|e| e.downcast_ref::<std::io::Error>().is_some());
        assert!(
            has_io_cause,
            "error chain must include io::Error (got: {err:#})"
        );
    }

    #[test]
    fn legacy_entries_without_provenance_default_to_unknown() {
        let json = r#"{"file_path":"test.rs","finding_title":"Bug","finding_category":"security","verdict":"tp","reason":"test","model":"gpt-5.4","timestamp":"2026-01-01T00:00:00Z"}"#;
        let entry: FeedbackEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.provenance, Provenance::Unknown);
    }

    #[test]
    fn provenance_serializes_correctly() {
        assert_eq!(serde_json::to_value(&Provenance::Human).unwrap(), "human");
        assert_eq!(serde_json::to_value(&Provenance::PostFix).unwrap(), "post_fix");
    }

    #[test]
    fn verdict_serializes_as_lowercase() {
        assert_eq!(serde_json::to_value(&Verdict::Tp).unwrap(), "tp");
        assert_eq!(serde_json::to_value(&Verdict::Fp).unwrap(), "fp");
        assert_eq!(serde_json::to_value(&Verdict::Partial).unwrap(), "partial");
        assert_eq!(serde_json::to_value(&Verdict::Wontfix).unwrap(), "wontfix");
    }

    #[test]
    fn feedback_records_context_misleading_with_chunk_ids() {
        let (store, _dir) = test_store();
        store
            .record_context_misleading(
                "src/retriever.rs",
                "Stale API reference",
                "correctness",
                vec!["chunk-abc".into(), "chunk-def".into()],
                "Injected docs described v1, code uses v2",
            )
            .unwrap();
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        match &all[0].verdict {
            Verdict::ContextMisleading { blamed_chunk_ids } => {
                assert_eq!(blamed_chunk_ids, &vec!["chunk-abc".to_string(), "chunk-def".to_string()]);
            }
            other => panic!("expected ContextMisleading, got {:?}", other),
        }
        assert_eq!(all[0].file_path, "src/retriever.rs");
        assert_eq!(all[0].finding_title, "Stale API reference");
        assert_eq!(
            all[0].finding_category, "correctness",
            "finding_category must round-trip, not be hardcoded empty"
        );
        assert_eq!(all[0].provenance, Provenance::Human);
    }

    #[test]
    fn legacy_verdicts_still_load_after_adding_context_misleading() {
        // Entries written before the ContextMisleading variant existed must still parse.
        let legacy = r#"{"file_path":"a.rs","finding_title":"X","finding_category":"security","verdict":"tp","reason":"r","model":"m","timestamp":"2026-01-01T00:00:00Z","provenance":"human"}
{"file_path":"b.rs","finding_title":"Y","finding_category":"style","verdict":"fp","reason":"r","model":"m","timestamp":"2026-01-02T00:00:00Z"}
{"file_path":"c.rs","finding_title":"Z","finding_category":"security","verdict":"partial","reason":"r","model":"m","timestamp":"2026-01-03T00:00:00Z","provenance":"post_fix"}
{"file_path":"d.rs","finding_title":"W","finding_category":"style","verdict":"wontfix","reason":"r","model":"m","timestamp":"2026-01-04T00:00:00Z","provenance":"human"}
"#;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        std::fs::write(&path, legacy).unwrap();
        let store = FeedbackStore::new(path);
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(all[0].verdict, Verdict::Tp);
        assert_eq!(all[1].verdict, Verdict::Fp);
        assert_eq!(all[2].verdict, Verdict::Partial);
        assert_eq!(all[3].verdict, Verdict::Wontfix);
    }

    #[test]
    fn context_misleading_with_empty_chunk_ids_roundtrips() {
        let (store, _dir) = test_store();
        store
            .record_context_misleading(
                "src/foo.rs",
                "No chunks blamed",
                "",
                vec![],
                "Reviewer did not identify specific chunks",
            )
            .unwrap();
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        match &all[0].verdict {
            Verdict::ContextMisleading { blamed_chunk_ids } => {
                assert!(blamed_chunk_ids.is_empty());
            }
            other => panic!("expected ContextMisleading, got {:?}", other),
        }
    }

    // --- Task 1: External provenance variant (issue #32) ---

    #[test]
    fn external_variant_roundtrips_through_jsonl() {
        let (store, _dir) = test_store();
        let entry = FeedbackEntry {
            file_path: "src/auth.rs".into(),
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            verdict: Verdict::Tp,
            reason: "Confirmed".into(),
            model: None,
            timestamp: Utc::now(),
            provenance: Provenance::External {
                agent: "pal".into(),
                model: Some("gemini-3-pro-preview".into()),
                confidence: Some(0.9),
            },
        };
        store.record(&entry).unwrap();
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        match &all[0].provenance {
            Provenance::External { agent, model, confidence } => {
                assert_eq!(agent, "pal");
                assert_eq!(model.as_deref(), Some("gemini-3-pro-preview"));
                assert_eq!(*confidence, Some(0.9));
            }
            other => panic!("expected External, got {:?}", other),
        }
    }

    #[test]
    fn external_serializes_with_external_tag() {
        let p = Provenance::External {
            agent: "pal".into(),
            model: Some("gpt-5.4".into()),
            confidence: None,
        };
        let v = serde_json::to_value(&p).unwrap();
        // Externally tagged: {"external": {...}}
        assert!(v.get("external").is_some(), "expected 'external' tag, got {v}");
        let inner = v.get("external").unwrap();
        assert_eq!(inner.get("agent").and_then(|x| x.as_str()), Some("pal"));
        // `confidence: None` may serialize as `null` OR be absent (if serde is
        // later configured with skip_serializing_if). Both are valid wire forms.
        assert!(inner.get("confidence").map_or(true, |c| c.is_null()),
            "confidence must be null or absent, got {:?}", inner.get("confidence"));
    }

    #[test]
    fn external_deserializes_when_confidence_key_absent() {
        // Agents may omit the confidence key entirely. Must round-trip to
        // Provenance::External { confidence: None, .. }.
        let json = r#"{"external":{"agent":"pal","model":"gpt-5.4"}}"#;
        let p: Provenance = serde_json::from_str(json).unwrap();
        match p {
            Provenance::External { agent, model, confidence } => {
                assert_eq!(agent, "pal");
                assert_eq!(model.as_deref(), Some("gpt-5.4"));
                assert_eq!(confidence, None);
            }
            o => panic!("{o:?}"),
        }
    }

    // --- Task 4: ExternalVerdictInput + record_external (issue #32) ---

    #[test]
    fn clamp_confidence_maps_values() {
        assert_eq!(clamp_confidence(None), None);
        assert_eq!(clamp_confidence(Some(0.42)), Some(0.42));
        assert_eq!(clamp_confidence(Some(1.5)), Some(1.0));
        assert_eq!(clamp_confidence(Some(-0.2)), Some(0.0));
        assert_eq!(clamp_confidence(Some(0.0)), Some(0.0));
        assert_eq!(clamp_confidence(Some(1.0)), Some(1.0));
    }

    #[test]
    fn clamp_confidence_rejects_nan_inf() {
        // f32::clamp is NOT NaN-safe — it returns NaN for NaN input.
        // clamp_confidence must detect non-finite values explicitly.
        assert_eq!(clamp_confidence(Some(f32::NAN)), None, "NaN must become None");
        assert_eq!(clamp_confidence(Some(f32::INFINITY)), None, "+inf must become None");
        assert_eq!(clamp_confidence(Some(f32::NEG_INFINITY)), None, "-inf must become None");
    }

    #[test]
    fn record_external_writes_external_provenance() {
        let (store, _dir) = test_store();
        let input = ExternalVerdictInput {
            file_path: "src/a.rs".into(),
            finding_title: "Bug".into(),
            finding_category: Some("security".into()),
            verdict: Verdict::Tp,
            reason: "confirmed".into(),
            agent: "pal".into(),
            agent_model: Some("gemini-3-pro-preview".into()),
            confidence: Some(0.85),
        };
        store.record_external(input).unwrap();
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        match &all[0].provenance {
            Provenance::External { agent, model, confidence } => {
                assert_eq!(agent, "pal");
                assert_eq!(model.as_deref(), Some("gemini-3-pro-preview"));
                assert_eq!(*confidence, Some(0.85));
            }
            o => panic!("expected External, got {o:?}"),
        }
        assert!(all[0].model.is_none(), "entry.model should be None (reviewer model, not agent model)");
    }

    #[test]
    fn record_external_normalizes_agent_name() {
        let (store, _dir) = test_store();
        store.record_external(ExternalVerdictInput {
            file_path: "a.rs".into(),
            finding_title: "t".into(),
            finding_category: None,
            verdict: Verdict::Tp,
            reason: "r".into(),
            agent: "  PaL  ".into(),
            agent_model: None,
            confidence: None,
        }).unwrap();
        let all = store.load_all().unwrap();
        match &all[0].provenance {
            Provenance::External { agent, .. } => assert_eq!(agent, "pal"),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn record_external_rejects_empty_agent() {
        let (store, _dir) = test_store();
        let err = store.record_external(ExternalVerdictInput {
            file_path: "a.rs".into(),
            finding_title: "t".into(),
            finding_category: None,
            verdict: Verdict::Tp,
            reason: "r".into(),
            agent: "   ".into(),
            agent_model: None,
            confidence: None,
        }).expect_err("empty agent must be rejected");
        assert!(err.to_string().to_lowercase().contains("agent"),
            "error message should mention agent: {err}");
    }

    #[test]
    fn record_external_defaults_missing_category_to_unknown() {
        let (store, _dir) = test_store();
        store.record_external(ExternalVerdictInput {
            file_path: "a.rs".into(),
            finding_title: "t".into(),
            finding_category: None,
            verdict: Verdict::Tp,
            reason: "r".into(),
            agent: "pal".into(),
            agent_model: None,
            confidence: None,
        }).unwrap();
        let all = store.load_all().unwrap();
        assert_eq!(all[0].finding_category, "unknown");
    }

    #[test]
    fn record_external_applies_clamp_confidence() {
        let (store, _dir) = test_store();
        store.record_external(ExternalVerdictInput {
            file_path: "a.rs".into(),
            finding_title: "t".into(),
            finding_category: None,
            verdict: Verdict::Tp,
            reason: "r".into(),
            agent: "pal".into(),
            agent_model: None,
            confidence: Some(1.5),
        }).unwrap();
        let all = store.load_all().unwrap();
        match &all[0].provenance {
            Provenance::External { confidence, .. } => {
                assert_eq!(*confidence, Some(1.0), "1.5 must clamp to 1.0");
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn record_external_rejects_context_misleading_verdict() {
        // ContextMisleading needs blamed_chunk_ids that the reviewer sees in our
        // injected context — an external agent can't credibly produce them.
        // Reject at ingest to avoid polluting the calibrator's retrieval signal.
        let (store, _dir) = test_store();
        let err = store.record_external(ExternalVerdictInput {
            file_path: "a.rs".into(),
            finding_title: "t".into(),
            finding_category: None,
            verdict: Verdict::ContextMisleading { blamed_chunk_ids: vec!["c1".into()] },
            reason: "r".into(),
            agent: "pal".into(),
            agent_model: None,
            confidence: None,
        }).expect_err("ContextMisleading must be rejected for External provenance");
        assert!(
            err.to_string().to_lowercase().contains("context"),
            "error message must mention context_misleading: {err}"
        );
    }

    #[test]
    fn provenance_external_frozen_row_round_trips() {
        // PIN: this hardcoded row was written by the external-feedback PR
        // (#95). Every future change to Provenance::External / FeedbackEntry
        // MUST keep this row deserializable. If you're modifying schema and
        // this test fails, add #[serde(alias = ...)] before deleting/renaming
        // a field, or accept the breakage explicitly with a migration plan.
        // Issue #98.
        let raw = r#"{"file_path":"src/x.rs","finding_title":"Bug","finding_category":"security","verdict":"tp","reason":"r","model":null,"timestamp":"2026-04-24T17:00:00Z","provenance":{"external":{"agent":"pal","model":"gpt-5.4","confidence":0.9}}}"#;
        let entry: FeedbackEntry =
            serde_json::from_str(raw).expect("frozen row must deserialize");
        match &entry.provenance {
            Provenance::External {
                agent,
                model,
                confidence,
            } => {
                assert_eq!(agent, "pal");
                assert_eq!(model.as_deref(), Some("gpt-5.4"));
                assert_eq!(*confidence, Some(0.9));
            }
            other => panic!("expected External provenance, got {other:?}"),
        }
        // Round-trip must re-serialize structurally-equivalent (key order
        // may differ; compare via Value).
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let serialized = serde_json::to_string(&entry).unwrap();
        let v2: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(v, v2, "frozen row must round-trip without drift");
    }

    #[test]
    fn provenance_external_frozen_row_minimal_optionals() {
        // Same pin as above but with optional model/confidence omitted.
        // Tests that #[serde(default)] on those fields holds across versions.
        let raw = r#"{"file_path":"a.rs","finding_title":"t","finding_category":"unknown","verdict":"fp","reason":"r","timestamp":"2026-04-24T17:00:00Z","provenance":{"external":{"agent":"third-opinion"}}}"#;
        let entry: FeedbackEntry = serde_json::from_str(raw)
            .expect("frozen minimal-optional row must deserialize");
        match &entry.provenance {
            Provenance::External {
                agent,
                model,
                confidence,
            } => {
                assert_eq!(agent, "third-opinion");
                assert!(model.is_none());
                assert!(confidence.is_none());
            }
            other => panic!("expected External provenance, got {other:?}"),
        }
    }

    #[test]
    fn record_external_rejects_wontfix_verdict() {
        // External agents cannot mark findings as wontfix — that's an
        // accepted-debt verdict that requires human / project-owner judgment.
        // Trust-boundary policy: External may record tp/fp/partial only.
        let (store, _dir) = test_store();
        let err = store
            .record_external(ExternalVerdictInput {
                file_path: "a.rs".into(),
                finding_title: "t".into(),
                finding_category: None,
                verdict: Verdict::Wontfix,
                reason: "r".into(),
                agent: "pal".into(),
                agent_model: None,
                confidence: None,
            })
            .expect_err("Wontfix must be rejected for External provenance");
        assert!(
            err.to_string().to_lowercase().contains("wontfix"),
            "error must mention wontfix: {err}"
        );
    }

    #[test]
    fn normalize_agent_idempotent_on_typical_input() {
        assert_eq!(normalize_agent("pal").unwrap(), "pal");
        assert_eq!(normalize_agent("  Pal  ").unwrap(), "pal");
        assert_eq!(normalize_agent("THIRD-OPINION").unwrap(), "third-opinion");
        assert!(normalize_agent("").is_err());
        assert!(normalize_agent("   ").is_err());
        // Idempotence: normalize(normalize(s)) == normalize(s)
        let once = normalize_agent("  MixedCase  ").unwrap();
        let twice = normalize_agent(&once).unwrap();
        assert_eq!(once, twice);
    }

    // --- Task 5: drain_inbox with claim-then-ingest (issue #32) ---

    #[test]
    fn drain_inbox_missing_dir_returns_zero_work() {
        // Inbox dir doesn't exist — first-run scenario. Must NOT error.
        let dir = TempDir::new().unwrap();
        let inbox = dir.path().join("nonexistent-inbox");
        let processed = dir.path().join("processed");
        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let report = store.drain_inbox(&inbox, &processed).unwrap();
        assert_eq!(report.drained_files, 0);
        assert_eq!(report.entries, 0);
        assert!(report.errors.is_empty());
        assert!(!processed.exists(), "processed/ must not be created when inbox is absent");
    }

    #[test]
    fn drain_inbox_empty_returns_zero_work() {
        let dir = TempDir::new().unwrap();
        let inbox = dir.path().join("inbox");
        let processed = dir.path().join("processed");
        std::fs::create_dir_all(&inbox).unwrap();
        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let report = store.drain_inbox(&inbox, &processed).unwrap();
        assert_eq!(report.drained_files, 0);
        assert_eq!(report.entries, 0);
        assert!(report.errors.is_empty());
        assert_eq!(report.processed_dir_total_bytes, 0);
        assert!(!processed.exists(), "processed/ should not be created when inbox is empty");
    }

    #[test]
    fn drain_inbox_valid_file_appends_and_moves() {
        let dir = TempDir::new().unwrap();
        let inbox = dir.path().join("inbox");
        let processed = dir.path().join("processed");
        std::fs::create_dir_all(&inbox).unwrap();

        let line = serde_json::to_string(&serde_json::json!({
            "file_path": "src/a.rs",
            "finding_title": "Bug",
            "finding_category": "security",
            "verdict": "tp",
            "reason": "confirmed",
            "agent": "pal",
            "agent_model": "gemini-3-pro-preview",
            "confidence": 0.9
        })).unwrap();
        let inbox_file = inbox.join("pal-run-1.jsonl");
        std::fs::write(&inbox_file, format!("{line}\n")).unwrap();

        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let report = store.drain_inbox(&inbox, &processed).unwrap();
        assert_eq!(report.drained_files, 1);
        assert_eq!(report.entries, 1);
        assert!(report.errors.is_empty());

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert!(matches!(all[0].provenance, Provenance::External { .. }));

        assert!(!inbox_file.exists(), "inbox file should be moved after drain");

        let processed_files: Vec<_> = std::fs::read_dir(&processed).unwrap().collect::<Result<_,_>>().unwrap();
        assert_eq!(processed_files.len(), 1);
        let name = processed_files[0].file_name().into_string().unwrap();
        assert!(name.starts_with("pal-run-1.jsonl."), "expected ulid-suffixed name, got {name}");
        assert!(name.ends_with(".jsonl"));

        // Claim-then-ingest invariant: processing/ must be empty on success.
        let processing = inbox.join("processing");
        if processing.exists() {
            let leftover: Vec<_> = std::fs::read_dir(&processing).unwrap().collect::<Result<_,_>>().unwrap();
            assert!(leftover.is_empty(),
                "processing/ must be empty after successful drain, found {:?}",
                leftover.iter().map(|e| e.path()).collect::<Vec<_>>());
        }
    }

    #[test]
    fn drain_inbox_malformed_line_skipped_rest_drained() {
        let dir = TempDir::new().unwrap();
        let inbox = dir.path().join("inbox");
        let processed = dir.path().join("processed");
        std::fs::create_dir_all(&inbox).unwrap();

        let good = serde_json::to_string(&serde_json::json!({
            "file_path": "src/a.rs",
            "finding_title": "Bug",
            "finding_category": "security",
            "verdict": "tp",
            "reason": "r",
            "agent": "pal",
            "agent_model": null,
            "confidence": null
        })).unwrap();
        let bad = "{not json";
        std::fs::write(inbox.join("mix.jsonl"), format!("{good}\n{bad}\n{good}\n")).unwrap();

        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let report = store.drain_inbox(&inbox, &processed).unwrap();
        assert_eq!(report.drained_files, 1);
        assert_eq!(report.entries, 2, "2 good + 1 bad = 2 appended");
        assert_eq!(report.errors.len(), 1);

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn drain_inbox_is_idempotent_on_empty_second_call() {
        // Honest name: this tests idempotency, not multi-process ENOENT races.
        // ENOENT tolerance is tested directly via rename_or_tolerate_race below.
        let dir = TempDir::new().unwrap();
        let inbox = dir.path().join("inbox");
        let processed = dir.path().join("processed");
        std::fs::create_dir_all(&inbox).unwrap();

        let line = r#"{"file_path":"a.rs","finding_title":"t","finding_category":"c","verdict":"tp","reason":"r","agent":"pal","agent_model":null,"confidence":null}"#;
        std::fs::write(inbox.join("a.jsonl"), format!("{line}\n")).unwrap();

        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let r1 = store.drain_inbox(&inbox, &processed).unwrap();
        assert_eq!(r1.drained_files, 1);
        let r2 = store.drain_inbox(&inbox, &processed).unwrap();
        assert_eq!(r2.drained_files, 0, "second drain is a no-op, not an error");
    }

    // --- #120-class hardening for drain_inbox (2026-04-29) ---
    //
    // Mirrors the symlink + size-cap + non-regular-file guards shipped on
    // src/ast_grep.rs in #120. drain_inbox ingests JSONL from
    // ~/.quorum/inbox/, which is process-readable but writable by any local
    // process. A compromised dependency / IDE plugin / supply-chain actor
    // can drop a symlink, FIFO, or oversized file at this path. Pre-fix:
    //   * `!p.is_dir()` follows symlinks → symlink to /etc/passwd reads it
    //   * `read_to_string` on a FIFO blocks indefinitely → daemon hang
    //   * `read_to_string` on a 10 GiB file → OOM
    // Post-fix: layered guards (symlink_metadata iteration filter + O_NOFOLLOW|
    // O_NONBLOCK on open + size cap + .take(MAX+1) defensive bound). Rejected
    // files remain in inbox/ (fail-closed; never silently flow to processing/).

    #[test]
    #[cfg(unix)]
    fn drain_inbox_skips_symlinked_inbox_file() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let inbox = dir.path().join("inbox");
        let processed = dir.path().join("processed");
        std::fs::create_dir_all(&inbox).unwrap();

        let outside = dir.path().join("outside.jsonl");
        std::fs::write(&outside, format!("{}\n", valid_external_jsonl_line())).unwrap();

        let evil = inbox.join("evil.jsonl");
        symlink(&outside, &evil).unwrap();

        let report = store.drain_inbox(&inbox, &processed).unwrap();

        assert_eq!(report.entries, 0, "symlinked inbox file must not be ingested");
        assert!(
            report.errors.iter().any(|e| e.message.starts_with("rejected:")),
            "expected 'rejected: ...' error, got: {:?}",
            report.errors
        );
        assert!(evil.exists(), "symlink must remain in inbox/ (fail-closed)");
        assert!(!inbox.join("processing").join("evil.jsonl").exists());
        assert!(
            !processed.exists() || std::fs::read_dir(&processed).unwrap().next().is_none(),
            "rejected symlink must not flow into processed/"
        );
    }

    #[test]
    #[cfg(unix)]
    fn drain_inbox_rejects_oversized_file() {
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let inbox = dir.path().join("inbox");
        let processed = dir.path().join("processed");
        std::fs::create_dir_all(&inbox).unwrap();

        let huge = "x".repeat(2 * 1024 * 1024);
        std::fs::write(inbox.join("huge.jsonl"), huge).unwrap();

        let report = store.drain_inbox(&inbox, &processed).unwrap();

        assert_eq!(report.entries, 0);
        assert!(
            report.errors.iter().any(|e| e.message.starts_with("rejected:")),
            "expected 'rejected: ...' error, got: {:?}",
            report.errors
        );
        assert!(inbox.join("huge.jsonl").exists(), "oversized file must remain in inbox/ (fail-closed)");
    }

    #[test]
    #[cfg(unix)]
    fn drain_inbox_rejects_non_regular_file() {
        use std::os::unix::net::UnixListener;
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let inbox = dir.path().join("inbox");
        let processed = dir.path().join("processed");
        std::fs::create_dir_all(&inbox).unwrap();

        let sock_path = inbox.join("evil.jsonl");
        let _listener = UnixListener::bind(&sock_path).unwrap();

        let report = store.drain_inbox(&inbox, &processed).unwrap();

        assert_eq!(report.entries, 0);
        assert!(
            report.errors.iter().any(|e| e.message.starts_with("rejected:")),
            "expected 'rejected: ...' error for non-regular file, got: {:?}",
            report.errors
        );
        assert!(sock_path.exists(), "non-regular file must remain in inbox/ (fail-closed)");
    }

    #[test]
    #[cfg(unix)]
    fn drain_inbox_rejects_fifo_file() {
        use std::ffi::CString;
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let inbox = dir.path().join("inbox");
        let processed = dir.path().join("processed");
        std::fs::create_dir_all(&inbox).unwrap();

        let fifo_path = inbox.join("evil.jsonl");
        let cstr = CString::new(fifo_path.to_str().unwrap()).unwrap();
        let rc = unsafe { libc::mkfifo(cstr.as_ptr(), 0o644) };
        assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());

        let report = store.drain_inbox(&inbox, &processed).unwrap();

        assert_eq!(report.entries, 0);
        assert!(
            report.errors.iter().any(|e| e.message.starts_with("rejected:")),
            "expected 'rejected: ...' error for FIFO, got: {:?}",
            report.errors
        );
        assert!(fifo_path.exists(), "FIFO must remain in inbox/ (fail-closed)");
        assert!(!inbox.join("processing").join("evil.jsonl").exists());
    }

    #[test]
    #[cfg(unix)]
    fn drain_inbox_accepts_file_at_size_cap() {
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let inbox = dir.path().join("inbox");
        let processed = dir.path().join("processed");
        std::fs::create_dir_all(&inbox).unwrap();

        let line = valid_external_jsonl_line();
        let mut content = String::with_capacity(MAX_INBOX_FILE_BYTES as usize);
        content.push_str(&line);
        content.push('\n');
        while content.len() < MAX_INBOX_FILE_BYTES as usize {
            content.push('\n');
        }
        content.truncate(MAX_INBOX_FILE_BYTES as usize);
        assert_eq!(content.len() as u64, MAX_INBOX_FILE_BYTES);
        std::fs::write(inbox.join("at_cap.jsonl"), &content).unwrap();

        let report = store.drain_inbox(&inbox, &processed).unwrap();
        assert_eq!(report.entries, 1, "exactly-at-cap file must be accepted");
        assert!(report.errors.is_empty(), "no errors expected, got: {:?}", report.errors);
    }

    #[test]
    #[cfg(unix)]
    fn drain_inbox_rejects_file_one_byte_over_cap() {
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let inbox = dir.path().join("inbox");
        let processed = dir.path().join("processed");
        std::fs::create_dir_all(&inbox).unwrap();

        let huge = "x".repeat(MAX_INBOX_FILE_BYTES as usize + 1);
        std::fs::write(inbox.join("over.jsonl"), huge).unwrap();

        let report = store.drain_inbox(&inbox, &processed).unwrap();
        assert_eq!(report.entries, 0);
        assert!(
            report.errors.iter().any(|e| e.message.starts_with("rejected:")),
            "expected 'rejected: ...' error, got: {:?}",
            report.errors
        );
        assert!(inbox.join("over.jsonl").exists(), "off-by-one file must remain in inbox/ (fail-closed)");
    }

    #[test]
    fn drain_inbox_happy_path_unaffected_by_nofollow_helper() {
        // Distinct from drain_inbox_valid_file_appends_and_moves so a future
        // regression breaking normal ingestion lights up two tests, not one.
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let inbox = dir.path().join("inbox");
        let processed = dir.path().join("processed");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(inbox.join("ok.jsonl"), format!("{}\n", valid_external_jsonl_line())).unwrap();

        let report = store.drain_inbox(&inbox, &processed).unwrap();
        assert_eq!(report.entries, 1);
        assert!(report.errors.is_empty(), "no errors expected, got: {:?}", report.errors);
    }

    #[test]
    fn rename_or_tolerate_race_swallows_nonexistent_source() {
        // Directly tests ENOENT-tolerance by calling the extracted seam
        // with a source path that doesn't exist. Proves the multi-process
        // race arm of drain_inbox without requiring actual concurrency.
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("not-there.jsonl");
        std::fs::create_dir_all(dir.path().join("processed")).unwrap();
        let dst = dir.path().join("processed").join("moved.jsonl");
        let renamed = rename_or_tolerate_race(&missing, &dst).unwrap();
        assert!(!renamed, "missing source must return Ok(false), not Err");
        assert!(!dst.exists(), "destination must not be created");
    }

    #[test]
    fn concurrent_record_writes_do_not_interleave_or_corrupt() {
        // Issue #91: without an advisory file lock, two threads (or two
        // processes) calling `record()` against the same JSONL file can
        // interleave bytes. `write_all` is not atomic above PIPE_BUF
        // (typically 4096 bytes on Linux/macOS), so each entry's payload
        // must be padded past that threshold to reliably surface the bug
        // — sub-PIPE_BUF entries are atomic by O_APPEND semantics on most
        // POSIX kernels and would let the unfixed code pass.
        //
        // Reproduces the bug deterministically on macOS APFS and Linux
        // ext4/tmpfs at ~6 KB payloads.
        use std::sync::Arc;
        use std::thread;
        const THREADS: usize = 2;
        const PER_THREAD: usize = 30;
        const PAD_BYTES: usize = 6_000; // > PIPE_BUF
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let store = Arc::new(FeedbackStore::new(path.clone()));
        let barrier = Arc::new(std::sync::Barrier::new(THREADS));
        let handles: Vec<_> = (0..THREADS)
            .map(|tid| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    for i in 0..PER_THREAD {
                        let mut e = sample_entry(Verdict::Tp);
                        // Padding inside `reason` keeps the JSON valid;
                        // distinct chars per thread make truncation
                        // detectable in failure messages.
                        let pad: String = std::iter::repeat(if tid == 0 { 'A' } else { 'B' })
                            .take(PAD_BYTES)
                            .collect();
                        e.reason = format!("t{tid}-i{i}-{pad}");
                        store.record(&e).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();
        assert_eq!(
            lines.len(),
            THREADS * PER_THREAD,
            "all writes must produce exactly one line each (no truncation, no merging)"
        );
        for (i, line) in lines.iter().enumerate() {
            serde_json::from_str::<FeedbackEntry>(line).unwrap_or_else(|e| {
                let preview: String = line.chars().take(120).collect();
                panic!("line {i} did not round-trip — interleaved write? {e}: {preview}...")
            });
        }
    }

    #[test]
    fn load_all_with_stats_counts_kept_and_skipped() {
        // Issue #92: corruption / schema-regression / interleaved-write loss
        // must be observable. Mix valid + malformed lines and assert the
        // returned counts.
        let (store, _dir) = test_store();
        let valid = sample_entry(Verdict::Tp);
        let valid_line = serde_json::to_string(&valid).unwrap();
        // 3 valid lines, 2 malformed lines, 1 blank line (always ignored).
        let content = format!(
            "{valid_line}\n\
             {{this is not json\n\
             {valid_line}\n\
             \n\
             garbage\n\
             {valid_line}\n"
        );
        std::fs::write(&store.path, content).unwrap();
        let (entries, stats) = store.load_all_with_stats().unwrap();
        assert_eq!(entries.len(), 3, "valid entries returned");
        assert_eq!(stats.kept, 3);
        assert_eq!(
            stats.skipped, 2,
            "two malformed lines must be counted (blank line is not 'skipped')"
        );
    }

    #[test]
    fn load_all_with_stats_returns_zero_skipped_on_empty() {
        // Empty file: no skip, no warn. Regression guard for the no-op path.
        let (store, _dir) = test_store();
        std::fs::write(&store.path, "").unwrap();
        let (entries, stats) = store.load_all_with_stats().unwrap();
        assert!(entries.is_empty());
        assert_eq!(stats.kept, 0);
        assert_eq!(stats.skipped, 0);
    }

    #[test]
    fn load_all_still_returns_valid_entries_when_some_lines_malformed() {
        // Public API contract: load_all must continue to return the valid
        // entries even when load_all_with_stats reports skipped > 0.
        let (store, _dir) = test_store();
        let valid_line = serde_json::to_string(&sample_entry(Verdict::Tp)).unwrap();
        std::fs::write(
            &store.path,
            format!("{valid_line}\nnot json\n{valid_line}\n"),
        )
        .unwrap();
        let entries = store.load_all().unwrap();
        assert_eq!(entries.len(), 2, "load_all must skip-and-return, not bail");
    }

    #[test]
    fn rename_or_tolerate_race_propagates_err_when_destination_parent_missing() {
        // Issue #101: NotFound is only benign when the SOURCE has vanished
        // (another process already claimed it). When the source is right
        // there but the rename fails because the destination parent dir is
        // missing, the error must propagate — silent Ok(false) would let
        // a mis-configured drain hook fail invisibly.
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("present.jsonl");
        std::fs::write(&src, b"line\n").unwrap();
        // Destination parent does NOT exist (no create_dir_all).
        let dst = dir.path().join("missing-parent").join("moved.jsonl");
        assert!(!dst.parent().unwrap().exists(), "precondition");
        let result = rename_or_tolerate_race(&src, &dst);
        assert!(
            result.is_err(),
            "rename must propagate Err when src still exists; got {:?}",
            result
        );
        assert!(src.exists(), "src must remain in place after a failed rename");
    }

    #[test]
    fn drain_inbox_rejects_uppercase_verdict_string() {
        // Verdict must round-trip through #[serde(rename_all="snake_case")].
        // "TP" is not valid; the line lands in errors, valid line still drains.
        let dir = TempDir::new().unwrap();
        let inbox = dir.path().join("inbox");
        let processed = dir.path().join("processed");
        std::fs::create_dir_all(&inbox).unwrap();

        let bad = r#"{"file_path":"a.rs","finding_title":"t","finding_category":"c","verdict":"TP","reason":"r","agent":"pal","agent_model":null,"confidence":null}"#;
        let good = r#"{"file_path":"b.rs","finding_title":"t","finding_category":"c","verdict":"tp","reason":"r","agent":"pal","agent_model":null,"confidence":null}"#;
        std::fs::write(inbox.join("mix.jsonl"), format!("{bad}\n{good}\n")).unwrap();

        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let report = store.drain_inbox(&inbox, &processed).unwrap();
        assert_eq!(report.drained_files, 1);
        assert_eq!(report.entries, 1, "only the valid line was appended");
        assert_eq!(report.errors.len(), 1, "uppercase TP must land in errors");
        let msg = report.errors[0].message.to_lowercase();
        assert!(msg.contains("tp") || msg.contains("verdict") || msg.contains("unknown variant"),
            "error must reference the bad verdict: {}", report.errors[0].message);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn clamp_always_finite_and_in_unit_interval(c in any::<f32>()) {
            match clamp_confidence(Some(c)) {
                Some(out) => {
                    prop_assert!(out.is_finite(), "clamp output must be finite, got {out}");
                    prop_assert!((0.0..=1.0).contains(&out), "out={out} not in [0,1]");
                }
                None => prop_assert!(!c.is_finite(), "None only allowed for non-finite input, got {c}"),
            }
        }

        #[test]
        fn normalize_agent_is_idempotent(s in "[[:print:]]{0,64}") {
            let first = normalize_agent(&s);
            if let Ok(first_val) = first {
                let second = normalize_agent(&first_val)
                    .expect("normalized output should itself be valid input");
                prop_assert_eq!(first_val, second);
            }
        }

        #[test]
        fn normalize_agent_err_iff_trim_empty(s in "[[:print:]]{0,64}") {
            let is_err = normalize_agent(&s).is_err();
            let trimmed_empty = s.trim().is_empty();
            prop_assert_eq!(is_err, trimmed_empty,
                "err iff trim empty for input {:?}", s);
        }
    }
}
