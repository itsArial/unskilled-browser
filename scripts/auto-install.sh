#!/usr/bin/env bash
# Reinstalls the desktop build when the browser's sources changed since the last install.
# Run by a Claude Code Stop hook: returns at once; the build runs in the background and logs to
# ~/.cache/unskilled-browser/install.log. One install at a time.
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
stamp="$HOME/.local/lib/unskilled-browser/.installed"
log="${XDG_CACHE_HOME:-$HOME/.cache}/unskilled-browser/install.log"

if [[ -f "$stamp" ]]; then
    changed=$(find "$repo/crates" "$repo/assets" "$repo/Cargo.toml" "$repo/Cargo.lock" "$repo/scripts/install.sh" \
        -type f -newer "$stamp" -not -path '*/target/*' -print -quit 2>/dev/null)
    [[ -z "$changed" ]] && exit 0
fi

mkdir -p "$(dirname "$log")"
nohup flock -n "$log.lock" "$repo/scripts/install.sh" > "$log" 2>&1 < /dev/null &
disown
