#!/bin/sh
set -eu

payload=$(cat)
pane=${TMUX_PANE:-${BOOTTY_PANE:-}}
bootty --json command agents.claude.ingest "$payload" "$pane" >/dev/null 2>&1 || :
printf '{}\n'
