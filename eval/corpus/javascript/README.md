# JavaScript corpus

## shelly_fan_control.js

Shelly Gen2 mJS (Espruino-derived) running on a Shelly Plus 2PM, controlling a
mains bathroom exhaust fan. 187 lines. Reconstructed pre-fix state of a real
script; the BLE MAC has been replaced with a placeholder (no bug depends on its
value, only on null vs non-null).

**Why this fixture exists.** quorum surfaced 7 findings on it, all info-level
(cyclomatic complexity x2, nullish-coalescing x5), and **none** of the four real
bugs. That miss is what exposed the v0.28.0-v0.29.0 regression where the skill
axis reviewer emitted zero LLM findings for two months (see CHANGELOG 0.30.0).
It then became the verification target for the fix.

**Measured results** (post-fix, `--skip-context7`, single file):

| model | $/file | out tok | findings | LLM-sourced | ground truth | high | secs |
|-------|-------:|--------:|---------:|------------:|-------------:|-----:|-----:|
| pre-fix, any model | - | - | 7 | 0 | 0/4 | 0 | 65 |
| gpt-5.4 | $0.164 | 8103 | 19 | 12 | 3/4 | 1 | 170 |
| gpt-5.5 | $0.115 | 4391 | 20 | 13 | 2/4 | 0 | 143 |
| **gpt-5.6** | $0.221 | 4545 | 22 | 15 | **4/4** | 1 | 81 |
| gpt-5.6-sol | $0.227 | 4822 | 19 | 12 | **4/4** | 2 | 165 |
| gpt-5.6-terra | $0.238* | 5181 | 18 | 11 | 1.5/4 | 0 | 132 |
| claude-opus-5 | $0.425 | 11714 | 31 | 24 | **4/4** | 3 | 159 |

`*` terra has no entry in the proxy's `/model/info`, so its price is assumed
equal to gpt-5.6 and is unverified. Both variants are reachable through the
LiteLLM `*` wildcard as `openai/gpt-5.6-{sol,terra}` with no proxy config.

Cost uses the v0.30.0 pricing table. Single run per model on this one file --
enough to pick a default, not enough to reason about tradeoffs; see punch-list
item 8b for the systematic study.

**gpt-5.6 is the default as of v0.31.0.** Same $0.055 per ground-truth bug as
gpt-5.4 but full coverage, in half the wall-clock; its 2x sticker price is
offset by ~44% fewer output tokens on a byte-identical prompt.

**gpt-5.5 is a trap.** Cheapest per file, but 2/4 and *zero* high-severity
findings: it missed both critical bugs (sfc-002 staleness guard, sfc-004 NaN
poisoning) and exited 1 rather than 2, so it would not fail a CI gate on a file
with two critical defects. Note 5.5 and 5.6 spend near-identical output tokens
(4391 vs 4545) -- 5.6 converts them into twice the coverage.

**The noise floor, measured.** `gpt-5.6` and `gpt-5.6-sol` are the same model,
so the gap between their rows *is* run-to-run variance: 3 findings, 3
LLM-sourced findings and 1 high severity. But they agree exactly on
ground-truth coverage (4/4) and exit code (2). Variance moves the decorative
numbers and leaves the load-bearing ones stable -- which is what makes the
ground-truth column trustworthy at n=1 and the raw finding count not.

**terra is a real regression, not an unlucky sample.** 1.5/4 with zero
high-severity findings sits far outside that noise band. It caught the unbounded
queue and the decoder truncation but never reached the NaN mechanism, and missed
both the staleness guard and the `JSON.parse` crash. It spent *more* output
tokens than sol to do it, and exited 1 where sol and gpt-5.6 exit 2 -- so it
would not fail a CI gate on a file with two critical defects. Same trap as
gpt-5.5. Do not use either for gated review.

**claude-opus-5 buys severity calibration**, not recall: same 4/4, but 3 high vs
1, correctly elevating sfc-004 and the LAN-writable actuation. Worth 2x on
release branches if you gate on exit 2; hard to justify per-file.

A plain single-prompt call to gpt-5.4 (no axes) finds sfc-001 and sfc-003 but
misses sfc-002 — the axis scaffolding earns its tokens on exactly the finding a
bare prompt cannot reach.

**What makes these four hard.** None are syntactic; none reproduce on a
single-pass read of one function:

- sfc-001 needs a cross-function join (producer in an event callback, consumer
  in a timer) plus reasoning about relative rates.
- sfc-002 is guard-clause *placement* relative to a state transition.
- sfc-003 depends on runtime semantics: an exception in an async callback kills
  the process rather than being logged.
- sfc-004 is NaN propagation where the existing `=== null` guard does not catch
  it.

**Precision control.** The post-fix version of the same file is a useful negative
control: it should yield 0 critical and 0 high, and must not re-report these
four. It is not vendored here; regenerate by applying the fixes described in
`shelly_fan_control.NOTES.md`.
