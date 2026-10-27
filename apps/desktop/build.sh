#!/usr/bin/env bash
# Build the LayerAgent desktop app.
#
# Usage:
#   ./build.sh dev      compile the shell (no frontend bundle required)
#   ./build.sh bundle   build the installable bundle (frontend bundle required)
#
# The frontend lives in ../../frontend and is built by its own tool chain.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
frontend="${here}/../../frontend"
frontend_dist="${frontend}/dist"
tauri_dir="${here}/src-tauri"

mode="${1:-bundle}"

usage() {
  echo "usage: $(basename "$0") [dev|bundle]" >&2
  exit 2
}

require_frontend() {
  if [ ! -d "${frontend_dist}" ]; then
    echo "error: the frontend bundle is missing: ${frontend_dist}" >&2
    echo "       build it first:  (cd ../../frontend && npm install && npm run build)" >&2
    exit 1
  fi
}

case "${mode}" in
  dev)
    echo "==> compiling the desktop shell"
    (cd "${tauri_dir}" && cargo build)
    ;;
  bundle)
    require_frontend
    echo "==> building the desktop bundle"
    if command -v cargo-tauri >/dev/null 2>&1; then
      (cd "${tauri_dir}" && cargo tauri build)
    else
      echo "error: the Tauri CLI is missing; install it with: cargo install tauri-cli --version '^2'" >&2
      exit 1
    fi
    ;;
  *)
    usage
    ;;
esac
