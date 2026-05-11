#!/usr/bin/env bash
# Build the frontend bundle and run the Diffie desktop app.
#
# Usage:
#   ./run.sh              # `cargo tauri dev` — boots vite + Tauri with HMR
#   ./run.sh --release    # `cargo tauri build` then run the release binary
#   ./run.sh --test       # run backend unit tests only

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FRONTEND="$ROOT/src"
BACKEND="$ROOT/src-tauri"

mode="dev"
case "${1:-}" in
  --release) mode="release" ;;
  --test)    mode="test" ;;
  --dev|"")  mode="dev" ;;
  *)
    echo "unknown flag: $1" >&2
    echo "usage: $0 [--release|--test]" >&2
    exit 2
    ;;
esac

if [[ "$mode" == "test" ]]; then
  cd "$BACKEND"
  exec cargo test --no-default-features --lib
fi

# Install npm deps on first run.
if [[ ! -d "$FRONTEND/node_modules" ]]; then
  echo ">> installing frontend deps"
  (cd "$FRONTEND" && npm install --no-audit --no-fund)
fi

ensure_tauri_cli() {
  if ! cargo tauri --version >/dev/null 2>&1; then
    echo ">> tauri-cli not found, installing (cargo install tauri-cli --version ^2)"
    cargo install tauri-cli --version "^2"
  fi
}

case "$mode" in
  dev)
    ensure_tauri_cli
    cd "$BACKEND"
    exec cargo tauri dev --features desktop
    ;;
  release)
    ensure_tauri_cli
    cd "$BACKEND"
    exec cargo tauri build --features desktop
    ;;
esac
