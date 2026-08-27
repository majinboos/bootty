#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/run-live-remote-bench.sh [output-jsonl]

Runs optional live remote-service probes and writes JSONL results. Every probe is
skipped unless its dependency and target are available. This harness is outside
routine validation and is intended for host-specific evidence only.

Optional targets:
  BOOTTY_LIVE_SSH_TARGET=user@host        LAN/WAN SSH target
  BOOTTY_LIVE_MOSH_TARGET=user@host       mosh target
  BOOTTY_LIVE_DOCKER_CONTAINER=name       existing Docker container for docker exec
  BOOTTY_LIVE_PODMAN_CONTAINER=name       existing Podman container for podman exec

Optional netem, disabled unless explicitly enabled:
  BOOTTY_LIVE_NETEM_APPLY=1
  BOOTTY_LIVE_NETEM_IFACE=<interface>

Output defaults to artifacts/live-remote/live-remote-<utc>.jsonl.
USAGE
}

if [[ ${1:-} == "-h" || ${1:-} == "--help" ]]; then
  usage
  exit 0
fi

output_file=${1:-}
if [[ -z "$output_file" ]]; then
  output_dir=artifacts/live-remote
  mkdir -p "$output_dir"
  stamp=$(date -u +%Y%m%dT%H%M%SZ)
  output_file=$output_dir/live-remote-$stamp.jsonl
else
  mkdir -p "$(dirname "$output_file")"
fi

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/bootty-live-remote.XXXXXX")
cleanup() {
  rm -rf "$work_dir"
}
trap cleanup EXIT

json_escape() {
  local value=${1-}
  value=${value//\\/\\\\}
  value=${value//"/\\"}
  value=${value//$'\n'/\\n}
  value=${value//$'\r'/\\r}
  printf '%s' "$value"
}

now_ns() {
  local value
  value=$(date +%s%N 2>/dev/null || true)
  if [[ $value =~ ^[0-9]+$ ]]; then
    printf '%s' "$value"
  else
    printf '%s000000000' "$(date +%s)"
  fi
}

emit_meta() {
  local uname_value shell_value
  uname_value=$(uname -a 2>/dev/null || printf 'unknown')
  shell_value=${SHELL:-unknown}
  printf '{"schema_version":1,"event":"metadata","recorded_at_utc":"%s","uname":"%s","shell":"%s"}\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    "$(json_escape "$uname_value")" \
    "$(json_escape "$shell_value")" >>"$output_file"
}

emit_result() {
  local name=$1 status=$2 detail=$3 duration_ns=$4 bytes=$5 exit_code=$6 profile=$7
  printf '{"schema_version":1,"event":"live_remote_probe","name":"%s","profile":"%s","status":"%s","detail":"%s","duration_ns":%s,"bytes":%s,"exit_code":%s}\n' \
    "$(json_escape "$name")" \
    "$(json_escape "$profile")" \
    "$(json_escape "$status")" \
    "$(json_escape "$detail")" \
    "$duration_ns" \
    "$bytes" \
    "$exit_code" >>"$output_file"
}

skip_probe() {
  emit_result "$1" skipped "$2" 0 0 0 "$3"
}

remote_probe=$(cat <<'REMOTE_PROBE'
printf "remote shell ready\r\n"; i=0; while [ "$i" -lt 128 ]; do printf "\033[32mkey-%03d\033[0m echo\r\n" "$i"; i=$((i + 1)); done; i=0; while [ "$i" -lt 512 ]; do printf "remote log line %05d cargo/test/kubectl stream payload payload payload\r\n" "$i"; i=$((i + 1)); done; printf "\033[8;40;120tremote resize ack 120x40\r\n"; printf "\033]8;id=remote;https://example.invalid/remote\033\\link\033]8;;\033\\\r\n"
REMOTE_PROBE
)

run_probe() {
  local name=$1 profile=$2
  shift 2
  local stdout_file=$work_dir/$name.stdout
  local stderr_file=$work_dir/$name.stderr
  local start end exit_code duration bytes detail
  start=$(now_ns)
  set +e
  if command -v timeout >/dev/null 2>&1; then
    timeout 20 "$@" >"$stdout_file" 2>"$stderr_file"
  else
    "$@" >"$stdout_file" 2>"$stderr_file"
  fi
  exit_code=$?
  set -e
  end=$(now_ns)
  duration=$((end - start))
  bytes=$(wc -c <"$stdout_file" | tr -d ' ')
  if [[ $exit_code -eq 0 ]]; then
    detail=ok
    emit_result "$name" pass "$detail" "$duration" "$bytes" "$exit_code" "$profile"
  else
    detail=$(head -n 1 "$stderr_file" 2>/dev/null || printf 'command failed')
    emit_result "$name" fail "$detail" "$duration" "$bytes" "$exit_code" "$profile"
  fi
}

with_netem() {
  local profile=$1 delay=$2 loss=$3
  shift 3
  if [[ ${BOOTTY_LIVE_NETEM_APPLY:-0} != 1 ]]; then
    skip_probe "$profile" "netem disabled; set BOOTTY_LIVE_NETEM_APPLY=1" "$profile"
    return 0
  fi
  if [[ -z ${BOOTTY_LIVE_NETEM_IFACE:-} ]]; then
    skip_probe "$profile" "BOOTTY_LIVE_NETEM_IFACE is not set" "$profile"
    return 0
  fi
  if ! command -v tc >/dev/null 2>&1; then
    skip_probe "$profile" "tc not found" "$profile"
    return 0
  fi

  sudo tc qdisc replace dev "$BOOTTY_LIVE_NETEM_IFACE" root netem delay "$delay" loss "$loss"
  trap 'sudo tc qdisc del dev "$BOOTTY_LIVE_NETEM_IFACE" root >/dev/null 2>&1 || true; cleanup' EXIT
  "$@"
  sudo tc qdisc del dev "$BOOTTY_LIVE_NETEM_IFACE" root >/dev/null 2>&1 || true
  trap cleanup EXIT
}

emit_meta

if command -v ssh >/dev/null 2>&1 && ssh -o BatchMode=yes -o ConnectTimeout=2 localhost true >/dev/null 2>&1; then
  run_probe localhost_ssh localhost ssh -o BatchMode=yes -o ConnectTimeout=2 localhost sh -lc "$remote_probe"
else
  skip_probe localhost_ssh "ssh localhost is unavailable" localhost
fi

if [[ -n ${BOOTTY_LIVE_SSH_TARGET:-} ]]; then
  if command -v ssh >/dev/null 2>&1; then
    run_probe lan_ssh lan ssh -o BatchMode=yes -o ConnectTimeout=5 "$BOOTTY_LIVE_SSH_TARGET" sh -lc "$remote_probe"
    with_netem wan_20ms_ssh 20ms 0% run_probe wan_20ms_ssh wan_20ms ssh -o BatchMode=yes -o ConnectTimeout=5 "$BOOTTY_LIVE_SSH_TARGET" sh -lc "$remote_probe"
    with_netem wan_100ms_ssh 100ms 0.1% run_probe wan_100ms_ssh wan_100ms ssh -o BatchMode=yes -o ConnectTimeout=5 "$BOOTTY_LIVE_SSH_TARGET" sh -lc "$remote_probe"
    with_netem wan_200ms_ssh 200ms 1% run_probe wan_200ms_ssh wan_200ms ssh -o BatchMode=yes -o ConnectTimeout=5 "$BOOTTY_LIVE_SSH_TARGET" sh -lc "$remote_probe"
  else
    skip_probe lan_ssh "ssh not found" lan
  fi
else
  skip_probe lan_ssh "BOOTTY_LIVE_SSH_TARGET is not set" lan
fi

if [[ -n ${BOOTTY_LIVE_MOSH_TARGET:-} ]]; then
  if command -v mosh >/dev/null 2>&1; then
    run_probe mosh mosh mosh "$BOOTTY_LIVE_MOSH_TARGET" -- sh -lc "$remote_probe"
  else
    skip_probe mosh "mosh not found" mosh
  fi
else
  skip_probe mosh "BOOTTY_LIVE_MOSH_TARGET is not set" mosh
fi

if [[ -n ${BOOTTY_LIVE_DOCKER_CONTAINER:-} ]]; then
  if command -v docker >/dev/null 2>&1; then
    run_probe docker_exec docker_exec docker exec "$BOOTTY_LIVE_DOCKER_CONTAINER" sh -lc "$remote_probe"
  else
    skip_probe docker_exec "docker not found" docker_exec
  fi
else
  skip_probe docker_exec "BOOTTY_LIVE_DOCKER_CONTAINER is not set" docker_exec
fi

if [[ -n ${BOOTTY_LIVE_PODMAN_CONTAINER:-} ]]; then
  if command -v podman >/dev/null 2>&1; then
    run_probe podman_exec podman_exec podman exec "$BOOTTY_LIVE_PODMAN_CONTAINER" sh -lc "$remote_probe"
  else
    skip_probe podman_exec "podman not found" podman_exec
  fi
else
  skip_probe podman_exec "BOOTTY_LIVE_PODMAN_CONTAINER is not set" podman_exec
fi

printf 'Wrote live remote benchmark results: %s\n' "$output_file"
