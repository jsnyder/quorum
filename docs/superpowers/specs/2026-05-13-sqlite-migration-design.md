# SQLite Migration for Reviews and Telemetry

**Issue:** #326
**Date:** 2026-05-13
**Status:** Draft

## Goal

Migrate `reviews.jsonl` and `telemetry.jsonl` from append-only JSONL files to a shared SQLite database (`~/.quorum/quorum.db`), improving query ergonomics for `quorum stats` and enabling future SQL-based analytics. Feedback stays on JSONL. Existing installs migrate automatically on first startup.

## Scope

**In scope:**
- SQLite database at `~/.quorum/quorum.db` with `reviews` and `telemetry` tables
- Normalized `review_finding_ids` child table for relational join support
- Startup migration: import existing JSONL, rename to `.jsonl.migrated`
- Schema versioning via `PRAGMA user_version`
- Same public API on `ReviewLog` and `TelemetryStore`; new `with_storage(StorageHandle)` constructor, `new(PathBuf)` retained for migration/tests

**Out of scope:**
- Migrating `feedback.jsonl` (hot path, small corpus, stays JSONL)
- Migrating `calibrator_traces.jsonl` or `trace.jsonl` (diagnostic, not queried)
- Auto-calibration on startup (separate spec, issue #327)
- Parquet export (issue #314)
- DuckDB integration (deferred)

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Which stores migrate | reviews + telemetry only | Feedback is append-heavy hot path; diagnostic logs are not queried |
| Migration strategy | Migrate-on-startup | Data volumes are small (<1s migration), `.migrated` rename gives rollback path |
| Schema shape | One table per store, flattened scalars | Data is always read as a unit; normalization adds overhead without query benefit |
| Structured fields | JSON text columns | Best balance of simplicity, inspectability, and schema evolution at our scale; validated by GPT-5.4 analysis |
| finding_ids | Normalized child table | Already acts as a relational join key in `analytics::linkage_stats()`; enables SQL-based join/overlap queries |
| Binary formats | Not used | Opaque BLOBs lose ad-hoc SQL inspection, manual debugging, and ecosystem compatibility for negligible size/perf gains at our scale |
| SQLite library | Same sqlite-vec-enabled build as context injection | Single SQLite library in the binary; DRY; vector features available if needed later |
| Trait abstraction | Not yet | Replace internals directly; `:memory:` connections in tests provide testability without trait indirection |
| Graph databases | Not adopted | LadybugDB/Cozo/SurrealDB are immature; our domain relationships are classic relational JOINs, not arbitrary-depth graph traversals |

## Architecture

### Database Location and Initialization

**File:** `~/.quorum/quorum.db` (SQLite 3, WAL journal mode)

A new `src/storage.rs` module owns database initialization, migration, and connection management. Called from `main.rs` early in startup before any store is used.

Initialization flow:
1. Open or create `~/.quorum/quorum.db` with WAL journal mode
2. Run schema migrations via `PRAGMA user_version`
3. If `reviews.jsonl` or `telemetry.jsonl` exist, trigger one-time JSONL import
4. Return a `StorageHandle` (wrapper around `Arc<Mutex<rusqlite::Connection>>`)

**Dependency:** `rusqlite` with `bundled` feature (statically links SQLite via the same build that context injection already uses, which includes sqlite-vec).

### Table Schemas

**`reviews` table:**

```sql
CREATE TABLE reviews (
    run_id            TEXT PRIMARY KEY,
    timestamp         TEXT NOT NULL,  -- ISO 8601
    quorum_version    TEXT NOT NULL,
    repo              TEXT,
    invoked_from      TEXT NOT NULL,
    model             TEXT NOT NULL,
    files_reviewed    INTEGER NOT NULL,
    lines_added       INTEGER,
    lines_removed     INTEGER,
    -- SeverityCounts flattened
    critical          INTEGER NOT NULL DEFAULT 0,
    high              INTEGER NOT NULL DEFAULT 0,
    medium            INTEGER NOT NULL DEFAULT 0,
    low               INTEGER NOT NULL DEFAULT 0,
    info              INTEGER NOT NULL DEFAULT 0,
    -- HashMap as JSON text
    suppressed_by_rule TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(suppressed_by_rule)),
    -- Token usage
    tokens_in         INTEGER NOT NULL,
    tokens_out        INTEGER NOT NULL,
    tokens_cache_read INTEGER NOT NULL,
    duration_ms       INTEGER NOT NULL,
    -- Flags flattened
    flag_deep         INTEGER NOT NULL DEFAULT 0,
    flag_parallel_n   INTEGER NOT NULL DEFAULT 0,
    flag_ensemble     INTEGER NOT NULL DEFAULT 0,
    mode              TEXT,
    -- ContextTelemetry flattened
    -- ContextTelemetry stored as single JSON TEXT column (37+ fields,
    -- including nested LegCounts and optional percentiles — too many
    -- for flattened columns, see implementation plan for rationale)
    context           TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(context))
);
```

**`review_finding_ids` table (normalized):**

```sql
CREATE TABLE review_finding_ids (
    run_id      TEXT NOT NULL REFERENCES reviews(run_id),
    finding_id  TEXT NOT NULL,
    PRIMARY KEY (run_id, finding_id)
);
CREATE INDEX idx_finding_id ON review_finding_ids(finding_id);
```

**`telemetry` table:**

```sql
CREATE TABLE telemetry (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    ts                TEXT NOT NULL,  -- ISO 8601
    files             TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(files)),
    findings          TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(findings)),
    model             TEXT NOT NULL,
    tokens_in         INTEGER NOT NULL,
    tokens_out        INTEGER NOT NULL,
    duration_ms       INTEGER NOT NULL,
    suppressed        INTEGER NOT NULL DEFAULT 0,
    -- Context7 counters
    context7_resolved       INTEGER NOT NULL DEFAULT 0,
    context7_resolve_failed INTEGER NOT NULL DEFAULT 0,
    context7_query_failed   INTEGER NOT NULL DEFAULT 0,
    context7_skipped_popular INTEGER NOT NULL DEFAULT 0,
    context7_budget_reduced  INTEGER NOT NULL DEFAULT 0,
    fp_kind_utilization_rate REAL,
    -- Judge counters
    judge_calls       INTEGER NOT NULL DEFAULT 0,
    judge_approved    INTEGER NOT NULL DEFAULT 0,
    judge_rejected    INTEGER NOT NULL DEFAULT 0,
    judge_uncertain   INTEGER NOT NULL DEFAULT 0,
    judge_skipped     INTEGER NOT NULL DEFAULT 0,
    judge_cache_hits  INTEGER NOT NULL DEFAULT 0,
    judge_latency_ms  INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_telemetry_ts ON telemetry(ts);
```

**Schema notes:**
- Booleans stored as `INTEGER` (0/1) per SQLite convention
- Timestamps stored as ISO 8601 text (sortable, human-readable, no timezone ambiguity)
- `run_id` is the natural primary key for reviews (already a ULID)
- Telemetry uses synthetic autoincrement PK (no natural key)
- JSON columns validated with `CHECK(json_valid(...))` for corruption guard
- `finding_ids` normalized into child table; all other JSON fields remain as text columns

### Startup Migration

When `storage::initialize()` opens the database, after schema migrations, it checks for existing JSONL files.

**Migration flow:**
1. Check if `~/.quorum/reviews.jsonl` exists and has content
2. Stream line-by-line (reusing existing skip-malformed-lines pattern, log warnings)
3. Insert all records into `reviews` table + `review_finding_ids` child table in a single transaction
4. On success, rename `reviews.jsonl` to `reviews.jsonl.migrated`
5. Repeat for `telemetry.jsonl` to `telemetry` table
6. Print summary to stderr: `"Migrated N reviews and M telemetry entries to quorum.db"`

**Error handling:**
- If migration fails mid-transaction, roll back. JSONL is untouched. Print error with count of rows parsed vs total lines. Next startup retries.
- If `quorum.db` exists but JSONL already renamed (`.migrated`), migration is a no-op.
- If `reviews` table is non-empty AND `reviews.jsonl` exists, skip with a warning (don't double-import).

**Rollback path:** Rename `reviews.jsonl.migrated` back to `reviews.jsonl` manually. Old binary ignores `quorum.db`.

**Performance:** Typical installs have hundreds to low-thousands of entries. SQLite bulk insert in a single transaction handles ~50k rows/second. Migration completes in well under a second.

### Store Implementation

**New module:** `src/storage.rs` — database initialization, migration, connection management.

**Modified modules:**
- `src/review_log.rs` — `ReviewLog` internals switch from JSONL to SQLite. Same public API: `append()`, `iter()`, `load_all()`.
- `src/telemetry.rs` — `TelemetryStore` internals switch from JSONL to SQLite. Same public API: `record()`, `load_all_with_stats()`.

**Connection sharing:** `storage::initialize()` returns a `StorageHandle` (wrapper around `Arc<Mutex<rusqlite::Connection>>`). Both `ReviewLog::new()` and `TelemetryStore::new()` accept a `StorageHandle` instead of a `PathBuf`.

**Serialization:**
- Scalar fields map directly to SQLite columns via rusqlite's `ToSql`/`FromSql` traits
- `HashMap` and `Vec` fields serialize to JSON text via `serde_json::to_string()` on write, `serde_json::from_str()` on read
- Small Rust wrapper newtypes for JSON-backed columns centralize serde glue
- `DateTime<Utc>` stored as ISO 8601 text strings
- Booleans stored as INTEGER 0/1
- `finding_ids` written to `review_finding_ids` child table; joined back on `load_all()`

**Thread safety:** `Arc<Mutex<Connection>>` with WAL mode. Reads don't block other reads. Writes acquire the mutex briefly for single-row inserts. No connection pooling needed — quorum is single-process.

### Modified Callers

**`src/main.rs` — startup path:**
- Call `storage::initialize(quorum_home)` early in `main()` to get a `StorageHandle`
- Pass `StorageHandle` to `ReviewLog::new()` and `TelemetryStore::new()`
- Migration happens transparently inside `initialize()`

**`src/main.rs` — review path:**
- `ReviewLog::append()` unchanged API, writes to SQLite
- `TelemetryStore::record()` unchanged API

**`src/main.rs` — stats path:**
- `ReviewLog::load_all()`, `TelemetryStore::load_all_with_stats()` unchanged API
- `stats::compute_report()` receives the same data types

**`src/dimensions.rs` / `src/analytics.rs`:**
- Operate on `Vec<ReviewRecord>` and `Vec<FeedbackEntry>` in memory. No changes.

**`src/feedback.rs`:**
- Not modified. Feedback stays on JSONL with its `PathBuf` constructor.

### Error Handling

**Database corruption:**
- If `quorum.db` fails to open, print a visible warning to stderr: `"warning: could not open quorum.db: <error>. Creating fresh database. Your previous review/telemetry data may need recovery."`. Fall back to a fresh database. Never silently swallow data loss.

**Write failures:**
- If a write fails (disk full, lock timeout), log a warning to stderr and continue. Reviews should never fail because telemetry couldn't be recorded.

**Migration edge cases:**
- Malformed JSONL lines: skip and log warning (matching existing behavior). Count skipped lines in migration summary.
- Empty JSONL files: treat as successful migration (0 rows), still rename to `.migrated`.
- Pre-existing data + JSONL present: skip with warning. User can delete JSONL manually.
- Read-only filesystem: print warning that migration was skipped due to read-only filesystem. Next startup retries.

**Schema evolution:**
- `PRAGMA user_version` tracks schema version. Pending migrations run in sequence on startup.
- Each migration runs in a transaction. Failure leaves version at old value for retry.
- New columns use `ALTER TABLE ADD COLUMN` with defaults (backward-compatible, no data rewrite).

**Concurrency:**
- WAL mode allows concurrent reads during writes.
- `Arc<Mutex<Connection>>` serializes writes. Daemon + manual review overlap is safe.

## Testing Strategy

**Unit tests (`src/storage.rs`):**
- Schema creation on fresh `:memory:` database
- Migration from v0 to v1 (and future version bumps)
- `PRAGMA user_version` correctly set after each migration

**Unit tests (`src/review_log.rs`, `src/telemetry.rs`):**
- `append()` + `load_all()` round-trip through SQLite
- `finding_ids` written to child table, joined back on read
- JSON columns (`suppressed_by_rule`, `findings`) round-trip correctly
- Empty/null optional fields handled (`mode`, `repo`, `lines_added`)
- `load_all()` returns records in timestamp order

**Integration tests (migration):**
- Synthetic `reviews.jsonl` and `telemetry.jsonl` in temp directory
- `storage::initialize()` imports rows, renames JSONL to `.migrated`
- Second `initialize()` call is idempotent (no-op)
- Malformed JSONL lines: skipped with warning, valid lines imported
- Empty JSONL: clean migration with 0 rows

**Integration tests (error paths):**
- Corrupt database file: warning printed, fresh DB created
- Pre-existing data + JSONL present: skip with warning
- `json_valid` constraint: invalid JSON rejected on insert

**No changes to existing analytics/stats tests** — they operate on `Vec<ReviewRecord>` in memory, agnostic to storage backend.

## Future Considerations

- **Feedback migration:** If feedback grows large or analytics demand SQL joins across feedback+reviews, migrate to `quorum.db` in a future release.
- **Parquet export:** Issue #314 can read directly from SQLite instead of JSONL, simplifying the export path.
- **Auto-calibration:** Issue #327 can query SQLite for calibration freshness instead of parsing JSONL timestamps.
- **Context index consolidation:** The per-source context databases (`~/.quorum/sources/<name>/index.db`) remain separate — different lifecycle and data shape than the analytics store. Unifying them into `quorum.db` is a future consideration, not part of this migration.
- **Further normalization:** If query patterns demand it, additional JSON fields (e.g., `files` in telemetry) can be promoted to child tables following the `review_finding_ids` pattern.
