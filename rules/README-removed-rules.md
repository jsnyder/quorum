# Removed rules

Rules deleted from the bundled set, with the evidence, so they are not
reintroduced by intuition.

## missing-await (removed 2026-08-23)

```yaml
rule:
  kind: call
  inside:
    kind: expression_statement
    inside: { kind: function_definition, regex: "^\\s*async\\s+def\\b", stopBy: end }
  not:
    inside: { kind: await, stopBy: neighbor }
```

**Why it was wrong.** It matched *every* bare call inside an `async def` that
was not awaited -- `print(x)`, `logger.info(...)`, `buf.append(...)`, all of it.
Deciding whether a callee is a coroutine function requires resolving the
callee's definition, which is semantic analysis. ast-grep has no type
information, so this is not fixable as a syntactic pattern.

**Measured impact.** ~40 findings on a single 208-line Python diff of an
async-heavy service, nearly all `[pre]`. Together with `assert-in-prod-code`
firing on test files, the two accounted for roughly 70% of raw findings on that
review (107 raw -> ~30 substantive).

**Why not keep it as speculative.** It was already marked
`precision: speculative, judge: required`, and enforcing that contract (see the
same-dated change to `judge.rs`) does stop it reaching users unjudged. But its
pre-judge precision is roughly 2%, so every async file would ship dozens of
candidates to the judge purely to be rejected. That is not a speculative rule,
it is a judge-work generator.

**The narrow version, if this ever comes back.** A fixed allowlist of stdlib
coroutines that must always be awaited -- `asyncio.sleep`, `asyncio.gather`,
`asyncio.wait`, `asyncio.wait_for` -- is genuinely high precision: a bare
`asyncio.sleep(1)` is always a bug. It was not built because the yield is close
to zero on real code (it is a beginner error), and a rule must clear two bars,
not one: high precision AND actionable often enough to be worth maintaining.

Revisit only with a concrete pattern observed in real code, not from intuition.

---

## The six `judge: required` rules (removed 2026-09-06, #520 part 2)

`logging-debug-leak` (python), `discarded-result` and `string-byte-slice-broad`
(rust), `nullish-coalescing-broad` and `string-format-sql` (typescript),
`jinja-loop-variable-scoping` (yaml).

These were the entire population of `precision: speculative, judge: required`.
Part 1 of #520 made that contract real -- speculative findings are now withheld
when no judge ran. Part 2 asked whether a good judge could make them useful.
It cannot, and the measurement is in `docs/judge-eval-520.md`.

**Every one of them is a strictly worse duplicate of a rule that already
ships.** The narrow sibling encodes the discriminator in the pattern itself,
deterministically and for free; the broad version omits it and delegates the
same decision to an LLM on every review.

| removed | ships instead | the discriminator the broad rule dropped |
|---|---|---|
| `string-byte-slice-broad` | `string-byte-slice` | requires literal numeric bounds, so `&vec[i]` stops matching |
| `discarded-result` | `ignored-io-result`, `discarded-fallible-result` | match named fallible operations, so `let _ = map.insert()` / `file.unlock()` stop matching |
| `nullish-coalescing-broad` | `nullish-coalescing-preferred` | requires a literal default on the right, so plain boolean `a \|\| b` stops matching |
| `string-format-sql` | `sql-template-injection` | requires a `.query/.execute/.raw` call **and** a `${...}` substitution |
| `jinja-loop-variable-scoping` | `ha-jinja-loop-scoped-reassignment` | adds `not: namespace(`, which is the whole bug |
| `logging-debug-leak` | -- | nothing, and nothing is needed |

**Measured.** Over 187 labelled findings (162 from real code across four
repositories, 25 synthetic), with the improved judge prompt from the same
change: **1 of 162 real-code findings survived the judge, and it was a false
positive.** All 11 constructed true positives survived, so this is the judge
working, not the judge blanket-rejecting. Feedback corpus agrees: 131 recorded
human verdicts across these rules, 3 correct, and all 3 sites have since been
edited away.

`logging-debug-leak` is a separate case: its pattern is `logging.debug($$$ARGS)`,
the bare module-level call. It produced **0 findings across 1,602 real Python
files**; the only `logging.debug(` calls found anywhere were in quorum's own
`eval/corpus/` fixture. Real code uses a named logger. It has never emitted a
finding, so there was never anything to judge.

**Cost of keeping them.** On a full `src/*.rs` review, 32 of 64 files would
trigger a judge call, sending 567k tokens of source to an external model and
adding ~38s, to adjudicate 216 findings of which none are true positives.

**What the deletion costs, stated so it is not rediscovered as a surprise.**
The narrow siblings are not supersets. Of 11 constructed true-positive shapes,
6 are still caught and **5 lose their only detector**: `&s[..mid]` and
`&s[mid..]` where the index is a computed expression rather than a literal;
`let _ = fs::write(..)` and other `fs::`-path calls bound with `let _ =`; and
`cfg.enabled || true`, where an explicit `false` is overridden. Those shapes
were already silent in normal use (`--judge` is opt-in and off by default, so
`judge: required` findings were withheld), and none of the five appeared once
across 162 real-code findings in four repositories. The two Rust gaps are
closable with a pattern edit to `string-byte-slice` and
`discarded-fallible-result`; see #537.

Revisit only with a concrete pattern observed in real code, not from intuition.
