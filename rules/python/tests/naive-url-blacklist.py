BLACKLISTED = ["127.0.0.1", "localhost", "0.0.0.0"]

def is_safe_url(url):
    # TP: should match - naive string-based URL blacklist check
    if any(b in url for b in BLACKLISTED):  # ruleid: naive-url-blacklist
        return False
    return True

def check_host(hostname, blocked):
    # TP: should match - same pattern with different var names
    return any(entry in hostname for entry in blocked)  # ruleid: naive-url-blacklist

# FP: should NOT match - any() with other conditions
result = any(x > 0 for x in values)  # ok: naive-url-blacklist

# FP: should NOT match - all() instead of any()
safe = all(b not in url for b in BLACKLISTED)  # ok: naive-url-blacklist
