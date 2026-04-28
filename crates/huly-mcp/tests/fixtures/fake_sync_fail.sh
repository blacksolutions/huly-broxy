#!/usr/bin/env bash
# Stub that fails: writes to stderr, exits non-zero. Records args in CWD.
printf '%s\n' "$@" > ./args.txt
echo "boom: connection refused" >&2
echo "partial output on stdout"
exit 7
