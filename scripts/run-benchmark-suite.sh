#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/run-benchmark-suite.sh [--ci-smoke] [--quick] [--output DIR]

Runs Bootty benchmark reproduction commands sequentially and writes JSONL command
metadata. Default mode is compile-only for all benchmark targets, suitable for
checking that the suite is present without running measured Criterion suites.

Options:
  --ci-smoke    run only the fast benchmark gate for blocking CI
  --quick       also run a small representative measured subset
  --output DIR write logs and summary.jsonl under DIR
USAGE
}

ci_smoke=0
quick=0
output_dir=artifacts/benchmark-reproduction/$(date -u +%Y%m%dT%H%M%SZ)
while [[ $# -gt 0 ]]; do
  case $1 in
    --ci-smoke)
      ci_smoke=1
      shift
      ;;
    --quick)
      quick=1
      shift
      ;;
    --output)
      if [[ $# -lt 2 ]]; then
        usage >&2
        exit 2
      fi
      output_dir=$2
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
done

mkdir -p "$output_dir"
summary=$output_dir/summary.jsonl
: >"$summary"
failures=0

json_escape() {
  local value=${1-}
  value=${value//\\/\\\\}
  value=${value//"/\\"}
  value=${value//$'\n'/\\n}
  value=${value//$'\r'/\\r}
  printf '%s' "$value"
}

emit_metadata() {
  local commit rust cargo
  commit=$(git rev-parse HEAD 2>/dev/null || printf unknown)
  rust=$(rustc --version 2>/dev/null || printf unknown)
  cargo=$(cargo --version 2>/dev/null || printf unknown)
  printf '{"schema_version":1,"event":"benchmark_reproduction_metadata","recorded_at_utc":"%s","commit":"%s","uname":"%s","rustc":"%s","cargo":"%s"}\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    "$(json_escape "$commit")" \
    "$(json_escape "$(uname -a)")" \
    "$(json_escape "$rust")" \
    "$(json_escape "$cargo")" >>"$summary"
}

run_logged() {
  local name=$1
  shift
  local log=$output_dir/$name.log
  local command_string start end exit_code status detail
  command_string=$(printf '%q ' "$@")
  command_string=${command_string% }
  start=$(date +%s)
  set +e
  "$@" >"$log" 2>&1
  exit_code=$?
  set -e
  end=$(date +%s)
  if [[ $exit_code -eq 0 ]]; then
    status=pass
    detail=ok
  else
    status=fail
    detail=$(tail -n 1 "$log" 2>/dev/null || printf 'command failed')
  fi
  printf '{"schema_version":1,"event":"benchmark_reproduction_command","name":"%s","status":"%s","detail":"%s","duration_s":%s,"exit_code":%s,"command":"%s","log":"%s"}\n' \
    "$(json_escape "$name")" \
    "$status" \
    "$(json_escape "$detail")" \
    "$((end - start))" \
    "$exit_code" \
    "$(json_escape "$command_string")" \
    "$(json_escape "$log")" >>"$summary"
  return "$exit_code"
}

bench_targets=(
  paint_plan
  pipeline_resources
  paint_plan_wgpu
  startup_config
  startup_milestones
  kitty_image
  graphics_protocols
  app_frame
  text_atlas
  hostile_input
  panes_multiwindow
  multiplexer
  remote_session
  real_app_replay
  resize_reflow
  scrollback
  parser_control
  render_pacing
  input_latency
  idle_overhead
  power_thermal
  input_protocols
)

emit_metadata
if ! run_logged validate_benchmark_manifests scripts/validate-benchmark-manifests.py; then
  failures=$((failures + 1))
fi
if ! run_logged validate_benchmark_dashboard scripts/build-benchmark-dashboard.py --self-test; then
  failures=$((failures + 1))
fi
if [[ $ci_smoke -eq 1 ]]; then
  if ! run_logged compile_paint_plan cargo test -p bootty-app --bench paint_plan --no-run; then
    failures=$((failures + 1))
  fi
  if [[ $failures -ne 0 ]]; then
    printf 'Wrote benchmark reproduction evidence with %s failure(s): %s\n' "$failures" "$output_dir" >&2
    exit 1
  fi
  printf 'Wrote benchmark reproduction evidence: %s\n' "$output_dir"
  exit 0
fi

for target in "${bench_targets[@]}"; do
  if ! run_logged "compile_$target" cargo test -p bootty-app --bench "$target" --no-run; then
    failures=$((failures + 1))
  fi
done
if ! run_logged compile_pty_drain cargo test -p bootty-runtime --bench pty_drain --no-run; then
  failures=$((failures + 1))
fi
if ! run_logged compile_flood_response cargo test -p bootty-runtime --bench flood_response --no-run; then
  failures=$((failures + 1))
fi

if [[ $quick -eq 1 ]]; then
  if ! run_logged quick_paint_plan_smoke cargo test -p bootty-app --bench paint_plan; then
    failures=$((failures + 1))
  fi
  if ! run_logged quick_input_protocols cargo bench -p bootty-app --bench input_protocols input_protocol_keyboard_legacy_printable -- --sample-size 10 --measurement-time 0.2 --warm-up-time 0.1; then
    failures=$((failures + 1))
  fi
  if ! run_logged quick_power_thermal cargo bench -p bootty-app --bench power_thermal power_thermal_idle_prompt_1s_render_model -- --sample-size 10 --measurement-time 0.2 --warm-up-time 0.1; then
    failures=$((failures + 1))
  fi
fi

if [[ $failures -ne 0 ]]; then
  printf 'Wrote benchmark reproduction evidence with %s failure(s): %s\n' "$failures" "$output_dir" >&2
  exit 1
fi
printf 'Wrote benchmark reproduction evidence: %s\n' "$output_dir"
