import subprocess

# TP: should match - subprocess.run without check
subprocess.run(["ls", "-la"])  # ruleid: subprocess-no-check
subprocess.run(cmd, shell=True)  # ruleid: subprocess-no-check

# FP: should NOT match - has check=True
subprocess.run(["ls"], check=True)  # ok: subprocess-no-check
subprocess.run(cmd, check=True, capture_output=True)  # ok: subprocess-no-check

# FP: should NOT match - not subprocess.run
subprocess.call(["ls"])  # ok: subprocess-no-check
subprocess.Popen(["ls"])  # ok: subprocess-no-check
