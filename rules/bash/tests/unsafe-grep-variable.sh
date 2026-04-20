#!/usr/bin/env bash
# Fixture: unsafe-grep-variable

PATTERN="$1"

# match: variable-as-regex
grep "$PATTERN" /var/log/messages

# match: unquoted variable
grep $PATTERN /var/log/syslog

# no-match: -F forces fixed-string
grep -F "$PATTERN" /var/log/messages

# no-match: literal pattern
grep "ERROR" /var/log/messages
