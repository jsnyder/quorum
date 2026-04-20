# Feedback Mining — 2026-04-19
Window: last 60d human-verified | TP=639 FP=295

## TP Title Clusters (recurring, >=2 occurrences)

These are titles LLM reviewers emitted that humans confirmed as TP more than once. Candidates for promotion to ast-grep rules if they have a clear syntactic signature.

### typescript (182 TPs, 4 recurring titles)

- **2×** Duplicated session continuation logic identical to chat.ts
- **2×** Role casting without validation
- **2×** console.log debug artifact left in code
- **2×** [⚠ also FP 1x] Empty catch block silently swallows errors

## TP Singletons (top 30 per lang) — raw LLM output for mining

### bash (15 singletons — showing 30)

- Unused LOG_FILE variable indicates dead code or incomplete logging
- Idempotency check is race-prone and can trigger duplicate ingestions
- session_id is interpolated into URL without encoding
- Lock file creation is non-atomic and allows concurrent checkpoint runs
- Full-directory find on every hook invocation can violate the never block goal
- Lock file is leaked on early exits, blocking future ingestion for the session
- Predictable /tmp filenames derived from untrusted session_id enable file clobbering/symlink attacks
- Predictable files in /tmp create symlink and race-condition risks
- grep -oP not portable on macOS
- Relative path to backfill-episodes.sh
- Regex injection in grep project check
- Broad grep matching for project coverage
- Percentile formula int(n*q) is off-by-one; use nearest-rank ceil(q*n)-1
- Manual JSON string concatenation is brittle to quotes in URL/query_file/captured_at
- Silent `|| continue` hides failed queries; no skip count surfaced

### javascript (13 singletons — showing 30)

- Event listener is never unsubscribed because `.bind(this)` creates a new function
- Event listeners are never removed because bound handler references differ
- Event listener is not removed because `bind(this)` creates a different function reference
- Update listener cannot be removed because `bind()` creates a new function
- Event listeners cannot be removed because `.bind(this)` creates new function instances
- Event listeners cannot be removed because `.bind(this)` creates new function objects
- Engine can be left in a broken running state if startup async work fails
- Chunk completion path is not exception-safe and can strand jobs as permanently busy
- Errors during async world generation leave the promise unresolved and player state inconsistent
- ASCII parser strips leading and trailing spaces from every row
- Lake generation uses undefined `this.waterLevel`, producing invalid heights
- Wall-sliding logic loses the attempted Z movement due to aliasing `camera.position`
- Transparent pixels are forced opaque because alpha uses `|| 255`

### other (6 singletons — showing 30)

- episode-store.ts store method iterates and calls upsert individually
- context.rs only accepts data as top-level array, lost data.results fallback
- session-episodes.ts nested objects lack additionalProperties:false
- enrichment_status can still be NULL despite having a default and CHECK constraint
- Casting tags to JSONB can abort the trigger and backfill on invalid data
- Winner selection is nondeterministic when length and captured_at are tied

### python (203 singletons — showing 30)

- np.load without disabling pickle support
- Path traversal in profile filenames
- Zero-norm division in normalization
- meta.json load crash with no error handling
- Name handling inconsistent across persistence path
- DiarizedSegment with start=0.0 end=0.0 fake timestamps
- Threshold fields not range-validated
- Assignee normalization inconsistency
- SAFETY_GROUPS bypass is documented but not implemented
- IndexError on empty results via results_by_group.get fallback
- ffmpeg process leak on timeout — returns None without killing subprocess
- yaml.safe_load(None) crash on empty config file
- LLMJudgeConfig allows enabled=true without api_key
- NoiseStressConfig has no range validation for divisors
- Serialized per-entity HTTP polling — O(n) latency scaling
- Poll interval drift — sleeps after cycle instead of targeting monotonic deadline
- start()/stop() lifecycle — non-idempotent start, stale task ref after stop
- Event buffer grows unbounded between compute() calls
- Scoring formula mismatch between _compute_events and _get_top_stressor
- start() not idempotent — duplicate polling tasks on repeated calls
- Unhandled wave.open exception on malformed WAV upload
- No error handling around inference pipeline
- assert for runtime integrity check — disabled under python -O
- The MQTT client shutdown order is reversed
- speech_on() reuses the existing session until check_expired() closes it
- The save/load path does not preserve the canonical profile name
- The comment states INFO logging but implementation uses debug
- dynamic_range_db is computed but not used in scoring
- _fetch_one returns None on non-200 without logging
- SQL injection in login query via username interpolation

### rust (128 singletons — showing 30)

- error-reporting path panics because it calls unwrap()
- read_token_file checks Unix file permissions but only prints a warning
- read_token_file converts fs::read_to_string failures into None
- client interpolates raw path and query values directly into request URLs
- chrono_offset calls duration_since(UNIX_EPOCH).unwrap()
- client repeats nearly identical non-success status and error-body handling
- client sets a default CONTENT_TYPE header to application/json
- pretty-print branch uses unwrap_or_default()
- compact JSON branch uses unwrap_or_default()
- non-compact entity-list path builds Value::Array(entities.to_vec())
- conversion from reqwest::Error stores err.to_string() in Connection variant
- From<reqwest::Error> maps HTTP errors to AppError::Http with err.to_string()
- code() mapping returns AUTH_FAILED for both MissingUrl and MissingToken
- regex validation helper prepends (?i) to every user-supplied pattern
- maximum regex length check uses pattern.len() which counts UTF-8 bytes
- area filter is implemented with the same matches_entity() path used for generic text pattern filter
- api_timeout parsed without range or finiteness validation
- Relative-time branch can overflow and panic with extreme i64 values
- Parser identifies relative expressions with broad contains(ago)
- Test module does not exercise risky branches
- Error impl does not expose source for Other variant
- No From<anyhow::Error> conversion
- Timeout errors reported with hardcoded 0.0
- Non-JSON error body discarded for 4xx/5xx
- Global AtomicBool spinner interferes across instances
- Add subcommand does not require any text content
- cmd_add ignores result of create_reminder
- Tag parsing preserves empty entries
- The test sessions_ingest_transcript_filters_credentials does not effectively verify that credentials are filtered
- Threshold is not validated, allowing all findings to collapse into one

### typescript (174 singletons — showing 30)

- Telemetry is enabled by default
- append() reads the full session file, mutates session.messages, and writes the whole JSON back. If two requests append to the same session at nearly the same time, both can read the same pre-update st
- get() catches all exceptions from readFileSync/JSON.parse and returns null with no logging.
- All public methods are async, but they use blocking APIs such as existsSync, mkdirSync, readFileSync, writeFileSync, readdirSync, and unlinkSync.
- The code updates session.updatedAt on every append(), but both get() and cleanup() enforce TTL using session.createdAt + this.ttlMs.
- Unlike get() and cleanup(), append() does not catch read/parse/write errors.
- Calibration results are correlated back to findings using free-form original_message text instead of a stable identifier.
- handleAxCalibration() does not validate that the model returned one evaluation per input finding.
- For adjusted verdicts, the code only increments adjusted if adjustedSeverity is one of critical|high|medium|low|info; otherwise the evaluation is dropped silently.
- When replaying session history, the code casts arbitrary persisted roles with msg.role as user | assistant.
- Session persistence is split into two sequential append() calls. If the first succeeds and the second throws, the function rejects after partially persisting.
- buildSynthesisPrompt() embeds fr.summary and each finding message verbatim into the system prompt. Those fields come from prior model output rather than trusted constants, so they can contain instruct
- After the synthesis call, the code assumes synthesisResult.usage.totalTokens always exists.
- The code parses provider JSON and directly assigns parsed.crossFileFindings without validating that it is actually an array of Finding objects.
- All aspect passes are executed through Promise.all(passPromises). If one model request rejects, Promise.all rejects and discards all other completed pass results.
- Synthesis findings are appended after the main deduplication pass and are never deduplicated against existing findings.
- The synthesis gate uses if (runSynthesis && deduplicated.length >= 0), but deduplicated.length >= 0 is always true for arrays.
- loadOptimizedProgram() reads optimized-signature.json and passes data.demos directly to program.setDemos() after only checking that data.demos.length > 0.
- Two catch blocks suppress errors without any logging: loadOptimizedProgram() and logReviewTelemetry().
- The session persistence path suppresses all errors without any logging or telemetry.
- Calibration requests are not fully reflected in the returned metadata. metadata.model is still populated from resolvedModel.
- Diff scoping relies on an exact map lookup. If parseDiff stores paths in a normalized form that does not exactly equal filePath, changedLines remains null.
- Model-produced line numbers are accepted verbatim and shifted by chunk.startLine - 1. There is no validation that the line is positive or within the chunk.
- doFetch() constructs the endpoint with raw string concatenation. If baseUrl already ends with / or includes a path suffix such as /v1, this can generate doubled slashes.
- The timeout is implemented via AbortController, but doFetch() catches all fetch errors and wraps them as Network error. AbortError is indistinguishable from other network failures.
- The Ollama branch appends /v1 with simple string concatenation that can produce double slashes.
- THIRD_OPINION_BASE_URL and OLLAMA_BASE_URL are accepted verbatim without any validation.
- Chunk.startLine is documented as 1-based line number where this chunk starts in the original file, but chunking is done on the redacted text.
- fetchFrameworkDocs() returns result.content verbatim with no length cap.
- load() sets this.loaded = true before any file reads/parsing occur, then swallows parse/read errors.

### yaml (92 singletons — showing 30)

- this.attributes.last_triggered unreliable in automation conditions
- Silent defaults mask sensor failures with plausible values
- Room temperature float(0) silently disables alerts
- PIR effectively disabled - radar_available never goes false
- Comment says 2min cooldown, code uses 5sec threshold
- Privacy mode reads nonexistent open_sensors attribute
- Short cycling detection broken - runtime = 0 when idle
- Delta T 30-day average is hardcoded placeholder 18.5
- Availability vs state health check inconsistency
- HVAC lockout restore overwrites manual thermostat changes
- as_datetime() not guarded against malformed timestamps
- max_tokens increase from 500 to 2000
- Outing start automation triggers when either tracker leaves home without stabilizing guard
- Media suppression treats unavailable as active via not is_state off
- Calendar response indexing fragile - dict key lookup raises before default runs
- LLM call missing continue_on_error
- motion_only can go negative
- Buzzer not guaranteed off if tier3 interrupted
- stale_list Jinja loop variable scoping bug
- notify.persistent_notification vs persistent_notification.create inconsistency
- Leak alert throttle uses .last_changed instead of stored datetime value
- current_threshold attribute does not exist on binary_sensor
- Monthly summary reads post-reset meter on day 1
- Leak volume can go negative after meter reset
- Both calendar update automations try to build a list inside a Jinja for loop using dates = dates + [event_date]
- The task-count and task-list templates use the same loop-local reassignment pattern
- Both date-update automations index directly into the response object
- The weekly_activity_counter_reset automation startup condition restricts to 00:01 window
- The automation references sensor.fresh_air_available but the sensor is named Fresh Air Available Optimized
- Fresh Air Available Optimized converts indoor PM2.5 with float(0) treating unavailable as clean air

