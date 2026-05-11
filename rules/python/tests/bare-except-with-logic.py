# TP: should match
try:
    do_something()
except:  # ruleid: bare-except-with-logic
    print("Error")

try:
    do_something()
except:  # ruleid: bare-except-with-logic
    logger.error("Failed")
    return None

# FP: should NOT match
try:
    do_something()
except:  # ok: bare-except-with-logic
    pass

try:
    do_something()
except Exception:  # ok: bare-except-with-logic
    print("Error")

try:
    do_something()
except ValueError:  # ok: bare-except-with-logic
    print("Error")
