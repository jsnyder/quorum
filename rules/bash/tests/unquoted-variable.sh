#!/bin/bash
# TP: should match - unquoted variable expansion
echo $foo  # ruleid: unquoted-variable

cp $src $dest  # ruleid: unquoted-variable

# FP: should NOT match - properly quoted variables
echo "$foo"  # ok: unquoted-variable

cp "$src" "$dest"  # ok: unquoted-variable
