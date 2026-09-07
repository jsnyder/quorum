#!/usr/bin/env python3
"""Score a run_eval.py results file: per-rule confusion matrix and the
survivor precision that actually decides whether a rule earns its tokens.

`judge: required` keeps a finding on 'approved' OR 'uncertain' and drops it
only on 'rejected' (src/judge.rs::enforce_judge_required). So the number that
matters is not the judge's accuracy but the precision of what SURVIVES it.
"""
import collections
import json
import sys


def load(path):
    rows, totals = [], None
    for line in open(path):
        d = json.loads(line)
        if "_totals" in d:
            totals = d
        else:
            rows.append(d)
    return rows, totals


def score(path, title):
    rows, totals = load(path)
    print(f"\n{'=' * 78}\n{title}   ({totals['_variant']}, {totals['_model']})\n{'=' * 78}")
    t = totals["_totals"]
    print(f"cost: {t['calls']} calls, {t['tin']:,} in + {t['tout']:,} out tokens, "
          f"{t['ms'] / 1000:.1f}s wall\n")

    by = collections.defaultdict(list)
    for r in rows:
        by[(r["rule"], r["label_source"])].append(r)

    print(f"{'rule':<30}{'src':<20}{'n':>4}{'lbl':>5}"
          f"{' appr/unc/rej/none':>22}{'  survivors':>11}{'  surv.prec':>11}")
    print("-" * 105)
    agg = collections.defaultdict(lambda: {"tp_surv": 0, "fp_surv": 0, "tp": 0, "fp": 0,
                                           "tp_rej": 0, "fp_rej": 0})
    for (rule, src), rs in sorted(by.items()):
        lab = [r for r in rs if r["label"] in ("tp", "fp")]
        a = sum(r["judge"] == "approved" for r in rs)
        u = sum(r["judge"] == "uncertain" for r in rs)
        j = sum(r["judge"] == "rejected" for r in rs)
        n = sum(r["judge"] == "unjudged" for r in rs)
        # judge:required keeps approved + uncertain; rejected and unjudged both drop.
        surv = [r for r in lab if r["judge"] in ("approved", "uncertain")]
        stp = sum(r["label"] == "tp" for r in surv)
        sfp = sum(r["label"] == "fp" for r in surv)
        prec = f"{stp / (stp + sfp):.0%}" if surv else "  n/a"
        print(f"{rule:<30}{src:<20}{len(rs):>4}{len(lab):>5}"
              f"{a:>8}/{u}/{j}/{n}{len(surv):>11}{prec:>11}")
        g = agg[rule]
        g["tp_surv"] += stp
        g["fp_surv"] += sfp
        g["tp"] += sum(r["label"] == "tp" for r in lab)
        g["fp"] += sum(r["label"] == "fp" for r in lab)
        drop = ("rejected", "unjudged")
        g["tp_rej"] += sum(r["label"] == "tp" and r["judge"] in drop for r in lab)
        g["fp_rej"] += sum(r["label"] == "fp" and r["judge"] in drop for r in lab)

    print(f"\n{'rule':<30}{'labelled tp/fp':>16}{'  tp kept':>18}"
          f"{'  fp dropped':>20}{'  survivor prec':>16}")
    print("-" * 105)
    for rule, g in sorted(agg.items()):
        rec = f"{(g['tp'] - g['tp_rej']) / g['tp']:.0%}" if g["tp"] else "n/a"
        cut = f"{g['fp_rej'] / g['fp']:.0%}" if g["fp"] else "n/a"
        sv = g["tp_surv"] + g["fp_surv"]
        prec = f"{g['tp_surv'] / sv:.0%}" if sv else "n/a"
        print(f"{rule:<30}{str(g['tp']) + '/' + str(g['fp']):>16}{rec:>18}{cut:>20}{prec:>16}")

    # unjudged: response truncation or a missing index leaves a finding with no
    # verdict, which `judge: required` then drops.
    miss = [r for r in rows if r["judge"] == "unjudged" or r["confidence"] is None]
    if miss:
        c = collections.Counter((r["file"], r["rule"]) for r in miss)
        print(f"\nfindings with NO verdict returned (dropped by judge:required): {len(miss)}")
        for (f, rule), n in c.most_common(8):
            print(f"  {n:>3}  {rule:<30} {f}")
    return rows


if __name__ == "__main__":
    for p in sys.argv[1:]:
        score(p, p)
