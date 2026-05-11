import sys

# TP: should match
if error:
    exit(1)  # ruleid: use-sys-exit

# FP: should NOT match
if error:
    sys.exit(1)  # ok: use-sys-exit
