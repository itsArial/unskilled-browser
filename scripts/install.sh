#!/usr/bin/env bash
# Installs a release build as a desktop app:
#   ~/.local/lib/unskilled-browser/versions/<id>/  binary + stripped CEF runtime ($ORIGIN rpath)
#   ~/.local/lib/unskilled-browser/current         symlink to the newest version
#   ~/.local/bin/unskilled-browser, launcher entry and icon
# Every install is a new version directory (unchanged CEF files are hard-linked, so it costs no
# space) and `current` flips atomically. A running browser keeps using its own version — Chromium
# re-executes its own binary for helper processes — and old versions are removed once nothing
# runs from them.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

root="$HOME/.local/lib/unskilled-browser"
stamp="$root/.installed"
# Taken before building: sources edited during the build still count as newer than the install.
started=$(mktemp)

cargo build --release -p unskilled
cef=$(ls -dt target/release/build/cef-dll-sys-*/out/cef_linux_x86_64 | head -1)
cef_id=$(sha256sum "$cef/archive.json" | cut -c1-16)

mkdir -p "$root/versions"
version="$root/versions/$(date +%Y%m%d-%H%M%S)-$$"
previous=$(readlink -f "$root/current" 2>/dev/null || true)

mkdir -p "$version/locales"
if [[ -n "$previous" && -f "$previous/.cef" && "$(cat "$previous/.cef")" == "$cef_id" ]]; then
    cp -al "$previous/." "$version/"
    rm -f "$version/unskilled"
else
    for f in libcef.so libvk_swiftshader.so libvulkan.so.1 vk_swiftshader_icd.json icudtl.dat \
             resources.pak chrome_100_percent.pak chrome_200_percent.pak v8_context_snapshot.bin; do
        cp "$cef/$f" "$version/"
    done
    # Only the UI locale we use (en-US); the full set is 50 MB.
    cp "$cef"/locales/en-US*.pak "$version/locales/"
    # The downloaded CEF ships full debug symbols (1.4 GB): every Chromium process maps it.
    strip --strip-unneeded "$version/libcef.so" "$version/libvk_swiftshader.so"
    echo "$cef_id" > "$version/.cef"
fi
cp target/release/unskilled "$version/unskilled"

# Atomic switch.
ln -sfn "$version" "$root/current.new"
mv -Tf "$root/current.new" "$root/current"

# Drop versions nothing runs from (and the pre-versioning flat layout).
in_use() {
    local dir=$1
    for exe in /proc/[0-9]*/exe; do
        [[ "$(readlink "$exe" 2>/dev/null)" == "$dir/"* ]] && return 0
    done
    return 1
}
for old in "$root"/versions/*; do
    [[ "$old" == "$version" ]] && continue
    in_use "$old" || rm -rf "$old"
done
if [[ -f "$root/unskilled" ]] && ! in_use "$root"; then
    rm -rf "$root/unskilled" "$root/libcef.so" "$root/libvk_swiftshader.so" "$root/libvulkan.so.1" \
        "$root/vk_swiftshader_icd.json" "$root/icudtl.dat" "$root"/*.pak "$root/v8_context_snapshot.bin" \
        "$root/locales" "$root/.cef"
fi

mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"
ln -sfn "$root/current/unskilled" "$HOME/.local/bin/unskilled-browser"
rm -f "$HOME/.local/share/icons/hicolor/scalable/apps/unskilled-browser.svg"
for size in 32 48 64 128 256 512; do
    dir="$HOME/.local/share/icons/hicolor/${size}x${size}/apps"
    mkdir -p "$dir"
    cp "assets/icons/unskilled-browser-$size.png" "$dir/unskilled-browser.png"
done
gtk-update-icon-cache -q -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
cat > "$HOME/.local/share/applications/unskilled-browser.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=unskilled browser
GenericName=Web Browser
Comment=Private browser
Keywords=browser;web;internet;private;unskilled;
Icon=unskilled-browser
Exec=$root/current/unskilled
Terminal=false
Categories=Network;WebBrowser;
StartupWMClass=unskilled-browser
DESKTOP

mv -f "$started" "$stamp"
echo "installed $(basename "$version"): $(du -sh "$version" | cut -f1) ($(ls "$root/versions" | wc -l) version(s) kept)"
