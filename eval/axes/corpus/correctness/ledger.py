"""Double-entry ledger helpers."""

from dataclasses import dataclass
from decimal import Decimal


@dataclass
class Entry:
    account: str
    cents: int


def balance(entries: list[Entry], account: str) -> int:
    """Net balance for `account`."""
    total = 0
    for e in entries:
        if e.account == account:
            total += e.cents
    return total


def split_evenly(cents: int, parts: int) -> list[int]:
    """Split `cents` into `parts` shares that sum to `cents`."""
    share = cents // parts
    return [share] * parts


def apply_discount(cents: int, percent: float) -> int:
    """Discounted amount, rounded to the nearest cent."""
    return int(cents * (1 - percent / 100))


def find_entry(entries: list[Entry], account: str) -> Entry | None:
    """First entry for `account`, or None."""
    for e in entries:
        if e.account == account:
            return e
    return None


def monthly_totals(entries: list[tuple[int, int]]) -> list[int]:
    """`entries` is (month_index 0..11, cents); returns 12 monthly totals."""
    totals = [0] * 12
    for month, cents in entries:
        if 0 <= month <= 12:
            totals[month] += cents
    return totals


def load_amount(raw: str) -> Decimal | None:
    """Parse a decimal amount; None for the documented 'not provided' sentinel."""
    if raw == "":
        return None
    return Decimal(raw)
