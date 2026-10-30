#!/usr/bin/env bash
# Builds and runs unskilled against the CEF runtime the `cef` crate downloaded.
#   scripts/run.sh [--release]
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
profile=debug
if [[ "${1:-}" == "--release" ]]; then profile=release; shift; fi
cargo build -q -p unskilled $([[ $profile == release ]] && echo --release)
cef_dir=$(ls -dt "target/$profile"/build/cef-dll-sys-*/out/cef_linux_x86_64 | head -1)
export LD_LIBRARY_PATH="$PWD/$cef_dir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "target/$profile/unskilled" "$@"
