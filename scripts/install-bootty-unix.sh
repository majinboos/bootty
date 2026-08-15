#!/usr/bin/env bash
set -euo pipefail

APP_NAME="Bootty"
BINARY_NAME="bootty"
CLI_NAME="bootty"
BUNDLE_IDENTIFIER="dev.bootty.desktop"
DIST_DIR="${BOOTTY_DIST_DIR:-dist}"

for argument in "$@"; do
  if [[ "$argument" == "--dev" ]]; then
    APP_NAME="BoottyDev"
    CLI_NAME="bootty-dev"
    BUNDLE_IDENTIFIER="dev.bootty.desktop.dev"
  fi
done

ensure_user_path() {
  local directory="$1"
  case ":$PATH:" in
    *":$directory:"*) return ;;
  esac

  local profile line
  case "${SHELL##*/}" in
    fish)
      profile="$HOME/.config/fish/config.fish"
      line="fish_add_path \"$directory\""
      ;;
    zsh)
      profile="$HOME/.zprofile"
      line="export PATH=\"$directory:\$PATH\""
      ;;
    *)
      profile="$HOME/.profile"
      line="export PATH=\"$directory:\$PATH\""
      ;;
  esac
  mkdir -p "$(dirname "$profile")"
  touch "$profile"
  grep -Fqx "$line" "$profile" || printf '\n%s\n' "$line" >> "$profile"
  echo "Added $directory to PATH in $profile"
}

./scripts/package-bootty-unix.sh "$@"

case "$(uname -s)" in
  Darwin)
    INSTALL_DIR="${BOOTTY_INSTALL_DIR:-/Applications}"
    APP_SOURCE="$DIST_DIR/$APP_NAME.app"
    APP_TARGET="$INSTALL_DIR/$APP_NAME.app"

    if [[ ! -d "$APP_SOURCE" ]]; then
      echo "packaged app not found at $APP_SOURCE" >&2
      exit 1
    fi

    rm -rf "$APP_TARGET"
    cp -R "$APP_SOURCE" "$APP_TARGET"
    CLI_DIR="$HOME/.local/bin"
    IFS=: read -r -a PATH_DIRS <<< "$PATH"
    for CANDIDATE in "${PATH_DIRS[@]}"; do
      case "$CANDIDATE" in
        "$HOME/.local/bin"|"$HOME/bin"|/usr/local/bin|/opt/homebrew/bin)
          if [[ -d "$CANDIDATE" && -w "$CANDIDATE" ]]; then
            CLI_DIR="$CANDIDATE"
            break
          fi
          ;;
      esac
    done
    mkdir -p "$CLI_DIR"
    ln -sfn "$APP_TARGET/Contents/MacOS/$BINARY_NAME" "$CLI_DIR/$CLI_NAME"
    ensure_user_path "$CLI_DIR"
    echo "Installed $APP_TARGET and $CLI_DIR/$CLI_NAME"
    ;;
  Linux)
    PREFIX="${BOOTTY_INSTALL_PREFIX:-$HOME/.local}"
    ARCH="$(uname -m)"
    ROOT_DIR="$DIST_DIR/$APP_NAME-linux-$ARCH"

    if [[ ! -d "$ROOT_DIR" ]]; then
      echo "packaged app not found at $ROOT_DIR" >&2
      exit 1
    fi

    install -Dm755 "$ROOT_DIR/bin/$CLI_NAME" "$PREFIX/bin/$CLI_NAME"
    install -Dm755 "$ROOT_DIR/bin/bootty-daemon" "$PREFIX/bin/bootty-daemon"
    ensure_user_path "$PREFIX/bin"
    if [[ -d "$ROOT_DIR/lib" ]]; then
      mkdir -p "$PREFIX/lib"
      cp -f "$ROOT_DIR/lib/"*.so "$PREFIX/lib/"
    fi
    if [[ -d "$ROOT_DIR/share/bootty/daemons" ]]; then
      install -d "$PREFIX/share/bootty/daemons"
      install -m755 "$ROOT_DIR/share/bootty/daemons/"* "$PREFIX/share/bootty/daemons/"
    fi
    install -Dm644 "$ROOT_DIR/share/applications/$BUNDLE_IDENTIFIER.desktop" \
      "$PREFIX/share/applications/$BUNDLE_IDENTIFIER.desktop"
    install -Dm644 "$ROOT_DIR/share/icons/hicolor/256x256/apps/bootty.png" \
      "$PREFIX/share/icons/hicolor/256x256/apps/bootty.png"
    install -Dm644 "$ROOT_DIR/share/icons/hicolor/scalable/apps/bootty.svg" \
      "$PREFIX/share/icons/hicolor/scalable/apps/bootty.svg"

    if command -v update-desktop-database >/dev/null 2>&1; then
      update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
      gtk-update-icon-cache -q "$PREFIX/share/icons/hicolor" >/dev/null 2>&1 || true
    fi

    echo "Installed $PREFIX/bin/$CLI_NAME"
    ;;
  *)
    echo "unsupported OS: $(uname -s)" >&2
    exit 1
    ;;
esac
