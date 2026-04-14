import subprocess

# TP: should match - shell=True with variable command
cmd = f"ls {user_input}"
subprocess.run(cmd, shell=True)  # ruleid: subprocess-shell-true
subprocess.call(cmd, shell=True)  # ruleid: subprocess-shell-true
subprocess.check_output(cmd, shell=True)  # ruleid: subprocess-shell-true
subprocess.Popen(cmd, shell=True)  # ruleid: subprocess-shell-true

# FP: should NOT match - shell=False or no shell
subprocess.run(["ls", "-la"])  # ok: subprocess-shell-true
subprocess.run(cmd, shell=False)  # ok: subprocess-shell-true
subprocess.run(cmd, check=True)  # ok: subprocess-shell-true
subprocess.call(["echo", "hello"])  # ok: subprocess-shell-true
