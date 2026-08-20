#!/usr/bin/env bash
set -euo pipefail

PACKAGE_NAME="bootty-app"
BINARY_NAME="bootty"
CARGO_PROFILE_ARGS=(--release)
CARGO_FEATURE_ARGS=(--features bootty-dev)
FAST=0
LINKAGE="dynamic"

append_rustflags() {
  if [[ -n "${RUSTFLAGS:-}" ]]; then
    export RUSTFLAGS="$RUSTFLAGS $*"
  else
    export RUSTFLAGS="$*"
  fi
}

while (($#)); do
  case "$1" in
    --fast)
      FAST=1
      ;;
    --static)
      LINKAGE="static"
      ;;
    *)
      echo "unknown build argument: $1" >&2
      exit 2
      ;;
  esac
  shift
done
if [[ "$FAST" -eq 1 ]]; then
  CARGO_PROFILE_ARGS=(--profile fast-release)
elif [[ "$LINKAGE" == "dynamic" ]]; then
  CARGO_PROFILE_ARGS=(--profile dynamic-release)
fi

if [[ "$LINKAGE" == "dynamic" ]]; then
  append_rustflags -C prefer-dynamic -C rpath
fi

cargo build "${CARGO_PROFILE_ARGS[@]}" "${CARGO_FEATURE_ARGS[@]}" -p "$PACKAGE_NAME" --bin "$BINARY_NAME"
