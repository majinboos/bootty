#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/record-replay-fixture.sh <fixture-name> <output-dir> -- <command> [args...]

Records a real application fixture bundle for Bootty replay benchmarks.
The bundle contains:
  stream.pty      raw terminal byte stream captured through script(1)
  timing.tsv      coarse command wall-clock timing metadata
  metadata.env    terminal size, TERM, command, platform, app versions
  SHA256SUMS      checksums for reproducibility

This recorder intentionally stays outside routine validation. It depends on a
host script(1) implementation and records whatever terminal behavior the host
command emits.
USAGE
}

if [[ $# -lt 4 || "$3" != "--" ]]; then
  usage >&2
  exit 2
fi

fixture_name=$1
output_root=$2
shift 3
cmd=("$@")
command_string=$(printf '%q ' "${cmd[@]}")
command_string=${command_string% }


if ! command -v script >/dev/null 2>&1; then
  echo "record-replay-fixture: script(1) is required" >&2
  exit 1
fi

output_dir=${output_root%/}/${fixture_name}
mkdir -p "$output_dir"
stream_file=$output_dir/stream.pty
timing_file=$output_dir/timing.tsv
metadata_file=$output_dir/metadata.env
checksum_file=$output_dir/SHA256SUMS

cols=${COLUMNS:-$(tput cols 2>/dev/null || printf '80')}
rows=${LINES:-$(tput lines 2>/dev/null || printf '24')}
term_value=${TERM:-unknown}
start_epoch=$(date -u +%Y-%m-%dT%H:%M:%SZ)
start_ns=$(date +%s%N 2>/dev/null || date +%s000000000)

{
  printf 'fixture=%q\n' "$fixture_name"
  printf 'recorded_at_utc=%q\n' "$start_epoch"
  printf 'cols=%q\n' "$cols"
  printf 'rows=%q\n' "$rows"
  printf 'term=%q\n' "$term_value"
  printf 'command=%q\n' "$command_string"
  printf '\n'
  printf 'uname=%q\n' "$(uname -a)"
  printf 'shell=%q\n' "${SHELL:-unknown}"
  for app in nvim vim helix hx emacs less fzf git tmux btop htop kubectl docker podman cargo npm pytest go; do
    if command -v "$app" >/dev/null 2>&1; then
      version=$($app --version 2>&1 | head -n 1 | tr -d '\r') || version=unknown
      printf 'version_%s=%q\n' "$app" "$version"
    fi
  done
} >"$metadata_file"

# Prefer util-linux script timing support when present; macOS script lacks -T.
if script --help 2>&1 | grep -q -- '-T'; then
  script -q -e -T "$timing_file" -c "$command_string" "$stream_file"
else
  printf 'start_ns\t%s\n' "$start_ns" >"$timing_file"
  script -q "$stream_file" "${cmd[@]}"
fi

end_ns=$(date +%s%N 2>/dev/null || date +%s000000000)
printf 'end_ns\t%s\n' "$end_ns" >>"$timing_file"
printf 'duration_ns\t%s\n' "$((end_ns - start_ns))" >>"$timing_file"

(
  cd "$output_dir"
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 stream.pty timing.tsv metadata.env >"$(basename "$checksum_file")"
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum stream.pty timing.tsv metadata.env >"$(basename "$checksum_file")"
  else
    echo "record-replay-fixture: no SHA-256 command found" >&2
    exit 1
  fi
)

printf 'Recorded fixture bundle: %s\n' "$output_dir"
