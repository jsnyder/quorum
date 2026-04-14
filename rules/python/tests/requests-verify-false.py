import requests

# TP: should match - verify=False disables TLS
requests.get("https://example.com", verify=False)  # ruleid: requests-verify-false
requests.post("https://example.com/api", data=payload, verify=False)  # ruleid: requests-verify-false
requests.put("https://example.com/api", json=data, verify=False)  # ruleid: requests-verify-false
requests.delete("https://example.com/api/1", verify=False)  # ruleid: requests-verify-false
requests.patch("https://example.com/api/1", json=patch, verify=False)  # ruleid: requests-verify-false
requests.head("https://example.com", verify=False)  # ruleid: requests-verify-false
requests.options("https://example.com", verify=False)  # ruleid: requests-verify-false
requests.request("GET", "https://example.com", verify=False)  # ruleid: requests-verify-false

# FP: should NOT match - verify not disabled
requests.get("https://example.com")  # ok: requests-verify-false
requests.get("https://example.com", verify=True)  # ok: requests-verify-false
requests.get("https://example.com", verify="/path/to/cert")  # ok: requests-verify-false
requests.post("https://example.com/api", data=payload)  # ok: requests-verify-false
