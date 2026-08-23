> NOTE: line references below were retargeted to the vendored copy in this
> directory (187 lines). They originally referenced the author's working
> file and did not match. Ground truth in `shelly_fan_control.ground_truth.json`
> is authoritative and verified against the vendored file.

# Recall fixture: bathroom-fan-humidity.PREFIX.js

Shelly Gen2 mJS (Espruino-derived) running on a Shelly Plus 2PM, fw 1.7.5,
controlling a mains bathroom exhaust fan on switch:1. 187 lines.

Reconstructed from session scrollback -- the file was untracked, so there is no
git history. It is the exact state reviewed when the four findings below were
produced. Syntax-verified. FIXED.js is the current post-fix version for diffing.

Ground truth: 4 real bugs. quorum surfaced 0 of them (7 findings, all info-level:
cyclomatic complexity on decode/tick, 5x nullish-coalescing-broad).

---

## 1. CRITICAL -- unbounded queue growth (L81, L86, L120)

`queue()` pushes to `dirty[]` whenever the value differs, with no check for the
key already being queued. `queue("ble_stats", JSON.stringify(stats))` runs on
every advert carrying service_data, and `stats.adverts` increments each time, so
the value ALWAYS differs and the key is pushed every advert -- duplicates included.

The drain timer removes one entry per 2s. Measured advert rate on this device was
~740 per 45s. Growth exceeds drain by ~3 orders of magnitude.

Outcome: heap exhaustion, script death within hours. If it dies mid-run the fan
latches ON. Secondary: a flash KVS.Set every 2s forever (~43k NVS writes/day),
`pending{}` never deletes keys, and the `sd_*` discovery writes at L120 are
not gated on `CFG.mac`, so rotating-MAC BLE devices keep minting new keys.

Fix applied: dedupe on `dirty.indexOf(k) < 0`, gate discovery writes on
`CFG.mac === null`, drop the per-advert stats write.

## 2. CRITICAL -- staleness guard sits above all turn-off logic (L138)

`if (now() - lastSeen > CFG.staleS) return;` is placed before every off path.
If the BLU H&T battery dies while a script-started run is active, every
subsequent tick returns early: no fallOff check, no maxRunS ceiling, no demand
expiry. The fan runs until a human intervenes.

The guard should gate turn-ON only. This is the one that would actually have
burned us in service.

Fix applied: restructured tick() so the running/turn-off block precedes the
freshness check, with a `ranFor >= maxRunS` escape inside the stale branch.

## 3. HIGH -- uncaught JSON.parse on external input in an async callback (L95-98)

`readDemand()` parses a KVS value written by any host on the LAN. In this runtime
an exception inside an async callback terminates the entire script. One malformed
write -- or a non-string value -- kills the controller, again potentially mid-run
with the fan on.

try/catch IS supported in Shelly mJS and is absent here.

Fix applied: try/catch, plus `isNum()` validation and a clamp to `now() +
maxDemandS` (an epoch-milliseconds value would otherwise hold demand for decades).

## 4. HIGH -- unchecked buffer reads produce NaN that poisons state (L64-72)

`charCodeAt(i+1)` / `charCodeAt(i+2)` are unbounded. A truncated advert yields
NaN. The guard at L127 is `if (d.h === null) return;` which is FALSE for NaN, so
`lastHum = NaN` is stored, `baseline` becomes NaN via the EMA and stays NaN
forever. All subsequent comparisons evaluate false: the fan silently never
triggers again until a script restart, and a running fan only exits via maxRunS.

Fix applied: `need(n)` bounds check before every multi-byte read, and the null
guard replaced with `isNum()` (`v === v` catches NaN).

---

## Notes for eval use

- All four are "correct-looking code, wrong over time or under failure" -- none
  are syntactic, and none reproduce on a single-pass read of one function.
- #1 needs a cross-function join: producer in a BLE event callback, consumer in
  a `Timer.set`. Probably out of reach for a single AST pattern.
- #2 is a guard-clause-placement shape; likely expressible but needs a notion of
  what counts as a state transition (`Switch.Set` here).
- #3 and #4 look like cheap local rules.
- Runtime semantics are what make #1 and #3 fatal rather than untidy: bounded
  heap, and exceptions in async callbacks killing the process. A generic JS
  reviewer has no reason to treat either as critical.

## Caveat on the complexity signal

I originally observed that quorum flagged cyclomatic complexity 16-17 on exactly
the two functions containing bugs 2 and 4, and read that as the signal pointing
at the right place. That reading is too generous: two hits across the only two
substantial functions in a 187-line file is close to chance. Worth testing
properly against this fixture rather than banking on it.
