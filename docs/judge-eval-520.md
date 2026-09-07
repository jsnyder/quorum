# Judge evaluation — #520 part 2

Part 1 made `judge: required` real: a speculative finding is now withheld when
no judge ran. That traded noise for silence — 214 findings on a 64-file review
stopped surfacing and nothing evaluated them. Part 2 asks whether a judge can
be made good enough to adjudicate them, and answers per rule.

**Conclusion: delete all six.** Not because the judge cannot be fixed — it can,
and this change fixes it — but because a working judge rejects essentially
everything these rules emit. Every one of them is a strictly worse duplicate of
a narrow rule that already ships and encodes the discriminator in the pattern
itself, deterministically and for free.

Harness, fixtures, labels and raw results: `eval/judge520/`.

---

## What was measured

**187 labelled findings** — 162 from real code in four repositories, 25 from
synthetic fixtures — judged under three prompts, with the proxy response cache
bypassed on every call.

| label source | n | how the label was set |
|---|---:|---|
| `corpus_file_level` | 109 | the file has recorded human verdicts for that rule in `~/.quorum/feedback.jsonl`, and they are unanimous |
| `manual_verified` | 53 | read by hand for this evaluation; every one is written up below |
| `synthetic` | 25 | constructed for this evaluation, `eval/judge520/fixtures/` |

### Instrument calibration

The harness is Python, but the prompt is not a paraphrase. `run_eval.py`
reconstructs `judge::build_judge_prompt` byte for byte — including
`pick_fence_for`'s backtick counting and `serde_json::to_string_pretty`'s
alphabetical key order — and this was verified by dumping a prompt from the
Rust function for the same input and diffing. Identical. The same check was
repeated after the prompt change, confirming that what shipped is exactly the
variant that was measured.

The request body matches `judge_completion`: same system prompt, `max_tokens:
2048`, `temperature: 0`.

---

## Baseline: the judge approves what humans already rejected

Judge model `gpt-4.1-mini` (the code default; note the `--judge-model` help text
disagrees — see "Bugs found" below).

Under `judge: required`, a finding is dropped on `rejected` **or** on no verdict
at all, and kept on `approved` or `uncertain`. So the number that decides
anything is not the judge's accuracy — it is the precision of what *survives*.

| rule | labelled tp/fp | tp kept | fp dropped | **survivor precision** |
|---|---:|---:|---:|---:|
| `discarded-result` | 2/18 | 100% | 22% | **12%** |
| `jinja-loop-variable-scoping` | 2/43 | 100% | 91% | **33%** |
| `nullish-coalescing-broad` | 2/34 | 100% | 68% | **15%** |
| `string-byte-slice-broad` | 3/57 | 100% | 89% | **33%** |
| `string-format-sql` | 2/24 | 100% | 88% | **40%** |

The single-sample "approved 9 of 9" observation that prompted this work was not
a fluke. On 15 `discarded-result` findings a human had already recorded as
false, the baseline judge approved 14.

**Why it fails is visible in its own reasons.** The judge frequently returned
`tp` while explaining a false positive:

- `skill_executor.rs:1154` — *"the call returns a CellResult, which is a struct,
  not a Result type... The discarded-result rule is a false positive in this
  context."* Verdict: **tp**.
- `daily_digest_notifications.yaml:122` — *"The code correctly uses a namespace
  object... which is the recommended pattern."* Verdict: **tp**. All four jinja
  approvals were namespace() code, i.e. the correct idiom.
- `main.rs:3786` — *"Although the comment above explains that a failed shutdown
  does not un-send a body that already went out..."* Verdict: **tp**.

Two distinct defects show up here. First, the neutral question sets no bar, so a
plausible-sounding pattern gets a yes. Second, the judge grades against the
rule's *remediation advice* rather than against risk: `discarded-result`'s
message says "add a comment explaining why", so seven approvals reduce to "no
comment is present", including on `let _ = lock_file.unlock()`.

There is a third failure mode with no defensible reading: on six
`string-byte-slice-broad` findings in `skill_integrator.rs` the judge answered
`uncertain` because *"the evidence references line 1064, which is beyond the
provided code snippet"*. The file was sent in full. `uncertain` is kept, so
these reached the user.

---

## The fix: state the bar

Two variants were tested against the same 187 instances.

- **`strict`** — names the speculative provenance, asks "does the surrounding
  code show a concrete way this goes wrong at runtime?", and makes fp the
  default. Also supplies each rule's recorded track record.
- **`strict-noctx`** — identical, minus the track record.

`strict-noctx` exists because `strict` is open to an obvious objection: a judge
told "this rule has never been right" will reject, and that measures the hint,
not the prompt. Removing the hint answers it.

| rule | baseline | `strict` | `strict-noctx` |
|---|---:|---:|---:|
| `discarded-result` | 12% | 67% | **100%** |
| `jinja-loop-variable-scoping` | 33% | 100% | **100%** |
| `nullish-coalescing-broad` | 15% | 100% | **100%** |
| `string-byte-slice-broad` | 33% | 75% | **75%** |
| `string-format-sql` | 40% | 100% | **100%** |

(survivor precision; `tp kept` is 100% in every cell of this table)

The improvement comes entirely from the framing — `strict-noctx` matches or
beats `strict` everywhere. **`strict-noctx` is what shipped**, in
`judge::build_judge_prompt`, pinned by
`judge_prompt_states_the_bar_instead_of_asking_neutrally`.

It is not blanket rejection: all 11 constructed true positives were approved
under both variants, including the ones the baseline also caught.

---

## Why that settles the rules rather than saving them

Run the working judge over real code and it produces this:

**1 of 162 real-code findings survived. It is a false positive** — `&sent[0]`,
a `Vec` index, marked `uncertain` and therefore kept.

| rule | real-code findings | survived the strict judge |
|---|---:|---:|
| `discarded-result` | 15 | 0 |
| `jinja-loop-variable-scoping` | 40 | 0 |
| `nullish-coalescing-broad` | 31 | 0 |
| `string-format-sql` | 22 | 0 |
| `string-byte-slice-broad` | 54 | 1 (a false positive) |

A rule whose entire real-world output a good judge deletes is not a rule that
needs a judge. It is a rule that has nothing to say.

### Cost of keeping them anyway

Measured on a full `src/*.rs` review (64 files):

- **32 files** trigger a judge call (a call is per-file, batching that file's findings)
- **567k input tokens** of source shipped to an external model
- **~38s** added wall time at `--parallel 4` (4.8s/call measured)
- **~$0.24** per review at `gpt-4.1-mini` list price
- **216 findings** adjudicated, of which none are true positives

### Every one has a better sibling that already ships

| removed | ships instead | the discriminator the broad rule dropped |
|---|---|---|
| `string-byte-slice-broad` | `string-byte-slice` | literal numeric bounds required, so `&vec[i]` stops matching (`&buf[..4]` still fires; the narrow rule is tighter, not clean) |
| `discarded-result` | `ignored-io-result`, `discarded-fallible-result` | matches named fallible operations, so `let _ = map.insert()` stops matching (`let _ = tx.send()` still fires — `send` is on the fallible list) |
| `nullish-coalescing-broad` | `nullish-coalescing-preferred` | requires a literal default on the right — plain boolean `a \|\| b` stops matching |
| `string-format-sql` | `sql-template-injection` | requires a `.query/.execute/.raw` call **and** a `${...}` substitution |
| `jinja-loop-variable-scoping` | `ha-jinja-loop-scoped-reassignment` | adds `not: namespace(`, which is the entire bug |
| `logging-debug-leak` | — | nothing, and nothing is needed |

The narrow siblings carry the confirmed true positives (`string-byte-slice` ~5,
`nullish-coalescing-preferred` ~10, `ha-jinja-loop-scoped-reassignment` 6,
`ignored-io-result` 3) and none of them is `judge: required`.

---

## Per-rule decision

### `logging-debug-leak` — DELETE (it has never fired)

Pattern is `logging.debug($$$ARGS)`: the bare module-level call. Scanned across
**1,602 real Python files** (prompt_health, prompt_health_v2, house_memory,
home_assistant custom_components + pyscript, quorum): **0 findings**. The only
`logging.debug(` calls found anywhere were in quorum's own
`eval/corpus/python/speculative_patterns.py`. Real code uses a named logger
(`_LOGGER.debug`, `logger.debug`, `log.debug` — 43 occurrences in the same
corpus). Zero feedback verdicts, because there was never a finding to triage.
The rule works correctly on a synthetic case; it just describes an idiom nobody
writes. Nothing to judge, nothing to keep.

### `string-byte-slice-broad` — DELETE

72 recorded human verdicts, **0 correct**. 54 live findings across 12 files, 0
true positives. Under the strict judge, 1 of 54 survives and is a false
positive. The real UTF-8 panic (#197, `line[6..]` on a non-ASCII `+++ b/`
prefix) is caught by the narrow `string-byte-slice`, which is not
`judge: required` and is pinned by
`narrow_rules_still_emit_when_the_speculative_ones_are_withheld`.

### `discarded-result` — DELETE

51 recorded verdicts, 3 correct (5.9%) — and **all three of those code sites
have since been edited away**, which is why `src/parser.rs` and
`tests/no_live_calls.rs` produce zero findings for this rule today. 15 live
findings, 0 true positives. Baseline judge approved 14/15; strict judge rejects
15/15. `let _ =` is the idiomatic Rust way to *say* a Result is deliberately
ignored, so a rule that flags it flags the acknowledgement it asks for.

### `nullish-coalescing-broad` — DELETE

Zero feedback verdicts (it has never been triaged), 256 live findings across two
TypeScript repositories. 31 hand-verified here: **31/31 false positives**, and
24 of the 31 are not about defaulting at all — they are plain boolean
disjunctions (`resp.status === 429 || resp.status >= 500`,
`content.includes('import ') || content.includes('class ')`) where `??` is not
even applicable. The rule's own `not: inside if_statement` exclusion does not
work: `session-queue.ts:222` is literally `if (this.stopped || this.processing
|| ...)` and it fires anyway. In two of the sampled files the author uses `??`
correctly on the very next line (`input.confidence ?? parsed.confidence`),
having chosen `||` deliberately for the strings above it.

### `string-format-sql` — DELETE

Zero feedback verdicts, 93 live findings. 22 hand-verified: **22/22 false
positives**. Seventeen have no interpolation whatsoever — they are correctly
parameterised queries using `$1, $2` placeholders, which is precisely what the
rule's message tells you to do. Three are not SQL at all: an LLM prompt template
containing the words "UPDATE" and "SKIP", and two log messages containing
"update failed" / "delete failed". The two genuinely dynamic cases
(`observation.ts:216`, `camera-event.ts:306`) assemble the WHERE clause from
literal column names and `$N` placeholders with every value in `params` — safe,
and read line by line to confirm it.

### `jinja-loop-variable-scoping` — DELETE

8 recorded verdicts, **0 correct**. 40 live findings across 5 Home Assistant
package files, 0 true positives. Strict judge rejects 40/40. The baseline
judge's four approvals were all `namespace()` code — the *correct* idiom — and
its own stated reasons said so. `ha-jinja-loop-scoped-reassignment` is the same
rule plus `not: namespace(`, has 6 confirmed true positives, and is not
`judge: required`.

---

## What the deletion costs

The narrow siblings are better rules, but they are not supersets. Running every
surviving bundled rule over the synthetic fixtures shows which of the 11
constructed true-positive shapes still get caught:

| shape | still caught by |
|---|---|
| `&message[..40]` on user text | `string-byte-slice` |
| `${req.params.id}` in a WHERE clause | `sql-template-injection` |
| `ORDER BY ${req.query.sort}` | `sql-template-injection` |
| `cfg.retries \|\| 3` (0 is meaningful) | `nullish-coalescing-preferred` |
| `{% set total %}` in a loop, read after `{% endfor %}` | `ha-jinja-loop-scoped-reassignment` |
| `{% set found %}` used as a post-loop flag | `ha-jinja-loop-scoped-reassignment` |
| **`&name[..mid]` where `mid = name.len()/2`** | **nothing** — `string-byte-slice` requires literal numeric bounds |
| **`&name[mid..]`** | **nothing** — same |
| **`let _ = fs::write(path, body)`** | **nothing** — `ignored-io-result` matches bare statements, not `let _ =`; `discarded-fallible-result` matches method calls, not `fs::` paths |
| **`let _ = raw.parse::<u16>().map(...)`** | **nothing** — same |
| **`cfg.enabled \|\| true` (explicit `false` overridden)** | **nothing** — `nullish-coalescing-preferred` requires a number/string/array/object on the right, not a boolean |

**Six of eleven. Five constructed true-positive shapes lose their only
detector.** This is the strongest argument against the deletion and it belongs
in the record.

Two things bound it, and neither is a dismissal:

1. Those shapes were already not reaching anyone. Since #520 part 1, a
   `judge: required` finding is withheld unless `--judge` is passed, and
   `--judge` is opt-in and off by default. The deletion removes a detector that
   was already silent in normal use.
2. Across **162 real-code findings in four repositories**, not one instance of
   any of the five uncovered shapes appeared. They are real bug shapes; their
   observed frequency in this corpus is zero. That is the same standard by which
   the rules are being deleted, applied honestly in the other direction.

The two Rust gaps are the ones worth closing, and closing them is a pattern
edit, not a judge: `string-byte-slice` could accept a non-literal index
expression, and `discarded-fallible-result` could add `fs::`-path calls
alongside its method-call list. Filed as a follow-up rather than done here,
because widening a `precision: high` rule needs its own before/after on the
corpus — which is exactly the mistake this document is about.

---

## What this evaluation cannot show

Stated plainly, because a judge evaluation that scores well on a trivially
separable set is a vacuous test in a new costume.

1. **No independent ground truth.** The `corpus_file_level` labels are verdicts
   recorded by me and other agents, not by an uninvolved human. The
   `manual_verified` labels are mine. They are reproducible — the reasoning for
   all 53 is above and the evidence strings are in
   `eval/judge520/results/` — but they are not an oracle.

2. **Labels are per file+rule, not per finding.** A `corpus_file_level` label
   says "every recorded verdict for this rule in this file was fp", and is
   applied to that rule's current findings in that file. Code moves. This is
   mitigated by checking every survivor by hand: under the strict judge there
   was exactly one, and it is written up above.

3. **Every true positive in the set is synthetic.** This is structural, not an
   oversight. `discarded-result`'s three real true positives sit at code sites
   that no longer exist, so no set drawn from current source can contain one.
   The synthetic positives are therefore load-bearing for the recall column, and
   they are easy cases by construction — an unmistakable `&message[..40]` on
   user text, an unmistakable `${req.params.id}` in a WHERE clause. **A judge
   that scores 100% recall on these has not been shown to catch a subtle true
   positive.** What it has been shown is that the strict prompt does not
   blanket-reject, which is the claim the recall column is used for here.

4. **One judge model, one run.** `gpt-4.1-mini` at `temperature: 0`, one call
   per file per variant. No repeat sampling, so per-finding variance is
   unmeasured. The aggregate gaps (12% → 100%) are far larger than plausible
   single-call noise, but a 75% vs 67% comparison is not.

5. **`nullish-coalescing-broad` and `string-format-sql` are evaluated on
   TypeScript from two repositories only**, both written by the same author.
   Their false-positive modes (boolean disjunction; parameterised queries) are
   generic enough that this seems unlikely to matter, but it is a single-author
   sample.

6. **31 findings never got a verdict at all** — see the truncation bug below.
   They are counted as dropped, which is what `judge: required` does to them,
   but they are not evidence about the judge's judgment.

---

## Bugs found along the way

Filed separately; none are fixed by this change.

1. **The judge silently drops every finding in a large file.** `judge_findings`
   batches all of a file's findings into one call, and `judge_completion` sets
   `max_tokens: 2048` with no chunking. On `src/calibrator.rs` (31 findings) the
   response came back `finish_reason: "length"` and **all 31 findings got no
   verdict**. Under `judge: required` they are then withheld — so the judge
   turns into a silent deleter on exactly the files with the most to say about.
   `judge.rs` warns at >50 findings but does not act on it.

2. **`QUORUM_BYPASS_PROXY_CACHE` never reaches the judge.** Three other request
   builders in `llm_client.rs` set `body["cache"] = {"no-cache": true}`;
   `judge_completion` does not. Any judge A/B run through the documented
   procedure silently compares cached replays. The harness in `eval/judge520/`
   sets the field itself to work around this.

3. **`--judge-model` help text is wrong.** It says the default is `gpt-5-nano`;
   `main.rs` uses `gpt-4.1-mini`.

4. **`all_bundled_rules_match_fixtures` is weaker than it reads.** It asserts a
   fixture produces *some* finding, not that it matches *its own* rule. Five of
   the six fixtures deleted here would have kept passing on unrelated rules;
   only the Python one went red.

---

## Reproducing

The six rules are vendored at their `438c04b` contents in
`eval/judge520/rules/`, so the measurement behind their deletion can still be
re-run after they left the bundled set.

```bash
export QUORUM_BASE_URL=... QUORUM_API_KEY=...
cd eval/judge520
python3 run_eval.py --manifest manifest.json --variant baseline     --out results/baseline.jsonl
python3 run_eval.py --manifest manifest.json --variant strict-noctx --out results/strict_noctx.jsonl
python3 score.py results/baseline.jsonl results/strict_noctx.jsonl
```

`--dump-prompt <file>` writes the first prompt and makes no API calls; use it to
re-verify against `build_judge_prompt` after any prompt edit.

Total spend for every run reported here: 93 calls, ~1.03M input tokens,
~41k output tokens.

---

## The judge subsystem now has no users

Deleting these six leaves **zero rules declaring `judge: required`**, and there
were never any declaring `judge: optional`. `--judge` still works, the
`JudgeRequirement` metadata still parses, `enforce_judge_required` still holds
the contract for any future speculative rule, and the prompt is now one that
does not approve everything.

Whether to keep the subsystem at all is a separate call, and this evaluation
does not make it. Keeping it costs nothing while unused; the argument for
keeping it is that the next speculative rule someone writes inherits a judge
that works. The argument against is that this evaluation is a fair sample of
what speculative pattern rules are worth, and the answer was "delete them all"
six times out of six.
