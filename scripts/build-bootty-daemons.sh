#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "cross-building every Bootty daemon target currently requires macOS" >&2
  exit 1
fi

TARGET_ROOT="${CARGO_TARGET_DIR:-target}"
OUTPUT_DIR="${BOOTTY_DAEMON_OUTPUT_DIR:-$TARGET_ROOT/bootty-daemons}"
PACKAGE="bootty-daemon"
PROFILE="daemon-release"

TARGETS=(
  aarch64-apple-darwin
  x86_64-apple-darwin
  x86_64-unknown-linux-gnu
  aarch64-unknown-linux-gnu
  x86_64-pc-windows-msvc
)

rustup target add "${TARGETS[@]}"
rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

for target in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --profile "$PROFILE" -p "$PACKAGE" --target "$target"
  cp "$TARGET_ROOT/$target/$PROFILE/bootty-daemon" "$OUTPUT_DIR/bootty-daemon-$target"
done

for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
  cargo zigbuild --profile "$PROFILE" -p "$PACKAGE" --target "$target"
  cp "$TARGET_ROOT/$target/$PROFILE/bootty-daemon" "$OUTPUT_DIR/bootty-daemon-$target"
done

cargo xwin build --profile "$PROFILE" -p "$PACKAGE" --target x86_64-pc-windows-msvc
cp "$TARGET_ROOT/x86_64-pc-windows-msvc/$PROFILE/bootty-daemon.exe" \
  "$OUTPUT_DIR/bootty-daemon-x86_64-pc-windows-msvc"

for target in "${TARGETS[@]}"; do
  test -s "$OUTPUT_DIR/bootty-daemon-$target"
done

find "$OUTPUT_DIR" -maxdepth 1 -type f -print
