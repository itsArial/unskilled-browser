#!/usr/bin/env bash
# Builds a portable release bundle into dist/:
#   dist/unskilled-browser-<version>-linux-x86_64.tar.gz   (Linux)
#   dist/unskilled-browser-<version>-windows-x86_64.zip    (Windows, run from Git Bash)
# plus a .sha256 next to each. The bundle is the binary with the stripped CEF runtime beside it;
# it runs from wherever it's unpacked. On Windows the binary is CEF's bootstrap.exe as unskilled.exe
# (our icon and version info, crates/winmain/build.rs) loading the browser from unskilled.dll:
# that is what runs Chromium's sub-processes sandboxed there.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
case "$(uname -s)" in
    Linux*) os=linux ;;
    MINGW* | MSYS* | CYGWIN*) os=windows ;;
    *) echo "unsupported platform: $(uname -s)" >&2; exit 1 ;;
esac

if [[ $os == linux ]]; then
    cargo build --release --locked -p unskilled
else
    cargo build --release --locked -p unskilled-winmain
fi
cef=$(ls -dt target/release/build/cef-dll-sys-*/out/cef_${os}_x86_64 | head -1)

name="unskilled-browser-$version-$os-x86_64"
out="dist/$name"
rm -rf "$out"
mkdir -p "$out/locales"

if [[ $os == linux ]]; then
    for f in libcef.so libvk_swiftshader.so libvulkan.so.1 vk_swiftshader_icd.json icudtl.dat \
             resources.pak chrome_100_percent.pak chrome_200_percent.pak v8_context_snapshot.bin; do
        cp "$cef/$f" "$out/"
    done
    # The CEF download ships full debug symbols (1.4 GB).
    strip --strip-unneeded "$out/libcef.so" "$out/libvk_swiftshader.so"
    cp target/release/unskilled "$out/unskilled"
    mkdir -p "$out/icons"
    cp assets/icons/unskilled-browser-*.png "$out/icons/"
else
    cp "$cef"/*.dll "$cef"/*.dat "$cef"/*.bin "$cef"/*.pak "$cef/vk_swiftshader_icd.json" "$out/"
    # The bootstrap loads <its own name>.dll from its directory.
    cp "$(ls -dt target/release/build/unskilled-winmain-*/out/unskilled.exe | head -1)" "$out/unskilled.exe"
    cp target/release/unskilled_winmain.dll "$out/unskilled.dll"
fi
# Only the UI locale in use (en-US); the full set is 50 MB.
cp "$cef"/locales/en-US*.pak "$out/locales/"
cp LICENSE README.md THIRD_PARTY_NOTICES.md "$out/"
cp "$cef/CREDITS.html" "$out/CHROMIUM_CREDITS.html"

cd dist
if [[ $os == linux ]]; then
    archive="$name.tar.gz"
    tar czf "$archive" "$name"
else
    archive="$name.zip"
    7z a -tzip -mx=9 "$archive" "$name" > /dev/null
fi
sha256sum "$archive" > "$archive.sha256"
echo "dist/$archive ($(du -h "$archive" | cut -f1))"
