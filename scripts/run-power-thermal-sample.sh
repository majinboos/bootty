#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/run-power-thermal-sample.sh [output-dir] -- <command> [args...]

Runs an opt-in host power/thermal sample around a command and writes JSONL
metadata plus raw tool logs. This stays outside routine validation because host
power tooling is platform-specific and may require privileges.

Environment:
  BOOTTY_POWER_SECONDS=10
  BOOTTY_POWER_INTERVAL_MS=1000
USAGE
}

if [[ ${1:-} == "-h" || ${1:-} == "--help" ]]; then
  usage
  exit 0
fi

if [[ $# -lt 3 || $2 != "--" ]]; then
  usage >&2
  exit 2
fi

output_dir=$1
shift 2
cmd=("$@")
mkdir -p "$output_dir"
summary=$output_dir/summary.jsonl
: >"$summary"
seconds=${BOOTTY_POWER_SECONDS:-10}
interval_ms=${BOOTTY_POWER_INTERVAL_MS:-1000}

json_escape() {
  local value=${1-}
  value=${value//\\/\\\\}
  value=${value//"/\\"}
  value=${value//$'\n'/\\n}
  value=${value//$'\r'/\\r}
  printf '%s' "$value"
}

emit() {
  printf '%s\n' "$1" >>"$summary"
}

command_string=$(printf '%q ' "${cmd[@]}")
command_string=${command_string% }
emit "{\"schema_version\":1,\"event\":\"power_metadata\",\"recorded_at_utc\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"uname\":\"$(json_escape "$(uname -a)")\",\"command\":\"$(json_escape "$command_string")\"}"

sample_pid=""
sample_log=""
start_sampler() {
  if command -v powermetrics >/dev/null 2>&1; then
    sample_log=$output_dir/powermetrics.log
    sudo powermetrics --samplers cpu_power,gpu_power,thermal -i "$interval_ms" -n "$seconds" 2>&1 | sudo tee "$sample_log" >/dev/null 2>&1 &
    sample_pid=$!
    emit "{\"schema_version\":1,\"event\":\"power_sampler\",\"tool\":\"powermetrics\",\"status\":\"started\",\"log\":\"$(json_escape "$sample_log")\"}"
  elif command -v pidstat >/dev/null 2>&1; then
    sample_log=$output_dir/pidstat.log
    pidstat -durh 1 "$seconds" >"$sample_log" 2>&1 &
    sample_pid=$!
    emit "{\"schema_version\":1,\"event\":\"power_sampler\",\"tool\":\"pidstat\",\"status\":\"started\",\"log\":\"$(json_escape "$sample_log")\"}"
  elif command -v nvidia-smi >/dev/null 2>&1; then
    sample_log=$output_dir/nvidia-smi.log
    nvidia-smi --query-gpu=timestamp,power.draw,utilization.gpu,temperature.gpu --format=csv -l 1 >"$sample_log" 2>&1 &
    sample_pid=$!
    emit "{\"schema_version\":1,\"event\":\"power_sampler\",\"tool\":\"nvidia-smi\",\"status\":\"started\",\"log\":\"$(json_escape "$sample_log")\"}"
  else
    emit "{\"schema_version\":1,\"event\":\"power_sampler\",\"status\":\"skipped\",\"detail\":\"no powermetrics, pidstat, or nvidia-smi found\"}"
  fi
}

stop_sampler() {
  if [[ -n $sample_pid ]] && kill -0 "$sample_pid" >/dev/null 2>&1; then
    wait "$sample_pid" || true
  fi
}

start=$(date +%s)
start_sampler
set +e
"${cmd[@]}" >"$output_dir/command.stdout" 2>"$output_dir/command.stderr"
exit_code=$?
set -e
stop_sampler
end=$(date +%s)
status=pass
if [[ $exit_code -ne 0 ]]; then
  status=fail
fi
emit "{\"schema_version\":1,\"event\":\"power_command\",\"status\":\"$status\",\"exit_code\":$exit_code,\"duration_s\":$((end - start)),\"stdout\":\"$(json_escape "$output_dir/command.stdout")\",\"stderr\":\"$(json_escape "$output_dir/command.stderr")\"}"
printf 'Wrote power/thermal evidence: %s\n' "$output_dir"
exit "$exit_code"
