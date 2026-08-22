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

| model | findings | LLM-sourced | ground truth | high |
|-------|---------:|------------:|-------------:|-----:|
| pre-fix, any model | 7 | 0 | 0/4 | 0 |
| gpt-5.4 | 19 | 12 | 3/4 | 1 |
| gpt-5.6 | 22 | 15 | 4/4 | 1 |
| claude-opus-5 | 31 | 24 | 4/4 | 3 |

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
