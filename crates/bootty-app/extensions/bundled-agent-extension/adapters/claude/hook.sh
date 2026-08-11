#!/bin/sh
set -eu

# Claude supplies one authoritative JSON object on stdin. Keep stdin attached
# to the ingest command; never inspect transcripts, terminal output, or
# process metadata.
if [ "$#" -ne 2 ] || [ "$1" != "--event" ] || [ -z "$2" ]; then
    printf '%s\n' "usage: claude-hook.sh --event EVENT" >&2
    exit 2
fi

exec bootty command agents.ingest --adapter claude --event-name "$2" --stdin-json
