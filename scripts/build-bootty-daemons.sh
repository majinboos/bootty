#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET_ROOT="${CARGO_TARGET_DIR:-target}"
OUTPUT_DIR="${BOOTTY_DAEMON_OUTPUT_DIR:-$TARGET_ROOT/bootty-daemons}"
PACKAGE="bootty-daemon"
PROFILE="daemon-release"
MANIFEST="$SCRIPT_DIR/bootty-daemon-targets.txt"

read_targets() {
  local target
  while IFS= read -r target || [[ -n "$target" ]]; do
    [[ -z "$target" || "$target" == \#* ]] && continue
    TARGETS+=("$target")
  done < "$MANIFEST"
}

verify_outputs() {
  local target
  for target in "${TARGETS[@]}"; do
    if [[ ! -s "$OUTPUT_DIR/bootty-daemon-$target" ]]; then
      echo "missing daemon artifact: $OUTPUT_DIR/bootty-daemon-$target" >&2
      return 1
    fi
  done
}

build_target() {
  local target="$1"
  local binary="bootty-daemon"
  local output="$OUTPUT_DIR/bootty-daemon-$target"

  case "$target" in
    aarch64-apple-darwin|x86_64-apple-darwin)
      if [[ "$(uname -s)" == "Darwin" ]]; then
        cargo build --profile "$PROFILE" -p "$PACKAGE" --target "$target"
      elif [[ -n "${SDKROOT:-}" ]]; then
        cargo zigbuild --profile "$PROFILE" -p "$PACKAGE" --target "$target"
      else
        echo "SDKROOT is required to cross-build $target outside Darwin" >&2
        return 1
      fi
      ;;
    x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu)
      cargo zigbuild --profile "$PROFILE" -p "$PACKAGE" --target "$target"
      ;;
    x86_64-pc-windows-msvc)
      cargo xwin build --profile "$PROFILE" -p "$PACKAGE" --target "$target"
      binary="bootty-daemon.exe"
      ;;
    *)
      echo "unsupported daemon target in $MANIFEST: $target" >&2
      return 1
      ;;
  esac

  cp "$TARGET_ROOT/$target/$PROFILE/$binary" "$output"
}

TARGETS=()
read_targets
[[ "${#TARGETS[@]}" -gt 0 ]] || { echo "daemon target manifest is empty: $MANIFEST" >&2; exit 1; }

if [[ -n "${BOOTTY_DAEMON_OUTPUT_DIR+x}" ]]; then
  if [[ -z "$BOOTTY_DAEMON_OUTPUT_DIR" ]]; then
    echo "BOOTTY_DAEMON_OUTPUT_DIR must name a complete staged daemon directory" >&2
    exit 1
  fi
  verify_outputs
  find "$OUTPUT_DIR" -maxdepth 1 -type f -print
  exit 0
fi

rustup target add "${TARGETS[@]}"
rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

for target in "${TARGETS[@]}"; do
  build_target "$target"
done

verify_outputs
find "$OUTPUT_DIR" -maxdepth 1 -type f -print
