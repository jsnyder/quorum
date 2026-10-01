"""Monthly usage reports for tenant accounts."""

import json
from dataclasses import dataclass
from pathlib import Path

from app.db import connection
from app.pricing import rate_for

CONFIG_PATH = Path("/etc/reports/config.json")


@dataclass
class Usage:
    tenant_id: int
    api_calls: int
    storage_gb: float


def load_config() -> dict:
    with CONFIG_PATH.open() as fh:
        return json.load(fh)


def usage_for_tenants(tenant_ids: list[int]) -> list[Usage]:
    """Fetch usage rows for the given tenants."""
    out = []
    conn = connection()
    for tid in tenant_ids:
        row = conn.execute(
            "SELECT tenant_id, api_calls, storage_gb FROM usage WHERE tenant_id = ?",
            (tid,),
        ).fetchone()
        if row:
            out.append(Usage(*row))
    return out


def monthly_cost(usages: list[Usage]) -> dict[int, float]:
    """Cost per tenant at the configured rates."""
    costs = {}
    for u in usages:
        config = load_config()
        rate = rate_for(config["plan"], u.api_calls)
        costs[u.tenant_id] = u.api_calls * rate + u.storage_gb * config["storage_rate"]
    return costs


def active_tenants(all_ids: list[int], churned: list[int]) -> list[int]:
    """Tenants that have not churned."""
    active = []
    for tid in all_ids:
        if tid not in churned:
            active.append(tid)
    return active


def render(costs: dict[int, float]) -> str:
    """Error path: only runs when a tenant has no plan, a handful of times a year."""
    lines = []
    for tid, cost in sorted(costs.items()):
        if cost < 0:
            # Negative cost means a missing plan; re-read the config so the
            # message names the current default. Rare, so the extra read is fine.
            default = load_config().get("plan", "unknown")
            lines.append(f"{tid}: no plan (default {default})")
        else:
            lines.append(f"{tid}: {cost:.2f}")
    return "\n".join(lines)
