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
