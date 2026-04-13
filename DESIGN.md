# Design System: quorum

## 1. Philosophy

The CLI follows the [Command Line Interface Guidelines](https://clig.dev/) and draws its visual language from `cargo`, `gh`, and `ruff` -- tools known for clean information hierarchy with minimal visual cues.

**The output is subtle and professional -- no emoji overload, no gratuitous gradients, just clean information hierarchy with minimal visual cues.**

**Three audiences, three modes:** humans at a terminal, LLMs reading output in context windows, and machines piping JSON.

Core principles:

- **Color encodes meaning, not decoration.** Green means pass, red means critical, yellow means warning, dim means secondary. Nothing else gets color.
- **Respect the user's environment.** Honor `NO_COLOR`, `TERM=dumb`, and pipe detection. Never assume a rich terminal.
- **Data goes to stdout, status goes to stderr.** Spinners, progress, and errors write to stderr. Structured output writes to stdout. This makes piping reliable.
- **Token-efficient by default.** When consumed by LLMs (detected via `CLAUDE_CODE` env or `--compact`), output is aggressively compressed for minimal context window impact.
- **Start fast.** Cold start under 50ms. The LLM call is the bottleneck, not the tool.

---

## 2. Output Modes

| Mode | When | Format | Audience |
|------|------|--------|----------|
| Human | stdout is a TTY, no flags | Styled findings, colored severity, headers | Terminal user |
| Compact | `--compact` or `CLAUDE_CODE` env set | Token-optimized single-line records | LLM context window |
| JSON | `--json` flag or stdout is piped | Machine-parseable JSON | Scripts, pipes |

Detection logic:

```
if --json flag OR !stdout.is_terminal() -> JSON
else if --compact or CLAUDE_CODE env    -> Compact
else                                    -> Human
```

### Compact mode design (critical for LLM consumption)

When output is destined for an LLM's context window, every token matters. The compact format follows these rules:

1. **One finding per line.** Severity, category, location, and title on a single line separated by `|`.
2. **No headers, separators, or decorations.** Zero chrome tokens.
3. **Truncate aggressively.** Titles/descriptions capped at ~80 chars with `...`.
4. **Source attribution inline.** Model names abbreviated after the title.
5. **Numeric precision matches utility.** Scores to 2 decimals, line numbers as integers.

Example (single file review):
```
!|security|L42|Unvalidated input to SQL query|gpt-5.4,ast
~|security|L89|Session token not rotated after priv change|gpt-5.4
-|style|L15|Consider extracting validation logic|ast
3 findings (1 critical, 1 warning, 1 info)
```

Example (multi-file review):
```
src/auth.rs: !|security|L42|Unvalidated input to SQL query ~|security|L89|Session token not rotated
src/db.rs: ~|resource|L23|Connection pool not bounded
src/main.rs: clean
3 findings across 3 files (1C 1W 1I)
```

Example (stats):
```
feedback:2230 precision:0.74 tp:1412 fp:498 models:3 reviewed:847 files calibrated:12.1k findings
```

### Token budget guidelines

| Output type | Target tokens | Strategy |
|-------------|--------------|----------|
| Single finding | < 30 | One-line `sev\|cat\|line\|title\|source` |
| File review (5 findings) | < 200 | One line per finding, summary line |
| Multi-file summary (10 files) | < 400 | File: findings on single line, summary |
| Stats dashboard | < 100 | Key-value pairs, abbreviated counts |
| Error messages | < 50 | Error + suggestion, no stack traces |

### Abbreviations for compact mode

| Full | Compact | Context |
|------|---------|---------|
| critical | C | Severity count |
| warning | W | Severity count |
| info | I | Severity count |
| security | security | Category (keep -- already short) |
| local-ast | ast | Source |
| auto-calibrate | auto | Source |

---

## 3. Color Rules

### When to disable color

All of these disable color independently:

1. stdout is not a TTY (piped to another program)
2. `NO_COLOR` environment variable is set and non-empty ([no-color.org](https://no-color.org))
3. `TERM=dumb`
4. `--no-color` flag

Check stdout and stderr independently -- if stdout is piped, stderr can still show a colored spinner.

### Color palette

Only ANSI 16 colors. No 256-color or truecolor -- maximum terminal compatibility.

| ANSI Code | Meaning | Usage |
|-----------|---------|-------|
| `\x1b[32m` | Green | Pass, clean review, no findings |
| `\x1b[31m` | Red | Critical severity, error |
| `\x1b[33m` | Yellow | High/medium severity, warning |
| `\x1b[2m` | Dim | Secondary info, labels, separators, metadata |
| `\x1b[1m` | Bold | Headers, finding titles |
| `\x1b[0m` | Reset | Always follows a color code |

No cyan, magenta, blue, or background colors. Restraint is the aesthetic.

### Implementation

Single `Style` struct detected once at startup:

```rust
pub struct Style {
    pub dim: &'static str,
    pub bold: &'static str,
    pub green: &'static str,
    pub red: &'static str,
    pub yellow: &'static str,
    pub reset: &'static str,
}
```

When color is disabled, all fields are empty strings. Zero-cost, no per-call branching.

---

## 4. Typography & Icons

### Section headers

Diamond marker, bold:

```
~ Review: src/auth.rs
```

### Severity icons

| Icon | Meaning | Color |
|------|---------|-------|
| `!` | Critical finding | Red |
| `~` | Warning finding | Yellow |
| `-` | Info/style finding | Dim |
| `=` | Pass / clean | Green |

### Labels

Dim, fixed-width, left-aligned:

```
  Severity      high
  Category      security
  Source        gpt-5.4
  Line          42-58
```

The label is dim. The value is default color. Two spaces of indent.

---

## 5. Finding Display

### Single finding

```
  ! Unvalidated input passed to SQL query                          [security] L42
    User input from request.query flows to db.execute() without
    sanitization. Use parameterized queries.
```

Rules:
- Severity icon is colored (red/yellow/dim)
- Title is bold
- Category tag in brackets, right-aligned with line number
- Description is default color, indented, wrapped at terminal width
- Two spaces of left indent throughout

### Review summary

```
~ Review: src/auth.rs

  ! Unvalidated input passed to SQL query                          [security] L42
    User input from request.query flows to db.execute() without
    sanitization. Use parameterized queries.

  ~ Session token not rotated after privilege change               [security] L89
    After role elevation, the existing session token persists.
    Rotate tokens on privilege change to prevent session fixation.

  - Consider extracting validation logic                             [style] L15
    The validate_user function handles both parsing and validation.
    Separating concerns would improve testability.

  3 findings (1 critical, 1 warning, 1 info)
```

Count line at bottom is dim.

### Clean review

```
~ Review: src/auth.rs

  = No findings.
```

---

## 6. Spinner

Braille spinner on stderr during LLM calls and analysis. Dim. Hidden when stderr is not a TTY.

```
~ Analyzing src/auth.rs...
```

Frames: `. .. ...` (simple dots, not braille -- fits the understated aesthetic)

Behavior:
- Write to stderr only (never pollute stdout)
- Clear line on stop (`\r\x1b[K`)
- Auto-disabled when stderr is not a TTY
- Show which phase: "Parsing...", "Reviewing (gpt-5.4)...", "Reviewing (claude)...", "Calibrating..."

---

## 7. Errors

Errors go to stderr. Always human-readable. Guide toward resolution.

### Format

```
Error: Could not connect to https://litellm.example.com. Is QUORUM_BASE_URL correct?
```

```
Error: API Error (401): Invalid API key
```

"Error:" is red (if stderr is a TTY). The message is default color.

### Rules from clig.dev

- Catch errors and rewrite them for humans -- no raw stack traces
- Suggest fixes when possible ("Set QUORUM_API_KEY or pass --api-key")
- Don't treat stderr like a log file -- no `ERR`, `WARN` prefixes
- Exit with non-zero status on error

---

## 8. Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Clean review (no findings, or info-only) |
| 1 | Findings with warnings or higher |
| 2 | Critical findings |
| 3 | Tool error (config, network, parse failure) |

This makes `quorum review src/*.rs && echo "clean"` work in CI.

---

## 9. Multi-File Review

```
~ Review: 3 files

  src/auth.rs (2 findings)

    ! Unvalidated input passed to SQL query                        [security] L42
      ...

    ~ Session token not rotated after privilege change             [security] L89
      ...

  src/db.rs (1 finding)

    ~ Connection pool not bounded                                [resource] L23
      ...

  src/main.rs
    = No findings.

  3 findings across 3 files (1 critical, 1 warning, 1 info)
```

Files with findings listed first, clean files last. Summary at bottom.

---

## 10. Provenance Display

When `--verbose` or `--provenance`:

```
  ! Unvalidated input passed to SQL query                          [security] L42
    User input from request.query flows to db.execute() without
    sanitization. Use parameterized queries.
    Source        gpt-5.4, local-ast
    Precedent     TP: similar finding in auth_handler.py (2026-03-15)
    Calibrator    confirmed (high confidence)
```

Provenance details are dim, only shown when requested.

---

## 11. Numeric Formatting

| Range | Format | Example |
|-------|--------|---------|
| < 1,000 | Raw number | `842` |
| 1,000 - 999,999 | `N.Nk` | `63.1k` |
| >= 1,000,000 | `N.NM` | `1.2M` |
| Scores/precision | 2 decimal places | `0.74` |
| Line numbers | Integer, `L` prefix | `L42` |
| Percentages | Integer + % | `88%` |
| Duration | Integer + unit | `1318ms`, `4.2s` |

---

## 12. Stats Dashboard

`quorum stats` displays feedback effectiveness and usage metrics. Data sourced from `~/.quorum/feedback.jsonl` and review telemetry.

### Metrics

**Feedback health:**

| Metric | What it measures | Why it matters |
|--------|-----------------|----------------|
| Total feedback entries | Size of the feedback corpus | Calibration quality scales with data |
| Precision by model | (TP + Partial) / (TP + Partial + FP) per model | Shows which models produce actionable findings |
| TP/FP/Partial/Wontfix breakdown | Verdict distribution | Identifies if a model is noisy vs useful |
| Precision trend (30d rolling) | Precision computed over sliding windows | Shows whether calibration is improving over time |
| Feedback velocity | Entries per week | Indicates engagement with the feedback loop |

**Review activity:**

| Metric | What it measures | Why it matters |
|--------|-----------------|----------------|
| Reviews run (total, 7d, 30d) | How often the tool is used | Basic adoption signal |
| Findings per review (mean) | Average density of findings | Trending down = codebase improving or model drifting |
| Suppression rate | % of findings killed by calibrator | Too high = over-fitting to past feedback |
| Categories reviewed | Distribution across security, style, etc. | Shows coverage breadth |

**LLM spend:**

| Metric | What it measures | Why it matters |
|--------|-----------------|----------------|
| Tokens in/out (total, 7d) | Prompt and completion token counts | Cost visibility |
| Estimated cost (7d, 30d) | Token counts x model pricing | Budget tracking |
| Tokens per finding | Cost efficiency of review | Higher = model is verbose or files are large |
| Cache hit rate | Parse cache effectiveness (daemon) | Measures warm-cache benefit |

### Display

Human mode:
```
~ Quorum Stats

  Feedback (2,230 entries)
    precision    0.74    tp    1,412    fp    498    partial    214    wontfix    106
    30d trend    0.71 -> 0.74 -> 0.77

  Activity (7d)
    reviews      23      findings/review    4.2    suppressed    18%
    top category security (41%)    style (28%)    resource (19%)

  Spend (7d)
    tokens in    842k    tokens out    126k    est. cost    $2.14
    tokens/finding    1.8k    cache hits    67%
```

Compact mode:
```
feedback:2230 precision:0.74 tp:1412 fp:498 trend:0.71>0.74>0.77 reviews-7d:23 findings/rev:4.2 suppressed:18% spend-7d:$2.14 tokens/finding:1.8k cache:67%
```

### Precision trend methodology

Precision trending is computed over fixed calendar windows (e.g., weekly) rather than per-review, because quorum reviews different subsets of code each time. A single review of a well-maintained module will naturally have fewer findings than a review of legacy code -- this doesn't mean overall quality changed.

To make trends meaningful:
- Window by calendar week, not by review count
- Require minimum 10 feedback entries per window to report
- Show the trend line, not point estimates ("0.71 -> 0.74 -> 0.77")
- Don't extrapolate or forecast -- just show what happened

### Data sources

- **Feedback entries**: `~/.quorum/feedback.jsonl` (already persisted)
- **Review telemetry**: `~/.quorum/telemetry.jsonl` (new -- append-only log of review runs)
  - Fields: timestamp, files reviewed, finding count by severity, model(s) used, tokens in/out, duration, calibrator actions
- **No network calls**: stats computed entirely from local data

### Telemetry design

Review telemetry is opt-in, local-only, and append-only:

```jsonl
{"ts":"2026-04-05T10:23:00Z","files":["src/auth.rs"],"findings":{"critical":1,"warning":1,"info":1},"model":"gpt-5.4","tokens_in":4200,"tokens_out":1800,"duration_ms":3400,"suppressed":2}
```

No file contents, no finding text, no code snippets. Just counts and metadata. The telemetry file can be deleted at any time with no impact on functionality.

---

## 13. Anti-patterns

Things this CLI will never do:

- Rainbow text or gratuitous color
- Box-drawing table borders
- Emoji in data output
- Progress bars (spinner is sufficient for LLM calls)
- Animated transitions
- ASCII art banners
- Log-level prefixes (ERR, WARN, INFO)
- Verbose success messages ("Successfully completed review of...")
- Color in JSON output mode
- Slow startup (target: <50ms to first spinner)
- Phone-home telemetry (all metrics are local-only)
