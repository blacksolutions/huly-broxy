#!/usr/bin/env bash
# Stub for status invocation.
# Records args (one per line) into ./args.txt in the CWD set by the parent.
printf '%s\n' "$@" > ./args.txt
cat <<'JSON'
{
  "summary": "2 changes detected.",
  "lastSync": "2026-04-22T00:00:00Z",
  "new": ["docs/new.md"],
  "modified": ["docs/changed.md"],
  "deleted": [],
  "totalTracked": 5,
  "totalOnDisk": 6
}
JSON
exit 0
