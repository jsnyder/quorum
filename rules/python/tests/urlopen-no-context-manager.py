from urllib.request import urlopen

# TP: should match - urlopen without context manager
response = urlopen("https://example.com")  # ruleid: urlopen-no-context-manager
data = response.read()

# FP: should NOT match - used as context manager
with urlopen("https://example.com") as resp:  # ok: urlopen-no-context-manager
    data = resp.read()
