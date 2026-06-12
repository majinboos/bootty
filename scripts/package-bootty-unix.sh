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
VERSION="${VERSION:-0.0.0}"

rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"

cargo build --release -p "$PACKAGE_NAME" --bin "$BINARY_NAME"

case "$(uname -s)" in
  Darwin)
    ARCH="$(uname -m)"
    BUNDLE_DIR="$DIST_DIR/$APP_NAME.app"
    CONTENTS_DIR="$BUNDLE_DIR/Contents"
    MACOS_DIR="$CONTENTS_DIR/MacOS"
    RESOURCES_DIR="$CONTENTS_DIR/Resources"

    mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"
    cp "$TARGET_ROOT/release/$BINARY_NAME" "$MACOS_DIR/$BINARY_NAME"
    ACTOOL="$(xcrun --find actool 2>/dev/null || true)"
    if [[ -z "$ACTOOL" ]]; then
      echo "Xcode 26 actool is required to package the macOS Liquid Glass app icon" >&2
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
      codesign --force --deep --sign - "$BUNDLE_DIR"
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

    cp "$TARGET_ROOT/release/$BINARY_NAME" "$ROOT_DIR/bin/$BINARY_NAME"
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
