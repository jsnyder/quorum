//! SQLite-backed persistent storage for quorum review and telemetry data.
//!
//! `StorageHandle` is a shared, thread-safe connection wrapper suitable for
//! passing across async boundaries. `initialize` opens (or creates) the
//! database at `<quorum_home>/quorum.db`, enables WAL journal mode, and runs
//! idempotent schema migrations keyed on `PRAGMA user_version`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use rusqlite::Connection;
use rusqlite::params;

/// Shared, thread-safe database connection handle.
///
/// Wrapped in `Arc<Mutex<_>>` so it can be cloned across pipeline stages
/// and (later) passed into async tasks that need write access.
pub type StorageHandle = Arc<Mutex<Connection>>;

/// The on-disk schema is newer than this build understands.
///
/// A distinct type rather than a plain `anyhow!` so `initialize`'s corruption
/// recovery can tell a version mismatch from a damaged file. Recovering from
/// this would rename the user's database aside and create an empty one --
/// data loss on a database that is perfectly intact.
#[derive(Debug)]
pub struct FutureSchemaVersion {
    pub found: u32,
    pub supported: u32,
}

impl std::fmt::Display for FutureSchemaVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "database schema version {} is newer than this build supports ({}). \
             It was written by a later version of quorum; upgrade, or point \
             QUORUM_HOME at a different directory.",
            self.found, self.supported
        )
    }
}

impl std::error::Error for FutureSchemaVersion {}

/// Current schema version. Bumped by each `migrate_vN_to_vN+1` function.
///
/// #551: this was `#[cfg(test)]`, so production had no single source of truth
/// for the current version -- each migration hardcoded its own number and
/// nothing could compare an on-disk version against "what this build knows".
/// `run_migrations` needs exactly that comparison to reject a database written
/// by a newer binary, so the constant is now real.
const SCHEMA_VERSION: u32 = 5;

/// Open (or create) the quorum SQLite database and run any pending
/// migrations. Returns a shared connection handle ready for use.
///
/// The database file lives at `<quorum_home>/quorum.db`. The parent
/// directory is created if it does not exist.
///
/// If the database file exists but is corrupt (cannot be opened, fails
/// integrity check, or migrations fail), the corrupt file is renamed to
/// `quorum.db.corrupt` and a fresh database is created.
///
/// # Errors
///
/// Returns an error if the directory cannot be created, or if a fresh
/// database cannot be created after corruption recovery.
/// Refuse to open the developer's real `~/.quorum` from a test (#503).
///
/// This is the function that actually opens the SQLite mapping, so it is the
/// right place to guard: there are several independent `$HOME` resolvers
/// (`main.rs::quorum_dir`, `mcp/handler.rs`, the `QUORUM_TRACE` path) and
/// guarding each would be the same "remember to" discipline that failed.
///
/// Why it matters: tests in a target run as threads in ONE process, so two
/// tests opening the same `~/.quorum/quorum.db` share a WAL mapping. A `-shm`
/// remap under that concurrency is the textbook cause of the SIGBUS seen in
/// CI -- a hard fault with no panic, no `test result:` line, passing on
/// re-run.
///
/// NOTE ON COVERAGE: `cfg(test)` is per-crate. When the *binary's* test target
/// is built, this library is compiled as an ordinary dependency with
/// `cfg(test)` OFF, so this guard does NOT fire for bin tests. The bin has its
/// own guard in `quorum_dir()`. Neither one covers the other; both are needed.
#[cfg(test)]
fn refuse_real_home(quorum_home: &Path) {
    let Ok(home) = std::env::var("HOME") else {
        return;
    };
    let real = std::path::Path::new(&home).join(".quorum");
    let same = quorum_home == real
        || matches!(
            (quorum_home.canonicalize(), real.canonicalize()),
            (Ok(a), Ok(b)) if a == b
        );
    assert!(
        !same,
        "storage::initialize() was pointed at the real {} inside a test \
         (#503). Tests share one process, so concurrent access to the same \
         quorum.db can SIGBUS. Use a tempdir.",
        real.display()
    );
}

pub fn initialize(quorum_home: &Path) -> anyhow::Result<StorageHandle> {
    #[cfg(test)]
    refuse_real_home(quorum_home);

    std::fs::create_dir_all(quorum_home)
        .with_context(|| format!("failed to create quorum home: {}", quorum_home.display()))?;

    let db_path = quorum_home.join("quorum.db");

    let conn = match open_and_migrate(&db_path) {
        Ok(conn) => conn,
        // #551: a future schema version is NOT corruption. Falling through to
        // the recovery below would rename an intact database aside and create
        // an empty one, which is data loss on a version mismatch.
        Err(e) if e.downcast_ref::<FutureSchemaVersion>().is_some() => return Err(e),
        Err(e) => {
            eprintln!(
                "warning: could not open quorum.db: {}. Creating fresh database. \
                 Your previous review/telemetry data may need recovery.",
                e
            );
            let backup = quorum_home.join("quorum.db.corrupt");
            if let Err(re) = std::fs::rename(&db_path, &backup) {
                eprintln!("warning: failed to rename corrupt database: {}", re);
            }
            for ext in &["-wal", "-shm"] {
                let sidecar = quorum_home.join(format!("quorum.db{ext}"));
                if sidecar.exists() {
                    let _ = std::fs::remove_file(&sidecar);
                }
            }
            open_and_migrate(&db_path)
                .context("failed to create fresh database after corruption recovery")?
        }
    };

    migrate_reviews_jsonl(&conn, quorum_home)?;
    migrate_telemetry_jsonl(&conn, quorum_home)?;

    Ok(Arc::new(Mutex::new(conn)))
}

/// Create an in-memory database with the full schema applied.
/// Used as a fallback when the on-disk database cannot be opened.
pub fn in_memory_handle() -> StorageHandle {
    let conn = Connection::open_in_memory().expect("in-memory DB");
    run_migrations(&conn).expect("in-memory schema migration");
    Arc::new(Mutex::new(conn))
}

/// Open the database, configure pragmas, run an integrity check, and
/// apply schema migrations. Returns the ready connection on success.
/// Pick an archive path for a migrated JSONL file that does not overwrite an
/// existing one.
///
/// #556: the archive name is fixed (`<name>.migrated`), and import now runs
/// whenever the source file exists rather than only on first run. On a machine
/// that has migrated before, the archive is already there and `std::fs::rename`
/// would replace it -- silently discarding the earlier archive. Suffix with a
/// counter when the plain name is taken.
fn archive_path(quorum_home: &Path, file_name: &str) -> std::path::PathBuf {
    let base = quorum_home.join(format!("{file_name}.migrated"));
    if !base.exists() {
        return base;
    }
    for n in 2..1000 {
        let candidate = quorum_home.join(format!("{file_name}.migrated.{n}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    // Vanishingly unlikely; better to overwrite the thousandth archive than to
    // fail the migration and strand the source file again.
    base
}

fn open_and_migrate(db_path: &Path) -> anyhow::Result<Connection> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("failed to open database: {}", db_path.display()))?;

    // WAL mode for concurrent readers + single writer.
    conn.pragma_update(None, "journal_mode", "WAL")
        .context("failed to set WAL journal mode")?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .context("failed to enable foreign keys")?;

    // Quick integrity check — catches corruption early.
    let ok: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if ok != "ok" {
        anyhow::bail!("database integrity check failed: {}", ok);
    }

    run_migrations(&conn)?;
    Ok(conn)
}

/// Read the current schema version from `PRAGMA user_version`.
fn current_version(conn: &Connection) -> anyhow::Result<u32> {
    let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    Ok(version)
}

/// Dispatch pending migrations in order.
fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
    let version = current_version(conn)?;

    // #551: the steps below are all `if version < N`, so a database written by
    // a NEWER binary satisfies none of them and this function returns Ok --
    // reporting "ready" for a schema it does not understand. The failure then
    // surfaces on the first query touching a renamed or dropped column, where
    // it reads as a query bug rather than a version mismatch.
    if version > SCHEMA_VERSION {
        return Err(FutureSchemaVersion {
            found: version,
            supported: SCHEMA_VERSION,
        }
        .into());
    }

    if version < 1 {
        migrate_v0_to_v1(conn).context("schema migration v0 -> v1 failed")?;
    }

    if version < 2 {
        migrate_v1_to_v2(conn).context("schema migration v1 -> v2 failed")?;
    }

    if version < 3 {
        migrate_v2_to_v3(conn).context("schema migration v2 -> v3 failed")?;
    }

    if version < 4 {
        migrate_v3_to_v4(conn).context("schema migration v3 -> v4 failed")?;
    }

    if version < 5 {
        migrate_v4_to_v5(conn).context("schema migration v4 -> v5 failed")?;
    }

    Ok(())
}

/// Schema v1: reviews, review_finding_ids, telemetry tables.
fn migrate_v0_to_v1(conn: &Connection) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;

    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS reviews (
            run_id            TEXT PRIMARY KEY,
            timestamp         TEXT NOT NULL,
            quorum_version    TEXT NOT NULL,
            repo              TEXT,
            invoked_from      TEXT NOT NULL,
            model             TEXT NOT NULL,
            files_reviewed    INTEGER NOT NULL,
            lines_added       INTEGER,
            lines_removed     INTEGER,
            critical          INTEGER NOT NULL DEFAULT 0,
            high              INTEGER NOT NULL DEFAULT 0,
            medium            INTEGER NOT NULL DEFAULT 0,
            low               INTEGER NOT NULL DEFAULT 0,
            info              INTEGER NOT NULL DEFAULT 0,
            suppressed_by_rule TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(suppressed_by_rule)),
            tokens_in         INTEGER NOT NULL,
            tokens_out        INTEGER NOT NULL,
            tokens_cache_read INTEGER NOT NULL DEFAULT 0,
            duration_ms       INTEGER NOT NULL,
            flag_deep         INTEGER NOT NULL DEFAULT 0,
            flag_parallel_n   INTEGER NOT NULL DEFAULT 0,
            flag_ensemble     INTEGER NOT NULL DEFAULT 0,
            mode              TEXT,
            context           TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(context))
        );

        CREATE TABLE IF NOT EXISTS review_finding_ids (
            run_id      TEXT NOT NULL REFERENCES reviews(run_id),
            finding_id  TEXT NOT NULL,
            PRIMARY KEY (run_id, finding_id)
        );
        CREATE INDEX IF NOT EXISTS idx_finding_id ON review_finding_ids(finding_id);

        CREATE TABLE IF NOT EXISTS telemetry (
            id                       INTEGER PRIMARY KEY AUTOINCREMENT,
            ts                       TEXT NOT NULL,
            files                    TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(files)),
            findings                 TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(findings)),
            model                    TEXT NOT NULL,
            tokens_in                INTEGER NOT NULL,
            tokens_out               INTEGER NOT NULL,
            duration_ms              INTEGER NOT NULL,
            suppressed               INTEGER NOT NULL DEFAULT 0,
            context7_resolved        INTEGER NOT NULL DEFAULT 0,
            context7_resolve_failed  INTEGER NOT NULL DEFAULT 0,
            context7_query_failed    INTEGER NOT NULL DEFAULT 0,
            context7_skipped_popular INTEGER NOT NULL DEFAULT 0,
            -- Inert since #522: popularity-tier budgets were deleted and
            -- nothing reads or writes this. Deliberately NOT dropped --
            -- NOT NULL DEFAULT 0 means omitting it from INSERTs is safe,
            -- whereas a DROP COLUMN migration that failed on a real
            -- telemetry database would break startup. Leave it alone.
            context7_budget_reduced  INTEGER NOT NULL DEFAULT 0,
            fp_kind_utilization_rate REAL,
            judge_calls              INTEGER NOT NULL DEFAULT 0,
            judge_approved           INTEGER NOT NULL DEFAULT 0,
            judge_rejected           INTEGER NOT NULL DEFAULT 0,
            judge_uncertain          INTEGER NOT NULL DEFAULT 0,
            judge_skipped            INTEGER NOT NULL DEFAULT 0,
            judge_cache_hits         INTEGER NOT NULL DEFAULT 0,
            judge_latency_ms         INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_telemetry_ts ON telemetry(ts);",
    )?;

    tx.pragma_update(None, "user_version", 1)?;
    tx.commit()?;

    Ok(())
}

/// Schema v2: timestamp index on reviews for efficient range queries.
fn migrate_v1_to_v2(conn: &Connection) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("CREATE INDEX IF NOT EXISTS idx_reviews_timestamp ON reviews(timestamp);")?;
    tx.pragma_update(None, "user_version", 2)?;
    tx.commit()?;
    Ok(())
}

/// Schema v3: add `title` and `file_path` columns to `review_finding_ids`.
///
/// These columns allow finding records to carry human-readable metadata
/// (the finding title and the source file it was raised against) so that
/// downstream consumers (PR comments, feedback joins) can display them
/// without a round-trip to the LLM response cache.
fn migrate_v2_to_v3(conn: &Connection) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "ALTER TABLE review_finding_ids ADD COLUMN title TEXT NOT NULL DEFAULT '';
         ALTER TABLE review_finding_ids ADD COLUMN file_path TEXT NOT NULL DEFAULT '';",
    )?;
    tx.pragma_update(None, "user_version", 3)?;
    tx.commit()?;
    Ok(())
}

/// Schema v4: add `rule_id` to `review_finding_ids` (#523).
///
/// Without it the feedback recording path had no way to learn which rule
/// produced a finding, so `FeedbackEntry.rule_id` was written as `None` on
/// every row and `stats --by-rule` returned an empty table by construction.
/// Existing rows keep `''` and stay derivable from their title prefix.
fn migrate_v3_to_v4(conn: &Connection) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "ALTER TABLE review_finding_ids ADD COLUMN rule_id TEXT NOT NULL DEFAULT '';",
    )?;
    tx.pragma_update(None, "user_version", 4)?;
    tx.commit()?;
    Ok(())
}

/// v5: index `review_finding_ids.file_path` (#553).
///
/// `ReviewLog::resolve_finding_id` selects rows by `file_path` and nothing
/// else, and it ran as a full scan: 3.6-4.7s of CPU per `quorum feedback`
/// call on a 13k-row table, 0.5s with this index.
fn migrate_v4_to_v5(conn: &Connection) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_rfi_file_path ON review_finding_ids(file_path);",
    )?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

/// Migrate `reviews.jsonl` into the `reviews` + `review_finding_ids` tables.
///
/// Runs once: if the file does not exist or the table already has data,
/// the function is a no-op. On success the source file is renamed to
/// `reviews.jsonl.migrated` so subsequent starts skip immediately.
///
/// Uses `serde_json::Value` for deserialization so the migration logic
/// lives in the library crate without depending on `ReviewRecord` (which
/// is declared in the binary crate). The JSON field extraction mirrors
/// the `ReviewRecord` serde shape exactly.
fn migrate_reviews_jsonl(conn: &Connection, quorum_home: &Path) -> anyhow::Result<()> {
    use std::io::{BufRead, BufReader};

    let jsonl_path = quorum_home.join("reviews.jsonl");
    if !jsonl_path.is_file() {
        return Ok(());
    }

    // #556: no row-count guard. It was written for the first-run case and
    // turned the second-run case into permanent limbo: after the May 2026
    // migration renamed the original file, an older binary wrote 25 more
    // reviews to a fresh `reviews.jsonl`. The table already had rows, so the
    // file was never imported, never renamed, and every command warned about
    // it forever while those 25 reviews stayed out of the database.
    //
    // The guard was not protecting anything either way: the inserts below are
    // `INSERT OR IGNORE`, so re-importing an already-imported file is a no-op.
    // Import whenever the file exists, then rename it, so the condition
    // clears itself.

    let file = std::fs::File::open(&jsonl_path)
        .with_context(|| format!("failed to open {}", jsonl_path.display()))?;
    let reader = BufReader::new(file);

    let tx = conn.unchecked_transaction()?;
    let mut migrated: usize = 0;
    let mut skipped: usize = 0;

    for (line_no, line_result) in reader.lines().enumerate() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                eprintln!(
                    "warning: reviews.jsonl line {}: read error: {}",
                    line_no + 1,
                    e
                );
                skipped += 1;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "warning: reviews.jsonl line {}: malformed JSON: {}",
                    line_no + 1,
                    e
                );
                skipped += 1;
                continue;
            }
        };

        // Extract required fields; skip line if any are missing.
        let run_id = match v["run_id"].as_str() {
            Some(s) => s,
            None => {
                eprintln!(
                    "warning: reviews.jsonl line {}: missing run_id",
                    line_no + 1
                );
                skipped += 1;
                continue;
            }
        };
        let timestamp = v["timestamp"].as_str().unwrap_or_default();
        let quorum_version = v["quorum_version"].as_str().unwrap_or_default();
        let repo = v["repo"].as_str().map(String::from);
        let invoked_from = v["invoked_from"].as_str().unwrap_or("unknown");
        let model = v["model"].as_str().unwrap_or("unknown");
        let files_reviewed = v["files_reviewed"].as_i64().unwrap_or(0);
        let lines_added: Option<i64> = v["lines_added"].as_i64();
        let lines_removed: Option<i64> = v["lines_removed"].as_i64();

        let sev = &v["findings_by_severity"];
        let critical = sev["critical"].as_i64().unwrap_or(0);
        let high = sev["high"].as_i64().unwrap_or(0);
        let medium = sev["medium"].as_i64().unwrap_or(0);
        let low = sev["low"].as_i64().unwrap_or(0);
        let info = sev["info"].as_i64().unwrap_or(0);

        // suppressed_by_rule and context are stored as JSON text columns.
        let suppressed_json = if v["suppressed_by_rule"].is_object() {
            serde_json::to_string(&v["suppressed_by_rule"])?
        } else {
            "{}".to_string()
        };

        let tokens_in = v["tokens_in"].as_i64().unwrap_or(0);
        let tokens_out = v["tokens_out"].as_i64().unwrap_or(0);
        let tokens_cache_read = v["tokens_cache_read"].as_i64().unwrap_or(0);
        let duration_ms = v["duration_ms"].as_i64().unwrap_or(0);

        let flags = &v["flags"];
        let flag_deep: i32 = if flags["deep"].as_bool().unwrap_or(false) {
            1
        } else {
            0
        };
        let flag_parallel_n = flags["parallel_n"].as_i64().unwrap_or(0);
        let flag_ensemble: i32 = if flags["ensemble"].as_bool().unwrap_or(false) {
            1
        } else {
            0
        };

        let mode: Option<&str> = v["mode"].as_str();

        let context_json = if v["context"].is_object() {
            serde_json::to_string(&v["context"])?
        } else {
            "{}".to_string()
        };

        tx.execute(
            "INSERT OR IGNORE INTO reviews (
                run_id, timestamp, quorum_version, repo, invoked_from, model,
                files_reviewed, lines_added, lines_removed,
                critical, high, medium, low, info,
                suppressed_by_rule,
                tokens_in, tokens_out, tokens_cache_read, duration_ms,
                flag_deep, flag_parallel_n, flag_ensemble,
                mode, context
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6,
                ?7, ?8, ?9,
                ?10, ?11, ?12, ?13, ?14,
                ?15,
                ?16, ?17, ?18, ?19,
                ?20, ?21, ?22,
                ?23, ?24
            )",
            params![
                run_id,
                timestamp,
                quorum_version,
                repo,
                invoked_from,
                model,
                files_reviewed,
                lines_added,
                lines_removed,
                critical,
                high,
                medium,
                low,
                info,
                suppressed_json,
                tokens_in,
                tokens_out,
                tokens_cache_read,
                duration_ms,
                flag_deep,
                flag_parallel_n,
                flag_ensemble,
                mode,
                context_json,
            ],
        )?;

        // #551: `INSERT OR IGNORE` above means a duplicate run_id leaves the
        // existing row untouched. Inserting this line's children anyway would
        // merge finding ids across the two lines, producing a review row that
        // matches neither source record. If the parent changed nothing, the
        // whole line is a duplicate -- skip it as a unit.
        //
        // This also makes the #556 change safe: re-importing a file whose rows
        // are already present now genuinely no-ops, rather than re-attaching
        // children on every run.
        if tx.changes() == 0 {
            skipped += 1;
            continue;
        }

        // finding_ids child table.
        if let Some(fids) = v["finding_ids"].as_array() {
            for fid_val in fids {
                if let Some(fid) = fid_val.as_str() {
                    tx.execute(
                        "INSERT OR IGNORE INTO review_finding_ids (run_id, finding_id) VALUES (?1, ?2)",
                        params![run_id, fid],
                    )?;
                }
            }
        }

        migrated += 1;
    }

    tx.commit()?;

    let migrated_path = archive_path(quorum_home, "reviews.jsonl");
    std::fs::rename(&jsonl_path, &migrated_path).with_context(|| {
        format!(
            "failed to rename {} to {}",
            jsonl_path.display(),
            migrated_path.display()
        )
    })?;

    eprintln!(
        "Migrated {} reviews to quorum.db ({} skipped)",
        migrated, skipped
    );

    Ok(())
}

/// Migrate `telemetry.jsonl` into the `telemetry` table.
///
/// Same pattern as `migrate_reviews_jsonl`: idempotent, skip if target
/// table has data, rename source to `.migrated` on success.
///
/// Uses `serde_json::Value` for the same library-crate isolation reason
/// as `migrate_reviews_jsonl`.
fn migrate_telemetry_jsonl(conn: &Connection, quorum_home: &Path) -> anyhow::Result<()> {
    use std::io::{BufRead, BufReader};

    let jsonl_path = quorum_home.join("telemetry.jsonl");
    if !jsonl_path.is_file() {
        return Ok(());
    }

    // #556: no row-count guard, same reasoning as the reviews migration
    // above -- the insert is `INSERT OR IGNORE`, so import is idempotent, and
    // guarding on table population strands any file written after the first
    // migration ran.

    let file = std::fs::File::open(&jsonl_path)
        .with_context(|| format!("failed to open {}", jsonl_path.display()))?;
    let reader = BufReader::new(file);

    let tx = conn.unchecked_transaction()?;
    let mut migrated: usize = 0;
    let mut skipped: usize = 0;

    for (line_no, line_result) in reader.lines().enumerate() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                eprintln!(
                    "warning: telemetry.jsonl line {}: read error: {}",
                    line_no + 1,
                    e
                );
                skipped += 1;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "warning: telemetry.jsonl line {}: malformed JSON: {}",
                    line_no + 1,
                    e
                );
                skipped += 1;
                continue;
            }
        };

        let ts = v["ts"].as_str().unwrap_or_default();
        let model = v["model"].as_str().unwrap_or("unknown");

        // files and findings are stored as JSON text columns.
        let files_json = if v["files"].is_array() {
            serde_json::to_string(&v["files"])?
        } else {
            "[]".to_string()
        };
        let findings_json = if v["findings"].is_object() {
            serde_json::to_string(&v["findings"])?
        } else {
            "{}".to_string()
        };

        let tokens_in = v["tokens_in"].as_i64().unwrap_or(0);
        let tokens_out = v["tokens_out"].as_i64().unwrap_or(0);
        let duration_ms = v["duration_ms"].as_i64().unwrap_or(0);
        let suppressed = v["suppressed"].as_i64().unwrap_or(0);

        let context7_resolved = v["context7_resolved"].as_i64().unwrap_or(0);
        let context7_resolve_failed = v["context7_resolve_failed"].as_i64().unwrap_or(0);
        let context7_query_failed = v["context7_query_failed"].as_i64().unwrap_or(0);
        let context7_skipped_popular = v["context7_skipped_popular"].as_i64().unwrap_or(0);

        let fp_kind_utilization_rate: Option<f64> = v["fp_kind_utilization_rate"].as_f64();

        let judge_calls = v["judge_calls"].as_i64().unwrap_or(0);
        let judge_approved = v["judge_approved"].as_i64().unwrap_or(0);
        let judge_rejected = v["judge_rejected"].as_i64().unwrap_or(0);
        let judge_uncertain = v["judge_uncertain"].as_i64().unwrap_or(0);
        let judge_skipped = v["judge_skipped"].as_i64().unwrap_or(0);
        let judge_cache_hits = v["judge_cache_hits"].as_i64().unwrap_or(0);
        let judge_latency_ms = v["judge_latency_ms"].as_i64().unwrap_or(0);

        tx.execute(
            "INSERT INTO telemetry (
                ts, files, findings, model,
                tokens_in, tokens_out, duration_ms, suppressed,
                context7_resolved, context7_resolve_failed, context7_query_failed,
                context7_skipped_popular,
                fp_kind_utilization_rate,
                judge_calls, judge_approved, judge_rejected,
                judge_uncertain, judge_skipped, judge_cache_hits,
                judge_latency_ms
            ) VALUES (
                ?1, ?2, ?3, ?4,
                ?5, ?6, ?7, ?8,
                ?9, ?10, ?11,
                ?12,
                ?13,
                ?14, ?15, ?16,
                ?17, ?18, ?19,
                ?20
            )",
            params![
                ts,
                files_json,
                findings_json,
                model,
                tokens_in,
                tokens_out,
                duration_ms,
                suppressed,
                context7_resolved,
                context7_resolve_failed,
                context7_query_failed,
                context7_skipped_popular,
                fp_kind_utilization_rate,
                judge_calls,
                judge_approved,
                judge_rejected,
                judge_uncertain,
                judge_skipped,
                judge_cache_hits,
                judge_latency_ms,
            ],
        )?;

        migrated += 1;
    }

    tx.commit()?;

    let migrated_path = archive_path(quorum_home, "telemetry.jsonl");
    std::fs::rename(&jsonl_path, &migrated_path).with_context(|| {
        format!(
            "failed to rename {} to {}",
            jsonl_path.display(),
            migrated_path.display()
        )
    })?;

    eprintln!(
        "Migrated {} telemetry entries to quorum.db ({} skipped)",
        migrated, skipped
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    /// The #503 guard must actually fire. Deterministic: reads `HOME` rather
    /// than mutating it, so it does not race other tests in this process
    /// (which is the #497 hazard) and does not depend on argv or cwd.
    #[test]
    #[should_panic(expected = "#503")]
    fn refuse_real_home_rejects_the_developers_quorum_dir() {
        let home = std::env::var("HOME").expect("HOME is set in a test run");
        super::refuse_real_home(&std::path::Path::new(&home).join(".quorum"));
    }

    /// The other half: a guard that rejected everything would also pass the
    /// test above while breaking every legitimate caller.
    #[test]
    fn refuse_real_home_allows_a_tempdir() {
        let tmp = tempfile::tempdir().unwrap();
        super::refuse_real_home(tmp.path());
    }

    use super::*;
    use tempfile::TempDir;

    #[test]
    fn initialize_creates_tables() {
        let dir = TempDir::new().unwrap();
        let handle = initialize(dir.path()).expect("initialize should succeed");
        let conn = handle.lock().unwrap();

        // Verify user_version is 1.
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        // Verify all three tables exist by querying sqlite_master.
        let tables: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT name FROM sqlite_master
                     WHERE type = 'table' AND name IN ('reviews', 'review_finding_ids', 'telemetry')
                     ORDER BY name",
                )
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(tables, vec!["review_finding_ids", "reviews", "telemetry"]);

        // Verify the index on review_finding_ids exists.
        let idx_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_finding_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(idx_count, 1);

        // Verify the index on telemetry(ts) exists.
        let ts_idx_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_telemetry_ts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(ts_idx_count, 1);

        // Verify WAL journal mode is active.
        let journal: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal.to_lowercase(), "wal");
    }

    #[test]
    fn initialize_is_idempotent() {
        let dir = TempDir::new().unwrap();

        // First call creates the schema.
        let handle1 = initialize(dir.path()).expect("first initialize should succeed");
        {
            let conn = handle1.lock().unwrap();
            let version: u32 = conn
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(version, SCHEMA_VERSION);
        }
        // Drop the first handle so the connection is closed.
        drop(handle1);

        // Second call on the same directory is a no-op (no errors, same version).
        let handle2 = initialize(dir.path()).expect("second initialize should succeed");
        let conn = handle2.lock().unwrap();
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        // Tables still exist and are intact.
        let table_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN ('reviews', 'review_finding_ids', 'telemetry')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 3);
    }

    // ── JSONL migration tests (Task 4) ───────────────────────────────

    /// Build a synthetic reviews.jsonl line for migration tests.
    /// Uses a raw JSON string to avoid depending on binary-crate types.
    fn sample_review_json(run_id: &str) -> String {
        format!(
            r#"{{"run_id":"{run_id}","timestamp":"2026-05-10T12:00:00+00:00","quorum_version":"0.22.0","repo":"test-repo","invoked_from":"tty","model":"gpt-5.4","files_reviewed":2,"lines_added":50,"lines_removed":10,"findings_by_severity":{{"critical":0,"high":1,"medium":2,"low":0,"info":3}},"suppressed_by_rule":{{}},"tokens_in":5000,"tokens_out":800,"tokens_cache_read":1000,"duration_ms":2500,"flags":{{"deep":false,"parallel_n":4,"ensemble":false}},"finding_ids":["fid-A","fid-B"]}}"#
        )
    }

    /// Build a synthetic telemetry.jsonl line for migration tests.
    fn sample_telemetry_json() -> String {
        r#"{"ts":"2026-05-10T12:00:00+00:00","files":["src/main.rs"],"findings":{"critical":1},"model":"gpt-5.4","tokens_in":4200,"tokens_out":1800,"duration_ms":3400,"suppressed":2,"context7_resolved":1,"context7_resolve_failed":0,"context7_query_failed":0,"context7_skipped_popular":0,"context7_budget_reduced":0,"fp_kind_utilization_rate":0.42,"judge_calls":5,"judge_approved":3,"judge_rejected":1,"judge_uncertain":1,"judge_skipped":0,"judge_cache_hits":2,"judge_latency_ms":120}"#.to_string()
    }

    #[test]
    fn migrate_reviews_jsonl_imports_and_renames() {
        use std::io::Write;
        let dir = TempDir::new().unwrap();
        let jsonl_path = dir.path().join("reviews.jsonl");
        {
            let mut f = std::fs::File::create(&jsonl_path).unwrap();
            writeln!(f, "{}", sample_review_json("01MIGRATE_REVIEW_TEST001")).unwrap();
        }

        let handle = initialize(dir.path()).expect("initialize");

        // The original file should be gone; .migrated should exist.
        assert!(!jsonl_path.exists(), "reviews.jsonl should be removed");
        assert!(
            dir.path().join("reviews.jsonl.migrated").exists(),
            "reviews.jsonl.migrated should exist"
        );

        let conn = handle.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM reviews", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let run_id: String = conn
            .query_row("SELECT run_id FROM reviews", [], |row| row.get(0))
            .unwrap();
        assert_eq!(run_id, "01MIGRATE_REVIEW_TEST001");

        // Verify finding_ids in child table.
        let fid_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM review_finding_ids WHERE run_id = ?1",
                params!["01MIGRATE_REVIEW_TEST001"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fid_count, 2);
    }

    #[test]
    fn migrate_telemetry_jsonl_imports_and_renames() {
        use std::io::Write;
        let dir = TempDir::new().unwrap();
        let jsonl_path = dir.path().join("telemetry.jsonl");
        {
            let mut f = std::fs::File::create(&jsonl_path).unwrap();
            writeln!(f, "{}", sample_telemetry_json()).unwrap();
        }

        let handle = initialize(dir.path()).expect("initialize");

        assert!(!jsonl_path.exists(), "telemetry.jsonl should be removed");
        assert!(
            dir.path().join("telemetry.jsonl.migrated").exists(),
            "telemetry.jsonl.migrated should exist"
        );

        let conn = handle.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM telemetry", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let model: String = conn
            .query_row("SELECT model FROM telemetry", [], |row| row.get(0))
            .unwrap();
        assert_eq!(model, "gpt-5.4");
    }

    #[test]
    fn migrate_skips_malformed_lines() {
        use std::io::Write;
        let dir = TempDir::new().unwrap();
        let jsonl_path = dir.path().join("reviews.jsonl");
        {
            let mut f = std::fs::File::create(&jsonl_path).unwrap();
            // Valid line
            writeln!(f, "{}", sample_review_json("01MIGRATE_SKIP_TEST00001")).unwrap();
            // Malformed line
            writeln!(f, "{{ this is not valid json }}").unwrap();
            // Duplicate run_id (INSERT OR IGNORE will skip the dupe)
            writeln!(f, "{}", sample_review_json("01MIGRATE_SKIP_TEST00001")).unwrap();
        }

        let handle = initialize(dir.path()).expect("initialize");

        let conn = handle.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM reviews", [], |row| row.get(0))
            .unwrap();
        // Only 1 because the second valid line has the same PK and uses INSERT OR IGNORE
        assert_eq!(
            count, 1,
            "should have 1 row (dupe ignored, malformed skipped)"
        );

        assert!(!jsonl_path.exists(), "reviews.jsonl should be removed");
    }

    #[test]
    fn migrate_v1_to_v2_creates_timestamp_index() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        migrate_v0_to_v1(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 1);

        migrate_v1_to_v2(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 2);

        let idx_exists: bool = conn
            .prepare(
                "SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_reviews_timestamp'",
            )
            .unwrap()
            .exists([])
            .unwrap();
        assert!(
            idx_exists,
            "idx_reviews_timestamp must exist after v2 migration"
        );
    }

    #[test]
    fn migrate_v2_to_v3_adds_title_and_file_path_columns() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();

        // Run v1 + v2 migrations to establish baseline schema.
        migrate_v0_to_v1(&conn).unwrap();
        migrate_v1_to_v2(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 2);

        // Insert a review + finding_id row under the v2 schema.
        conn.execute(
            "INSERT INTO reviews (
                run_id, timestamp, quorum_version, invoked_from, model,
                files_reviewed, tokens_in, tokens_out, duration_ms
            ) VALUES ('run-v2-test', '2026-06-01T00:00:00Z', '0.23.0', 'tty', 'gpt-5.4', 1, 100, 50, 500)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO review_finding_ids (run_id, finding_id) VALUES ('run-v2-test', 'fid-legacy')",
            [],
        )
        .unwrap();

        // Run v3 migration.
        migrate_v2_to_v3(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 3);

        // Legacy row should have empty defaults for the new columns.
        let (title, file_path): (String, String) = conn
            .query_row(
                "SELECT title, file_path FROM review_finding_ids WHERE finding_id = 'fid-legacy'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(title, "", "legacy row title should default to empty string");
        assert_eq!(
            file_path, "",
            "legacy row file_path should default to empty string"
        );

        // New inserts with title + file_path should work.
        conn.execute(
            "INSERT INTO review_finding_ids (run_id, finding_id, title, file_path) \
             VALUES ('run-v2-test', 'fid-new', 'SQL injection risk', 'src/db.rs')",
            [],
        )
        .unwrap();

        let (new_title, new_fp): (String, String) = conn
            .query_row(
                "SELECT title, file_path FROM review_finding_ids WHERE finding_id = 'fid-new'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(new_title, "SQL injection risk");
        assert_eq!(new_fp, "src/db.rs");
    }

    /// #556: a JSONL file written *after* a migration must still be imported.
    ///
    /// The old row-count guard skipped import whenever the table had rows, so
    /// a file created after the first migration was never imported and never
    /// renamed -- warning on every command forever while its reviews stayed
    /// out of the database. On this machine that stranded 25 reviews from
    /// May 2026.
    #[test]
    fn populated_table_still_imports_a_later_jsonl_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // First run: one review, migrated and renamed.
        std::fs::write(
            home.join("reviews.jsonl"),
            r#"{"run_id":"first","timestamp":"2026-05-14T01:00:00+00:00","files_reviewed":1}
"#,
        )
        .unwrap();
        let handle = initialize(home).unwrap();
        drop(handle);
        assert!(home.join("reviews.jsonl.migrated").is_file());
        assert!(!home.join("reviews.jsonl").exists());

        // Second run: an older binary writes a fresh file with a new run_id.
        std::fs::write(
            home.join("reviews.jsonl"),
            r#"{"run_id":"stranded","timestamp":"2026-05-15T23:10:00+00:00","files_reviewed":1}
"#,
        )
        .unwrap();
        let handle = initialize(home).unwrap();

        let found: i64 = {
            let conn = handle.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM reviews WHERE run_id = 'stranded'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(found, 1, "the later file's review must reach the database");
        assert!(
            !home.join("reviews.jsonl").exists(),
            "the file must be renamed so the condition clears itself"
        );
    }

    /// #556: a second migration must not overwrite the first one's archive.
    ///
    /// The archive name is fixed and `std::fs::rename` replaces its
    /// destination, so removing the row-count guard without changing this
    /// would discard the earlier archive. On the machine this was found on
    /// that is 1.7 MB of pre-migration reviews replaced by a 46 KB file --
    /// on the very run meant to demonstrate the fix.
    #[test]
    fn second_migration_preserves_the_first_archive() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        std::fs::write(
            home.join("reviews.jsonl"),
            "{\"run_id\":\"first\",\"timestamp\":\"2026-05-14T01:00:00+00:00\",\"files_reviewed\":1}\n",
        )
        .unwrap();
        drop(initialize(home).unwrap());

        let archive = home.join("reviews.jsonl.migrated");
        let original = std::fs::read_to_string(&archive).unwrap();
        assert!(original.contains("first"));

        std::fs::write(
            home.join("reviews.jsonl"),
            "{\"run_id\":\"second\",\"timestamp\":\"2026-05-15T23:10:00+00:00\",\"files_reviewed\":1}\n",
        )
        .unwrap();
        drop(initialize(home).unwrap());

        assert_eq!(
            std::fs::read_to_string(&archive).unwrap(),
            original,
            "the first archive must be untouched"
        );
        let second = std::fs::read_to_string(home.join("reviews.jsonl.migrated.2"))
            .expect("the second file must be archived under a free name");
        assert!(second.contains("second"));
        assert!(!home.join("reviews.jsonl").exists());
    }

    /// #551: duplicate run_ids must be skipped as a unit, children included.
    ///
    /// The parent is `INSERT OR IGNORE`, so a duplicate leaves the existing
    /// row untouched. Inserting the duplicate's children anyway merged finding
    /// ids across both lines, producing a review row matching neither source
    /// record.
    #[test]
    fn duplicate_run_id_does_not_merge_finding_ids() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(
            home.join("reviews.jsonl"),
            concat!(
                r#"{"run_id":"dup","timestamp":"2026-05-14T01:00:00+00:00","files_reviewed":1,"finding_ids":["a","b"]}"#,
                "\n",
                r#"{"run_id":"dup","timestamp":"2026-05-14T02:00:00+00:00","files_reviewed":1,"finding_ids":["c","d"]}"#,
                "\n",
            ),
        )
        .unwrap();

        let handle = initialize(home).unwrap();
        let conn = handle.lock().unwrap();

        let reviews: i64 = conn
            .query_row("SELECT COUNT(*) FROM reviews WHERE run_id='dup'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(reviews, 1, "duplicate run_id must not create a second row");

        let fids: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM review_finding_ids WHERE run_id='dup'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            fids, 2,
            "only the first line's finding ids belong to the stored row; \
             found {fids}, which means the duplicate's children were merged in"
        );
    }

    /// #551: a database from a newer release must be refused, not opened.
    #[test]
    fn future_schema_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        {
            let conn = rusqlite::Connection::open(home.join("quorum.db")).unwrap();
            conn.pragma_update(None, "user_version", 99u32).unwrap();
        }
        let err = initialize(home).expect_err("a future schema version must be refused");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("99") && msg.contains(&SCHEMA_VERSION.to_string()),
            "the error must name both versions so the cause is obvious; got: {msg}"
        );

        // The assertion that actually matters. The first version of this guard
        // bailed inside `run_migrations`, which `initialize` wraps in
        // corruption recovery -- so it returned an error AND renamed the
        // database aside and created an empty one. Erroring is not enough; the
        // user's intact database has to still be there.
        assert!(
            home.join("quorum.db").is_file(),
            "the database must be left in place"
        );
        assert!(
            !home.join("quorum.db.corrupt").exists(),
            "a version mismatch must not be treated as corruption -- renaming \
             an intact database aside is data loss"
        );
        let still_future: u32 = {
            let conn = rusqlite::Connection::open(home.join("quorum.db")).unwrap();
            conn.pragma_query_value(None, "user_version", |r| r.get(0))
                .unwrap()
        };
        assert_eq!(
            still_future, 99,
            "the database must be untouched, not migrated or recreated"
        );
    }

    /// The other half: the current version still opens. A guard comparing the
    /// wrong way round would pass the test above and reject every real
    /// database.
    #[test]
    fn current_schema_version_still_opens() {
        let dir = tempfile::tempdir().unwrap();
        let handle = initialize(dir.path()).expect("a fresh database must open");
        let version: u32 = {
            let conn = handle.lock().unwrap();
            conn.pragma_query_value(None, "user_version", |r| r.get(0))
                .unwrap()
        };
        assert_eq!(version, SCHEMA_VERSION);
        // And re-opening it is fine, which is the common case.
        initialize(dir.path()).expect("re-opening at the current version must work");
    }

    #[test]
    fn migrate_v4_to_v5_creates_file_path_index() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 5);

        let idx_exists: bool = conn
            .prepare("SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_rfi_file_path'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(
            idx_exists,
            "idx_rfi_file_path must exist after v5 migration (#553)"
        );
    }

    #[test]
    fn run_migrations_advances_to_latest() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn corrupt_db_creates_fresh() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("quorum.db");

        // Write garbage to simulate corruption
        std::fs::write(&db_path, b"this is not a sqlite database").unwrap();

        // Should recover by creating fresh database
        let handle = initialize(dir.path()).unwrap();
        let conn = handle.lock().unwrap();
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        // Corrupt file should be saved
        assert!(dir.path().join("quorum.db.corrupt").exists());
    }

    #[test]
    fn end_to_end_migration_and_readback() {
        use std::io::Write;

        let dir = TempDir::new().unwrap();

        // ── 1. Write synthetic JSONL files ──────────────────────────────
        let reviews_path = dir.path().join("reviews.jsonl");
        let telemetry_path = dir.path().join("telemetry.jsonl");
        {
            let mut rf = std::fs::File::create(&reviews_path).unwrap();
            writeln!(rf, "{}", sample_review_json("01E2E_REVIEW_TEST_00001")).unwrap();
            writeln!(rf, "{}", sample_review_json("01E2E_REVIEW_TEST_00002")).unwrap();
        }
        {
            let mut tf = std::fs::File::create(&telemetry_path).unwrap();
            writeln!(tf, "{}", sample_telemetry_json()).unwrap();
        }

        // ── 2. Call initialize() to trigger migration ───────────────────
        let handle = initialize(dir.path()).expect("initialize should succeed");

        // ── 3. Verify JSONL files renamed to .migrated ──────────────────
        assert!(
            !reviews_path.exists(),
            "reviews.jsonl should be removed after migration"
        );
        assert!(
            dir.path().join("reviews.jsonl.migrated").exists(),
            "reviews.jsonl.migrated should exist"
        );
        assert!(
            !telemetry_path.exists(),
            "telemetry.jsonl should be removed after migration"
        );
        assert!(
            dir.path().join("telemetry.jsonl.migrated").exists(),
            "telemetry.jsonl.migrated should exist"
        );

        // ── 4. Read back from SQLite and verify data integrity ──────────
        let conn = handle.lock().unwrap();

        // 4a. Review count
        let review_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM reviews", [], |row| row.get(0))
            .unwrap();
        assert_eq!(review_count, 2, "should have migrated 2 review rows");

        // 4b. Verify first review record field-by-field
        let (repo, model, invoked_from, files_reviewed, lines_added, lines_removed): (
            String,
            String,
            String,
            i64,
            i64,
            i64,
        ) = conn
            .query_row(
                "SELECT repo, model, invoked_from, files_reviewed, lines_added, lines_removed \
                 FROM reviews WHERE run_id = ?1",
                params!["01E2E_REVIEW_TEST_00001"],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(repo, "test-repo");
        assert_eq!(model, "gpt-5.4");
        assert_eq!(invoked_from, "tty");
        assert_eq!(files_reviewed, 2);
        assert_eq!(lines_added, 50);
        assert_eq!(lines_removed, 10);

        // 4c. Severity counts
        let (critical, high, medium, low, info): (i64, i64, i64, i64, i64) = conn
            .query_row(
                "SELECT critical, high, medium, low, info FROM reviews WHERE run_id = ?1",
                params!["01E2E_REVIEW_TEST_00001"],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(critical, 0);
        assert_eq!(high, 1);
        assert_eq!(medium, 2);
        assert_eq!(low, 0);
        assert_eq!(info, 3);

        // 4d. Token counts and duration
        let (tokens_in, tokens_out, tokens_cache_read, duration_ms): (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT tokens_in, tokens_out, tokens_cache_read, duration_ms \
                 FROM reviews WHERE run_id = ?1",
                params!["01E2E_REVIEW_TEST_00001"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(tokens_in, 5000);
        assert_eq!(tokens_out, 800);
        assert_eq!(tokens_cache_read, 1000);
        assert_eq!(duration_ms, 2500);

        // 4e. Flags
        let (flag_deep, flag_parallel_n, flag_ensemble): (i32, i64, i32) = conn
            .query_row(
                "SELECT flag_deep, flag_parallel_n, flag_ensemble FROM reviews WHERE run_id = ?1",
                params!["01E2E_REVIEW_TEST_00001"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(flag_deep, 0);
        assert_eq!(flag_parallel_n, 4);
        assert_eq!(flag_ensemble, 0);

        // 4f. Finding IDs in child table
        let finding_ids: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT finding_id FROM review_finding_ids \
                     WHERE run_id = ?1 ORDER BY finding_id",
                )
                .unwrap();
            stmt.query_map(params!["01E2E_REVIEW_TEST_00001"], |row| row.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(finding_ids, vec!["fid-A", "fid-B"]);

        // 4g. Context and suppressed_by_rule default to empty JSON objects
        let (context_json, suppressed_json): (String, String) = conn
            .query_row(
                "SELECT context, suppressed_by_rule FROM reviews WHERE run_id = ?1",
                params!["01E2E_REVIEW_TEST_00001"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(context_json, "{}");
        assert_eq!(suppressed_json, "{}");

        // 4h. Second review also present
        let run_id_2: String = conn
            .query_row(
                "SELECT run_id FROM reviews WHERE run_id = ?1",
                params!["01E2E_REVIEW_TEST_00002"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(run_id_2, "01E2E_REVIEW_TEST_00002");

        // 4i. Telemetry record
        let telemetry_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM telemetry", [], |row| row.get(0))
            .unwrap();
        assert_eq!(telemetry_count, 1, "should have migrated 1 telemetry row");

        let (t_model, t_tokens_in, t_tokens_out, t_duration_ms, t_suppressed): (
            String,
            i64,
            i64,
            i64,
            i64,
        ) = conn
            .query_row(
                "SELECT model, tokens_in, tokens_out, duration_ms, suppressed FROM telemetry",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(t_model, "gpt-5.4");
        assert_eq!(t_tokens_in, 4200);
        assert_eq!(t_tokens_out, 1800);
        assert_eq!(t_duration_ms, 3400);
        assert_eq!(t_suppressed, 2);

        // 4j. Context7 and judge counters in telemetry
        let (c7_resolved, c7_resolve_failed, c7_query_failed, c7_skipped, c7_budget): (
            i64,
            i64,
            i64,
            i64,
            i64,
        ) = conn
            .query_row(
                "SELECT context7_resolved, context7_resolve_failed, context7_query_failed, \
                 context7_skipped_popular, context7_budget_reduced FROM telemetry",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(c7_resolved, 1);
        assert_eq!(c7_resolve_failed, 0);
        assert_eq!(c7_query_failed, 0);
        assert_eq!(c7_skipped, 0);
        assert_eq!(c7_budget, 0);

        let (judge_calls, judge_approved, judge_rejected, judge_cache_hits, judge_latency): (
            i64,
            i64,
            i64,
            i64,
            i64,
        ) = conn
            .query_row(
                "SELECT judge_calls, judge_approved, judge_rejected, \
                 judge_cache_hits, judge_latency_ms FROM telemetry",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(judge_calls, 5);
        assert_eq!(judge_approved, 3);
        assert_eq!(judge_rejected, 1);
        assert_eq!(judge_cache_hits, 2);
        assert_eq!(judge_latency, 120);

        // 4k. fp_kind_utilization_rate (nullable float)
        let fp_rate: Option<f64> = conn
            .query_row(
                "SELECT fp_kind_utilization_rate FROM telemetry",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            (fp_rate.unwrap() - 0.42).abs() < f64::EPSILON,
            "fp_kind_utilization_rate should be 0.42"
        );

        // 4l. Telemetry files and findings JSON columns
        let (files_json, findings_json): (String, String) = conn
            .query_row("SELECT files, findings FROM telemetry", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        let files_val: serde_json::Value = serde_json::from_str(&files_json).unwrap();
        assert_eq!(files_val, serde_json::json!(["src/main.rs"]));
        let findings_val: serde_json::Value = serde_json::from_str(&findings_json).unwrap();
        assert_eq!(findings_val, serde_json::json!({"critical": 1}));

        // ── 5. Verify second initialize() is idempotent ─────────────────
        drop(conn);
        drop(handle);

        let handle2 = initialize(dir.path()).expect("second initialize should succeed");
        let conn2 = handle2.lock().unwrap();

        let review_count_2: i64 = conn2
            .query_row("SELECT COUNT(*) FROM reviews", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            review_count_2, 2,
            "second initialize should not duplicate reviews"
        );

        let telemetry_count_2: i64 = conn2
            .query_row("SELECT COUNT(*) FROM telemetry", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            telemetry_count_2, 1,
            "second initialize should not duplicate telemetry"
        );
    }

    #[test]
    fn migrate_is_idempotent() {
        use std::io::Write;
        let dir = TempDir::new().unwrap();
        let jsonl_path = dir.path().join("reviews.jsonl");
        {
            let mut f = std::fs::File::create(&jsonl_path).unwrap();
            writeln!(f, "{}", sample_review_json("01MIGRATE_IDEM_TEST00001")).unwrap();
        }

        // First initialize: migrates the file.
        let handle1 = initialize(dir.path()).expect("first initialize");
        {
            let conn = handle1.lock().unwrap();
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM reviews", [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 1);
        }
        drop(handle1);

        // Second initialize: no JSONL exists, migration is a no-op.
        let handle2 = initialize(dir.path()).expect("second initialize");
        let conn = handle2.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM reviews", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "count should still be 1 after second initialize");
    }
}
