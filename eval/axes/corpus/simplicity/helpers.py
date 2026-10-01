"""Small helpers for the ingest pipeline."""

import copy
import os

# Feature flag for the rewritten path. Nothing sets it: grep the repo and
# the deploy manifests and it is never exported.
ENABLE_V2_PATH = os.environ.get("INGEST_ENABLE_V2_PATH") == "1"


def largest(values: list[int]) -> int:
    """Largest value in a non-empty list."""
    best = values[0]
    for v in values[1:]:
        if v > best:
            best = v
    return best


def clone_record(record: dict) -> dict:
    """Deep copy of a nested record."""
    out = {}
    for k, v in record.items():
        if isinstance(v, dict):
            out[k] = clone_record(v)
        elif isinstance(v, list):
            out[k] = [clone_record(x) if isinstance(x, dict) else x for x in v]
        else:
            out[k] = v
    return out


def ingest(records: list[dict]) -> list[dict]:
    out = []
    for r in records:
        if ENABLE_V2_PATH:
            out.append(_ingest_v2(r))
        else:
            out.append(_ingest_v1(r))
    return out


def _ingest_v1(r: dict) -> dict:
    r = copy.deepcopy(r)
    r["ingested"] = True
    return r


def _ingest_v2(r: dict) -> dict:
    r = copy.deepcopy(r)
    r["ingested"] = True
    r["schema"] = 2
    return r


def parse_port(raw: str) -> int:
    """Parse a port from config. Errors are explicit on purpose: a bad port
    at startup should fail loudly with the value that was given."""
    try:
        port = int(raw)
    except ValueError as e:
        raise ValueError(f"port must be an integer, got {raw!r}") from e
    if not 1 <= port <= 65535:
        raise ValueError(f"port out of range: {port}")
    return port
