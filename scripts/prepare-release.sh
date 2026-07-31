#!/usr/bin/env bash
set -euo pipefail

bump="${1:-patch}"
current="$(awk '$0 == "[workspace.package]" { in_package = 1; next } /^\[/ { in_package = 0 } in_package && $1 == "version" { gsub(/"/, "", $3); print $3; exit }' Cargo.toml)"

if [[ ! "$current" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
  echo "unsupported workspace version: $current" >&2
  exit 1
fi

major="${BASH_REMATCH[1]}"
minor="${BASH_REMATCH[2]}"
patch="${BASH_REMATCH[3]}"
case "$bump" in
  major) next="$((major + 1)).0.0" ;;
  minor) next="$major.$((minor + 1)).0" ;;
  patch) next="$major.$minor.$((patch + 1))" ;;
  *) echo "usage: mise run release -- patch|minor|major" >&2; exit 2 ;;
esac

[[ "$(git branch --show-current)" == "main" ]] || { echo "run releases from main" >&2; exit 1; }
[[ -z "$(git status --porcelain)" ]] || { echo "working tree is not clean" >&2; exit 1; }
git fetch origin main --quiet
[[ "$(git rev-parse HEAD)" == "$(git rev-parse origin/main)" ]] || { echo "main is not synced with origin/main" >&2; exit 1; }
! git ls-remote --exit-code --tags origin "refs/tags/v$next" >/dev/null 2>&1 || { echo "v$next already exists" >&2; exit 1; }

branch="${GIT_USERNAME:-$(whoami)}/release-v$next"
gh stack init "$branch"
awk -v version="$next" '
  $0 == "[workspace.package]" { in_package = 1 }
  in_package && $1 == "version" { sub(/"[^"]+"/, "\"" version "\""); in_package = 0 }
  { print }
' Cargo.toml > Cargo.toml.tmp
mv Cargo.toml.tmp Cargo.toml
cargo metadata --format-version 1 >/dev/null

git add Cargo.toml Cargo.lock
git commit -m "chore(release): prepare v$next"
gh stack submit --auto --open
gh pr merge --auto --squash

echo "v$next will publish after the release PR passes CI and merges."
