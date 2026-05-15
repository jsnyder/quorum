# SQLite Migration Test Plan

**Issue:** #326 | **Date:** 2026-05-12 | **Status:** Draft

## Acceptance Criteria

- All ReviewRecord fields (including 37-field ContextTelemetry as JSON blob) survive SQLite round-trip with bit-exact fidelity
- All TelemetryEntry fields (including Option<f32> NULL) survive SQLite round-trip
- finding_ids normalize into child table and rejoin in original order
- Existing JSONL files auto-migrate on first startup; originals renamed to .migrated
- Malformed JSONL lines are skipped with warning; valid lines still imported
- Migration is idempotent: second startup is a no-op
- Double-import guard: non-empty table + existing JSONL = skip with warning
- Corrupt quorum.db is moved aside; fresh DB created with user warning
- Existing JSONL-based tests continue passing (dual-backend preserved)
- WAL mode + Arc<Mutex> serializes concurrent daemon+CLI writes safely
- PRAGMA user_version tracks schema; future v2 migrations chain correctly
- stats commands produce identical output pre/post migration
- Rollback path works: rename .migrated back, old binary ignores quorum.db

## Test Matrix

| # | Scenario | Expected | In Plan |
|---|----------|----------|---------|
| **Unit: Schema** | | | |
| 1 | initialize() creates reviews, review_finding_ids, telemetry tables | Tables queryable, user_version=1 | Y |
| 2 | initialize() is idempotent (second call on same DB) | No error, version still 1 | Y |
| 3 | WAL journal mode is active after init | PRAGMA journal_mode returns "wal" | N |
| 4 | Foreign keys enabled | PRAGMA foreign_keys returns 1 | N |
| **Unit: ReviewLog SQLite** | | | |
| 5 | Full ReviewRecord round-trip (all scalar fields) | Exact match on readback | Y |
| 6 | Optional fields round-trip (repo=None, lines_added=None, mode=None) | NULLs preserved | Y |
| 7 | ContextTelemetry JSON round-trip (5 fields checked) | Values survive serde | Y |
| 8 | finding_ids written to child table, rejoined on load | Order preserved | Y |
| 9 | Empty finding_ids: no child rows, empty Vec on load | Clean empty state | Y |
| 10 | suppressed_by_rule HashMap round-trip (non-empty + empty) | Exact key/value match | Partial |
| 11 | Timestamp precision: sub-second DateTime<Utc> round-trip | No truncation | N |
| 12 | u64 boundary: tokens_in=u64::MAX round-trip through i64 cast | Overflow or faithful | **N (GAP)** |
| 13 | Duplicate run_id insert (PK constraint) | Error returned | N |
| 14 | load_all returns records ordered by timestamp | Chronological order | N |
| 15 | ContextTelemetry with ALL 37 fields populated | Every field survives JSON blob | **N (GAP)** |
| **Unit: TelemetryStore SQLite** | | | |
| 16 | Full TelemetryEntry round-trip | Exact match | Y |
| 17 | fp_kind_utilization_rate=None round-trip | SQL NULL preserved | Y |
| 18 | fp_kind_utilization_rate=Some(0.0) vs None distinction | 0.0 != NULL | N |
| 19 | files=[] and findings={} (empty collections) | Empty JSON arrays/objects | N |
| 20 | Multiple entries: load order by ts | Chronological | N |
| **Integration: Migration** | | | |
| 21 | reviews.jsonl migrated, renamed to .migrated | Row count matches, file renamed | Y |
| 22 | telemetry.jsonl migrated, renamed to .migrated | Row count matches, file renamed | Y |
| 23 | Malformed lines skipped, valid lines imported | Partial import succeeds | Y |
| 24 | Idempotent: second init after migration is no-op | Count unchanged | Y |
| 25 | Double-import guard: table non-empty + JSONL present | Skip with warning | Y (in code) |
| 26 | Empty JSONL file: clean migration with 0 rows | File renamed, 0 rows | N |
| 27 | E2E: write via old JSONL API, init migrates, read via SQLite API | Full fidelity | Y |
| 28 | Real-world JSONL with serde(default) fields missing | Defaults applied correctly | **N (GAP)** |
| **Integration: Error Paths** | | | |
| 29 | Corrupt DB file: moves aside, creates fresh | user_version=1 on new DB | Y |
| 30 | Corrupt DB + existing JSONL: recovery then migration | Data from JSONL in new DB | N |
| 31 | Read-only filesystem: migration skipped gracefully | Warning, no crash | **N (GAP)** |
| 32 | json_valid CHECK constraint: invalid JSON rejected on insert | Constraint error | N |
| 33 | Lock poisoned: concurrent panic leaves mutex poisoned | Graceful error message | N |
| **Concurrency** | | | |
| 34 | Two threads writing reviews concurrently via Arc<Mutex> | Both succeed, no data loss | **N (GAP)** |
| 35 | Read during write (WAL mode) | Reader sees consistent snapshot | **N (GAP)** |
| **Schema Evolution** | | | |
| 36 | Future v2 migration: ALTER TABLE ADD COLUMN chains from v1 | New column has default, old data intact | **N (GAP)** |
| 37 | DB at version 2, code expects 1: forward-compat behavior | No crash (skip unknown) | **N (GAP)** |

## Coverage Gaps and Recommendations

**P0 -- Must have before merge:**

1. **u64 overflow via i64 cast (#12)**: tokens_in/out/duration_ms are u64 in Rust but stored as i64 in SQLite. Values above i64::MAX will silently wrap or panic. Add a test asserting behavior; consider clamping or storing as TEXT for safety.

2. **Full ContextTelemetry fidelity (#15)**: Plan tests only 5 of 37 fields. Add a test populating every field (especially nested LegCounts, Option<f32> rerank scores, Vec<String> chunk IDs) and asserting round-trip. This is the highest-risk gap because the JSON blob is opaque to SQLite.

3. **Legacy JSONL with missing fields (#28)**: Real-world JSONL files predate many fields (judge_*, context7_*, fp_kind_*). Write a test with a minimal pre-v0.18 JSON blob to confirm serde(default) fills correctly during migration.

4. **Concurrent writes (#34)**: Spawn 2 threads sharing a StorageHandle, each inserting 50 reviews. Assert final count = 100. This validates the Arc<Mutex> + WAL strategy the design relies on.

**P1 -- Should have:**

5. **Schema evolution dry run (#36)**: Write a test that runs v0-to-v1, then a synthetic v1-to-v2 ALTER TABLE ADD COLUMN, confirming old rows get the default and new rows use the new column.

6. **Timestamp ordering (#14, #20)**: The plan's load_all_sqlite uses ORDER BY timestamp/ts. Add a test inserting out-of-order and verifying sorted output.

7. **Read-only FS (#31)**: Use a temp dir with permissions set to read-only after JSONL creation. Confirm migration is skipped without crashing.

**P2 -- Nice to have:**

8. **Corrupt DB + JSONL recovery (#30)**: Combine corruption recovery with pending JSONL migration in a single init call.

9. **Forward-compat (#37)**: Set user_version=2 manually, call initialize(), verify it does not re-run v0-to-v1 and does not crash.

## Priority Ranking

| Priority | Tests | Rationale |
|----------|-------|-----------|
| P0 | 12, 15, 28, 34 | Data loss or silent corruption risks |
| P1 | 3, 4, 14, 20, 26, 30, 31, 36 | Correctness and evolution safety |
| P2 | 13, 18, 19, 32, 33, 35, 37 | Defense-in-depth, edge hardening |
