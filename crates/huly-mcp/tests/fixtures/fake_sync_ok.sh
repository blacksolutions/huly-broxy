#!/usr/bin/env bash
# Stub for `node {script} [--dry-run]`. Records args to ./args.txt in CWD,
# prints success.
printf '%s\n' "$@" > ./args.txt
echo "=== Huly Sync Tool ==="
for arg in "$@"; do
  if [ "$arg" = "--dry-run" ]; then
    echo "Mode: DRY RUN"
  fi
done
echo "[Scan] no document found for foo"
echo "Sync OK"
exit 0
