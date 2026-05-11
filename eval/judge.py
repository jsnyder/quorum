import os
import re
import sys
from pathlib import Path

import httpx

from schema import CanonicalFinding, GroundTruthEntry, Verdict, load_ground_truth

LINE_TOLERANCE = 5
TITLE_KEYWORD_THRESHOLD = 0.4


def _tokenize(title: str) -> set[str]:
    return set(re.findall(r"[a-zA-Z_][a-zA-Z0-9_]*", title.lower()))


def _title_similarity(a: str, b: str) -> float:
    ta, tb = _tokenize(a), _tokenize(b)
    if not ta or not tb:
        return 0.0
    overlap = len(ta & tb)
    return overlap / min(len(ta), len(tb))


def _lines_compatible(finding: CanonicalFinding, gt: GroundTruthEntry) -> bool:
    if finding.line_start == 0:
        return True
    return (
        abs(finding.line_start - gt.line_start) <= LINE_TOLERANCE
        or (gt.line_start <= finding.line_start <= gt.line_end)
        or (finding.line_start <= gt.line_start <= finding.line_end)
    )


def match_ground_truth(
    finding: CanonicalFinding,
    ground_truth: list[GroundTruthEntry],
) -> GroundTruthEntry | None:
    best_match: GroundTruthEntry | None = None
    best_score = 0.0
    for gt in ground_truth:
        score = _title_similarity(finding.title, gt.title)
        if score >= TITLE_KEYWORD_THRESHOLD and _lines_compatible(finding, gt):
            if score > best_score:
                best_score = score
                best_match = gt
    return best_match


def _load_all_ground_truth(corpus_dir: Path) -> dict[str, list[GroundTruthEntry]]:
    gt_map: dict[str, list[GroundTruthEntry]] = {}
    for lang_dir in corpus_dir.iterdir():
        if not lang_dir.is_dir():
            continue
        for gt_file in lang_dir.glob("*.ground_truth.json"):
            stem = gt_file.name.replace(".ground_truth.json", "")
            rel_key = f"{lang_dir.name}/{stem}"
            for ext in (".rs", ".py", ".ts", ".tsx", ".yaml", ".yml", ".sh", ".bash"):
                full_key = f"{lang_dir.name}/{stem}{ext}"
                gt_map[full_key] = load_ground_truth(gt_file)
            gt_map[rel_key] = load_ground_truth(gt_file)
    return gt_map


def judge_auto(
    findings: list[CanonicalFinding],
    ground_truth: list[GroundTruthEntry],
) -> tuple[list[Verdict], list[CanonicalFinding]]:
    verdicts = []
    unmatched = []
    matched_gt_ids: set[str] = set()

    for f in findings:
        gt = match_ground_truth(f, ground_truth)
        if gt and gt.id not in matched_gt_ids:
            matched_gt_ids.add(gt.id)
            verdicts.append(Verdict(
                file=f.file,
                tool=f.tool,
                finding_title=f.title,
                verdict="tp",
                judge="auto",
                reason=f"Matched ground truth: {gt.title}",
                matched_ground_truth_id=gt.id,
            ))
        elif gt:
            verdicts.append(Verdict(
                file=f.file,
                tool=f.tool,
                finding_title=f.title,
                verdict="tp",
                judge="auto",
                reason=f"Duplicate match for: {gt.title}",
                matched_ground_truth_id=gt.id,
            ))
        else:
            unmatched.append(f)

    return verdicts, unmatched


def judge_panel(
    findings: list[CanonicalFinding],
    source_file: Path,
    ground_truth: list[GroundTruthEntry],
) -> list[Verdict]:
    base_url = os.environ.get("QUORUM_BASE_URL", "https://litellm.5745.house")
    api_key = os.environ.get("QUORUM_API_KEY", "")
    if not api_key:
        return [
            Verdict(
                file=f.file, tool=f.tool, finding_title=f.title,
                verdict="tp", judge="panel-skipped",
                reason="No QUORUM_API_KEY set, defaulting to TP",
            )
            for f in findings
        ]

    source_lines = source_file.read_text().splitlines() if source_file.exists() else []
    gt_summary = "\n".join(f"- [{g.id}] {g.title} ({g.severity})" for g in ground_truth)

    models = ["claude-sonnet-4", "gemini-2.5-pro"]
    verdicts = []

    for f in findings:
        start = max(0, f.line_start - 25)
        end = min(len(source_lines), f.line_end + 25) if f.line_end > 0 else min(len(source_lines), 50)
        excerpt = "\n".join(f"{i+start+1}: {l}" for i, l in enumerate(source_lines[start:end]))

        prompt = (
            f"You are judging a code review finding.\n\n"
            f"## Source excerpt\n```\n{excerpt}\n```\n\n"
            f"## Finding\n"
            f"- Title: {f.title}\n- Severity: {f.severity}\n- Category: {f.category}\n"
            f"- Lines: {f.line_start}-{f.line_end}\n- Description: {f.description}\n\n"
            f"## Known bugs in this file\n{gt_summary}\n\n"
            f"Is this finding a genuine bug, vulnerability, or quality issue? "
            f"Answer with exactly one of: tp, fp, partial. Then one sentence reason.\n"
            f"Format: VERDICT: <tp|fp|partial> REASON: <reason>"
        )

        votes: list[tuple[str, str]] = []
        errors = 0
        for model in models:
            try:
                resp = httpx.post(
                    f"{base_url}/v1/chat/completions",
                    headers={"Authorization": f"Bearer {api_key}"},
                    json={
                        "model": model,
                        "messages": [{"role": "user", "content": prompt}],
                        "temperature": 0,
                        "max_tokens": 200,
                    },
                    timeout=60,
                )
                body = resp.json()
                if "choices" not in body:
                    err_msg = body.get("error", {}).get("message", str(body)[:100])
                    errors += 1
                    print(f"      Judge {model}: API error: {err_msg}", file=sys.stderr)
                    continue
                text = body["choices"][0]["message"]["content"] or ""
                verdict, reason = _parse_verdict(text)
                votes.append((verdict, reason))
            except Exception as e:
                errors += 1
                print(f"      Judge {model}: exception: {e}", file=sys.stderr)

        if not votes:
            verdicts.append(Verdict(
                file=f.file, tool=f.tool, finding_title=f.title,
                verdict="tp", judge="panel-error",
                reason=f"All {errors} judges errored, defaulting to TP",
            ))
            continue

        verdict_counts: dict[str, int] = {}
        for v, _ in votes:
            verdict_counts[v] = verdict_counts.get(v, 0) + 1
        final_verdict = max(verdict_counts, key=lambda k: verdict_counts[k])
        reasons = [r for _, r in votes]

        judge_type = "panel" if len(set(v for v, _ in votes)) == 1 else "panel-disputed"
        if errors > 0:
            judge_type += f"-degraded({len(votes)}/{len(models)})"

        verdicts.append(Verdict(
            file=f.file,
            tool=f.tool,
            finding_title=f.title,
            verdict=final_verdict,
            judge=judge_type,
            reason=f"Votes: {', '.join(v for v, _ in votes)}. {reasons[0]}",
        ))

    return verdicts


def _parse_verdict(text: str) -> tuple[str, str]:
    text = text.strip()
    for prefix in ("VERDICT:", "verdict:"):
        if prefix in text:
            after = text.split(prefix, 1)[1].strip()
            parts = after.split("REASON:", 1) if "REASON:" in after else after.split("reason:", 1) if "reason:" in after else [after, ""]
            verdict = parts[0].strip().lower()
            reason = parts[1].strip() if len(parts) > 1 else ""
            if verdict in ("tp", "fp", "partial"):
                return verdict, reason
    first_word = text.split()[0].lower().rstrip(".:,") if text else ""
    if first_word in ("tp", "fp", "partial"):
        return first_word, text
    return "tp", f"Could not parse verdict, defaulting to TP. Raw: {text[:100]}"


def judge_findings(
    all_findings: dict[str, list[CanonicalFinding]],
    corpus_dir: Path,
) -> list[Verdict]:
    gt_map = _load_all_ground_truth(corpus_dir)
    all_verdicts: list[Verdict] = []

    for _tool, findings in all_findings.items():
        by_file: dict[str, list[CanonicalFinding]] = {}
        for f in findings:
            by_file.setdefault(f.file, []).append(f)

        for file_rel, file_findings in by_file.items():
            gt = gt_map.get(file_rel, [])
            auto_verdicts, unmatched = judge_auto(file_findings, gt)
            all_verdicts.extend(auto_verdicts)

            if unmatched:
                source_path = corpus_dir / file_rel
                if not source_path.exists():
                    for ext in (".rs", ".py", ".ts"):
                        candidate = corpus_dir / f"{file_rel}{ext}"
                        if candidate.exists():
                            source_path = candidate
                            break

                panel_verdicts = judge_panel(unmatched, source_path, gt)
                all_verdicts.extend(panel_verdicts)

    return all_verdicts
