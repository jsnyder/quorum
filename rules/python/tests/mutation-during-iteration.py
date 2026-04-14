items = [1, 2, 3, 4, 5]
data = {"a", "b", "c"}

# TP: should match - appending to list while iterating
for item in items:
    if item > 3:
        items.append(item * 2)  # ruleid: mutation-during-iteration

# TP: should match - removing from collection while iterating
for x in data:
    data.remove(x)  # ruleid: mutation-during-iteration

# FP: should NOT match - iterating a copy
for item in list(items):  # ok: mutation-during-iteration
    items.append(item * 2)

# FP: should NOT match - mutating a different collection
other = []
for item in items:  # ok: mutation-during-iteration
    other.append(item)
