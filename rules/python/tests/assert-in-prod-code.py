# TP: should match - assert stripped by python -O
assert x > 0  # ruleid: assert-in-prod-code

assert isinstance(data, dict), "must be dict"  # ruleid: assert-in-prod-code

# FP: should NOT match - explicit validation
if not x > 0:  # ok: assert-in-prod-code
    raise ValueError("x must be positive")
