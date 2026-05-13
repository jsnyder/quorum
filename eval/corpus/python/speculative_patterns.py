import logging
import json
import aiohttp


# --- TRUE POSITIVE: missing-await ---
# Unawaited coroutine from aiohttp session.get.
async def fetch_status(session: aiohttp.ClientSession, url: str) -> dict:
    response = {"status": "pending"}
    # Bug: session.get() returns a coroutine, not the response
    session.get(url)
    return response


# --- TRUE POSITIVE: missing-await ---
# Async validate_item called without await in loop.
async def validate_item(item: dict) -> bool:
    return "id" in item and "name" in item

async def process_batch(items: list[dict]) -> list[dict]:
    for item in items:
        validate_item(item)
    return [item for item in items if "id" in item]


# --- FALSE POSITIVE: missing-await ---
# Sync json.dumps call inside async function (false positive).
async def validate_json(data: dict) -> bool:
    try:
        json.dumps(data)
        return True
    except (TypeError, ValueError):
        return False


# --- FALSE POSITIVE: missing-await ---
# Sync list.append inside async function (false positive).
async def collect_names(items: list[dict]) -> list[str]:
    results = []
    for item in items:
        results.append(item["name"])
    return results


# --- TRUE POSITIVE: logging-debug-leak ---
# Password logged at debug level.
def authenticate(username: str, password: str) -> bool:
    logging.debug(f"Authenticating {username} with password={password}")
    return username == "admin"


# --- TRUE POSITIVE: logging-debug-leak ---
# API token logged at debug level.
def connect_api(endpoint: str, token: str) -> None:
    logging.debug(f"Connecting to {endpoint} with token={token}")
    pass


# --- FALSE POSITIVE: logging-debug-leak ---
# Non-sensitive request metadata in debug log.
def log_request(request_id: str, method: str) -> None:
    logging.debug(f"Processing request {request_id}: {method}")
    pass


# --- FALSE POSITIVE: logging-debug-leak ---
# Timing metrics in debug log (not sensitive).
def log_timing(operation: str, elapsed_ms: float) -> None:
    logging.debug(f"Operation {operation} took {elapsed_ms}ms")
    pass


# --- Non-speculative code for context ---
class DataProcessor:
    def __init__(self, batch_size: int = 100):
        self.batch_size = batch_size
        self.processed = 0

    def process(self, data: list) -> list:
        results = []
        for i in range(0, len(data), self.batch_size):
            batch = data[i : i + self.batch_size]
            results.extend(batch)
            self.processed += len(batch)
        return results
