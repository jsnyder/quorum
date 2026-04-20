# Fixture: blocking-call-in-async
import asyncio, time, requests, subprocess

# match: time.sleep inside async
async def wait_a_bit():
    time.sleep(1)

# match: requests.get inside async
async def fetch(url):
    return requests.get(url).text

# match: subprocess.run inside async
async def run_cmd():
    subprocess.run(["ls"], check=True)

# no-match: async-native calls
async def proper_wait():
    await asyncio.sleep(1)

# no-match: blocking call in SYNC function is fine
def sync_fetch(url):
    return requests.get(url).text
