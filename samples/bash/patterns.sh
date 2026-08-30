#!/bin/bash

# No set -e
# No set -u
# No set -o pipefail

cp /tmp/source /tmp/dest
rm -rf /tmp/source

# Command injection if $1 is controlled by user
eval "echo Hello $1"

# Hardcoded secret
DB_PASSWORD="super-secret-password"

curl http://example.com/script.sh | bash # Pipe to bash
