from schema import CanonicalFinding

SEVERITY_NORMALIZE = {
    "critical": "critical",
    "high": "high",
    "medium": "medium",
    "low": "low",
    "info": "info",
    "warning": "medium",
}

def _norm_severity(s: str) -> str:
    return SEVERITY_NORMALIZE.get(s.lower(), "info")

def _norm_category(c) -> str:
    if isinstance(c, str):
        return c.lower()
    if isinstance(c, dict):
        for key in c:
            return key.lower()
    return "unknown"

def normalize_quorum(
    data: list | dict,
    tool_name: str,
    file_path: str | None = None,
) -> list[CanonicalFinding]:
    findings = []
    if isinstance(data, dict):
        for fp, file_findings in data.items():
            for f in file_findings:
                findings.append(_quorum_finding(f, tool_name, fp))
    elif isinstance(data, list):
        for f in data:
            findings.append(_quorum_finding(f, tool_name, file_path or "unknown"))
    return findings

def _quorum_finding(f: dict, tool: str, file_path: str) -> CanonicalFinding:
    return CanonicalFinding(
        tool=tool,
        file=file_path,
        title=f.get("title", ""),
        category=_norm_category(f.get("category", "unknown")),
        severity=_norm_severity(f.get("severity", "info")),
        line_start=f.get("line_start", 0),
        line_end=f.get("line_end", 0),
        description=f.get("description", ""),
    )

def normalize_pal(
    findings_list: list[dict],
    file_path: str,
) -> list[CanonicalFinding]:
    return [
        CanonicalFinding(
            tool="pal",
            file=file_path,
            title=f.get("title", ""),
            category="unknown",
            severity=_norm_severity(f.get("severity", "info")),
            line_start=0,
            line_end=0,
            description=f.get("title", ""),
        )
        for f in findings_list
    ]

def normalize_third_opinion(
    data: dict,
    file_path: str,
) -> list[CanonicalFinding]:
    return [
        CanonicalFinding(
            tool="third-opinion",
            file=file_path,
            title=f.get("title", ""),
            category="unknown",
            severity=_norm_severity(f.get("severity", "info")),
            line_start=0,
            line_end=0,
            description=f.get("title", ""),
        )
        for f in data.get("findings", [])
    ]
