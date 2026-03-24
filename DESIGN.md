# Design System: quorum

## 1. Philosophy

The CLI follows the [Command Line Interface Guidelines](https://clig.dev/) and draws its visual language from `cargo`, `gh`, and `ruff` -- tools known for clean information hierarchy with minimal visual cues.

**The output is subtle and professional -- no emoji overload, no gratuitous gradients, just clean information hierarchy with minimal visual cues.**

Core principles:

- **Color encodes meaning, not decoration.** Green means pass, red means critical, yellow means warning, dim means secondary. Nothing else gets color.
- **Respect the user's environment.** Honor `NO_COLOR`, `TERM=dumb`, and pipe detection. Never assume a rich terminal.
- **Data goes to stdout, status goes to stderr.** Spinners, progress, and errors write to stderr. Structured output writes to stdout. This makes piping reliable.
- **Three audiences, three modes.** Humans get styled output. Machines get JSON. Auto-detect when possible.
- **Start fast.** Cold start under 50ms. The LLM call is the bottleneck, not the tool.

---

## 2. Output Modes

| Mode | When | Format |
|------|------|--------|
| Human | stdout is a TTY | Styled findings, colored severity, headers |
| JSON | `--json` flag or stdout is piped | Pretty-printed JSON |

Detection logic:

```
if --json flag OR !stdout.is_terminal() -> JSON
else -> Human
```

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

## 11. Anti-patterns

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
