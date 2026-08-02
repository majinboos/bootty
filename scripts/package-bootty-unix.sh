#!/usr/bin/env bash
set -euo pipefail

APP_NAME="Bootty"
BINARY_NAME="bootty"
PACKAGE_NAME="bootty-app"
DIST_DIR="${BOOTTY_DIST_DIR:-dist}"
TARGET_ROOT="${CARGO_TARGET_DIR:-target}"
MACOS_ICON_NAME="bootty"
MACOS_ICON_SOURCE="crates/bootty-app/assets/$MACOS_ICON_NAME.icon"
VERSION="${BOOTTY_VERSION:-$(awk '
  $0 == "[workspace.package]" { in_workspace_package = 1; next }
  /^\[/ { in_workspace_package = 0 }
  in_workspace_package && $1 == "version" { gsub(/\"/, "", $3); print $3; exit }
' Cargo.toml)}"

PROFILE="release"
CARGO_PROFILE_ARGS=(--release)
FAST=0
LINKAGE="dynamic"
while (($#)); do
  case "$1" in
    --fast)
      FAST=1
      ;;
    --static)
      LINKAGE="static"
      ;;
    *)
      echo "unknown package argument: $1" >&2
      exit 2
      ;;
  esac
  shift
done
if [[ "$FAST" -eq 1 ]]; then
  PROFILE="fast-release"
  CARGO_PROFILE_ARGS=(--profile fast-release)
elif [[ "$LINKAGE" == "dynamic" ]]; then
  PROFILE="dynamic-release"
  CARGO_PROFILE_ARGS=(--profile dynamic-release)
fi

VERSION="${VERSION:-0.0.0}"

ensure_project_zig() {
  local required_zig zig_path zig_version
  required_zig="$(awk -F'"' '$1 ~ /^zig[[:space:]]*=/ { print $2; exit }' mise.toml)"

  if command -v mise >/dev/null 2>&1; then
    zig_path="$(mise which zig 2>/dev/null || true)"
    if [[ -n "$zig_path" && -x "$zig_path" ]]; then
      export PATH="$(dirname "$zig_path"):$PATH"
    fi
  fi

  if ! zig_version="$(zig version 2>/dev/null)"; then
    echo "Zig $required_zig is required to package $APP_NAME; install it with mise" >&2
    exit 1
  fi

  if [[ -n "$required_zig" && "$zig_version" != "$required_zig" ]]; then
    echo "Zig $required_zig is required to package $APP_NAME; found $zig_version at $(command -v zig)" >&2
    exit 1
  fi
}

append_rustflags() {
  if [[ -n "${RUSTFLAGS:-}" ]]; then
    export RUSTFLAGS="$RUSTFLAGS $*"
  else
    export RUSTFLAGS="$*"
  fi
}

enable_dynamic_linkage() {
  case "$(uname -s)" in
    Darwin)
      append_rustflags -C prefer-dynamic -C link-arg=-Wl,-rpath,@executable_path/../Frameworks
      ;;
    Linux)
      append_rustflags -C prefer-dynamic -C 'link-arg=-Wl,-rpath,$ORIGIN/../lib'
      ;;
    *)
      echo "dynamic packaging is unsupported on $(uname -s); pass --static" >&2
      exit 1
      ;;
  esac
}

copy_dynamic_libraries() {
  local binary_path="$1"
  local dest_dir="$2"
  local copied=0
  local dependency_name source_dir library
  local -a dependency_names=()

  case "$(uname -s)" in
    Darwin)
      while IFS= read -r dependency_name; do
        dependency_names+=("$dependency_name")
      done < <(otool -L "$binary_path" | awk '/@rpath\/.*\.dylib/ { n=$1; sub("@rpath/", "", n); print n }')
      ;;
    Linux)
      while IFS= read -r dependency_name; do
        dependency_names+=("$dependency_name")
      done < <(ldd "$binary_path" | awk '/=>/ { n=$1; if (n ~ /^lib.*\.so/) print n }')
      ;;
  esac

  if [[ "${#dependency_names[@]}" -eq 0 ]]; then
    echo "expected dynamic Rust libraries referenced by $binary_path" >&2
    exit 1
  fi

  mkdir -p "$dest_dir"
  for dependency_name in "${dependency_names[@]}"; do
    library=""
    for source_dir in "$TARGET_ROOT/$PROFILE/deps" "$(rustc --print target-libdir)"; do
      if [[ -f "$source_dir/$dependency_name" ]]; then
        library="$source_dir/$dependency_name"
        break
      fi
    done

    if [[ -z "$library" ]]; then
      echo "could not find dynamic dependency $dependency_name for $binary_path" >&2
      exit 1
    fi

    cp -f "$library" "$dest_dir/"
    copied=1
  done

  if [[ "$copied" -eq 0 ]]; then
    echo "expected dynamic Rust libraries under $TARGET_ROOT/$PROFILE/deps or rustc target-libdir" >&2
    exit 1
  fi
}

ensure_project_zig
if [[ "$LINKAGE" == "dynamic" ]]; then
  enable_dynamic_linkage
fi
rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"

cargo build "${CARGO_PROFILE_ARGS[@]}" -p "$PACKAGE_NAME" --bin "$BINARY_NAME"

case "$(uname -s)" in
  Darwin)
    ARCH="$(uname -m)"
    BUNDLE_DIR="$DIST_DIR/$APP_NAME.app"
    CONTENTS_DIR="$BUNDLE_DIR/Contents"
    MACOS_DIR="$CONTENTS_DIR/MacOS"
    RESOURCES_DIR="$CONTENTS_DIR/Resources"

    mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"
    cp "$TARGET_ROOT/$PROFILE/$BINARY_NAME" "$MACOS_DIR/$BINARY_NAME"
    if [[ "$LINKAGE" == "dynamic" ]]; then
      copy_dynamic_libraries "$MACOS_DIR/$BINARY_NAME" "$CONTENTS_DIR/Frameworks"
    fi
    ACTOOL="$(xcrun --find actool 2>/dev/null || true)"
    if [[ -z "$ACTOOL" ]]; then
      echo "Xcode actool is required to package the macOS app icon" >&2
      exit 1
    fi
    ICON_PARTIAL_INFO="$CONTENTS_DIR/assetcatalog-info.plist"
    ACTOOL_LOG="$(mktemp)"
    if ! "$ACTOOL" "$MACOS_ICON_SOURCE" \
      --compile "$RESOURCES_DIR" \
      --app-icon "$MACOS_ICON_NAME" \
      --enable-on-demand-resources NO \
      --development-region en \
      --target-device mac \
      --platform macosx \
      --enable-icon-stack-fallback-generation=enabled \
      --include-all-app-icons \
      --minimum-deployment-target 13.0 \
      --output-partial-info-plist "$ICON_PARTIAL_INFO" \
      >"$ACTOOL_LOG" 2>&1; then
      echo "actool failed compiling the app icon:" >&2
      cat "$ACTOOL_LOG" >&2
      rm -f "$ICON_PARTIAL_INFO" "$ACTOOL_LOG"
      exit 1
    fi
    rm -f "$ICON_PARTIAL_INFO" "$ACTOOL_LOG"
    chmod +x "$MACOS_DIR/$BINARY_NAME"

    cat > "$CONTENTS_DIR/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleDisplayName</key>
  <string>$APP_NAME</string>
  <key>CFBundleExecutable</key>
  <string>$BINARY_NAME</string>
  <key>CFBundleIconFile</key>
  <string>bootty</string>
  <key>CFBundleIconName</key>
  <string>$MACOS_ICON_NAME</string>
  <key>CFBundleIdentifier</key>
  <string>dev.bootty.desktop</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>$APP_NAME</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$VERSION</string>
  <key>CFBundleVersion</key>
  <string>$VERSION</string>
  <key>LSMinimumSystemVersion</key>
  <string>13.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

    if command -v codesign >/dev/null 2>&1; then
      if [[ -d "$CONTENTS_DIR/Frameworks" ]]; then
        find "$CONTENTS_DIR/Frameworks" -type f -name '*.dylib' -exec codesign --force --sign - {} \;
      fi
      codesign --force --sign - \
        --requirements '=designated => identifier "dev.bootty.desktop"' \
        "$BUNDLE_DIR"
    fi

    (cd "$DIST_DIR" && zip -qry "$APP_NAME-macos-$ARCH.app.zip" "$APP_NAME.app")
    ;;
  Linux)
    ARCH="$(uname -m)"
    ROOT_DIR="$DIST_DIR/$APP_NAME-linux-$ARCH"

    mkdir -p \
      "$ROOT_DIR/bin" \
      "$ROOT_DIR/share/applications" \
      "$ROOT_DIR/share/icons/hicolor/256x256/apps" \
      "$ROOT_DIR/share/icons/hicolor/scalable/apps"

    cp "$TARGET_ROOT/$PROFILE/$BINARY_NAME" "$ROOT_DIR/bin/$BINARY_NAME"
    if [[ "$LINKAGE" == "dynamic" ]]; then
      copy_dynamic_libraries "$ROOT_DIR/bin/$BINARY_NAME" "$ROOT_DIR/lib"
    fi
    cp "crates/bootty-app/assets/bootty-mascot.png" "$ROOT_DIR/share/icons/hicolor/256x256/apps/bootty.png"
    cp "crates/bootty-app/assets/bootty-mascot.svg" "$ROOT_DIR/share/icons/hicolor/scalable/apps/bootty.svg"
    chmod +x "$ROOT_DIR/bin/$BINARY_NAME"

    cat > "$ROOT_DIR/share/applications/dev.bootty.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=$APP_NAME
Comment=Native GPU-rendered terminal
Exec=$BINARY_NAME
Icon=bootty
Terminal=false
Categories=System;TerminalEmulator;
DESKTOP

    tar -C "$DIST_DIR" -czf "$DIST_DIR/$APP_NAME-linux-$ARCH.tar.gz" "$APP_NAME-linux-$ARCH"
    ;;
  *)
    echo "unsupported OS: $(uname -s)" >&2
    exit 1
    ;;
esac

find "$DIST_DIR" -maxdepth 2 -type f -print
