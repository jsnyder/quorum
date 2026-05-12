# Fixture: missing-await
import asyncio

async def fetch_data():
    pass

async def save_data():
    pass

# match: async function called without await inside async context
async def process():
    fetch_data()  # ruleid: missing-await

# match: another unawaited call
async def run():
    save_data()  # ruleid: missing-await

# no-match: properly awaited
async def proper_process():
    await fetch_data()  # ok: missing-await

# no-match: sync function call inside async context is fine
def sync_helper():
    pass

async def sync_usage():
    sync_helper()  # ok: missing-await - not an async call pattern

# no-match: call in sync function
def sync_caller():
    fetch_data()  # ok: missing-await - not inside async function
