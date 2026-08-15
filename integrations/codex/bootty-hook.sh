#!/bin/sh
set -eu

payload=$(cat)
bootty --json command agents.codex.ingest "$payload" >/dev/null 2>&1 || :
printf '{}\n'
