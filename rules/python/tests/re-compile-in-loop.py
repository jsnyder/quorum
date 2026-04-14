import re

# TP: should match - regex call inside loop
for line in lines:
    match = re.search(r"\d+", line)  # ruleid: re-compile-in-loop
    result = re.sub(r"\s+", " ", line)  # ruleid: re-compile-in-loop
    found = re.findall(r"[a-z]+", line)  # ruleid: re-compile-in-loop

while True:
    m = re.match(r"^ok", text)  # ruleid: re-compile-in-loop

# FP: should NOT match - outside loop
match = re.search(r"\d+", text)  # ok: re-compile-in-loop

# FP: should NOT match - pre-compiled
pattern = re.compile(r"\d+")
for line in lines:
    match = pattern.search(line)  # ok: re-compile-in-loop
