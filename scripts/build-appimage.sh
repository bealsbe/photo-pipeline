#!/usr/bin/env bash
# Package the release build as a portable AppImage: one file to copy to
# another Linux PC and run.
#
#   scripts/build-appimage.sh            build (if needed) and package
#   scripts/build-appimage.sh --no-build package the binary already built
#
# Needs appimagetool on PATH, or APPIMAGETOOL=/path/to/appimagetool.
# Writes PhotoPipeline-<version>-x86_64.AppImage into dist/.
#
# What goes in: the app, libwebgpu_dawn.so beside it (the helpers find it
# there), and the few libraries a desktop may lack (libjpeg, libgomp).
# What stays out: glibc, libstdc++, and the graphics stack (Vulkan loader,
# EGL, Wayland, X11, xkbcommon). Those must come from the target system,
# so its drivers are used. The AppImage therefore runs on systems at
# least as new as the one that built it (same or newer glibc).
#
# Data stays outside the AppImage, as with any build: settings in
# ~/.config/photo-pipeline, names and the record of corrections in
# ~/.local/share/photo-pipeline, models and caches in
# ~/.cache/photo_pipeline.
set -euo pipefail

here="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
cd "$here"

build=1
for arg in "$@"; do
    case "$arg" in
        --no-build) build=0 ;;
        -h|--help) sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

tool="${APPIMAGETOOL:-$(command -v appimagetool || true)}"
if [[ -z "$tool" || ! -x "$tool" ]]; then
    echo "appimagetool not found: put it on PATH or set APPIMAGETOOL" >&2
    exit 1
fi

if ((build)); then
    ./build.sh
fi

bin="target/release/photo-pipeline"
dawn="target/release/libwebgpu_dawn.so"
for f in "$bin" "$dawn"; do
    [[ -e "$f" ]] || { echo "missing $f: run ./build.sh first" >&2; exit 1; }
done

app_id="$(sed -n 's/^pub const APP_ID: &str = "\(.*\)";/\1/p' src/app/mod.rs)"
[[ -n "$app_id" ]] || { echo "can't read APP_ID from src/app/mod.rs" >&2; exit 1; }
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
commit="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
appdir="$work/PhotoPipeline.AppDir"
mkdir -p "$appdir/usr/bin" "$appdir/usr/lib" "$appdir/usr/share/applications"

# The app and its WebGPU library side by side (cp -L: the build's copy of
# the library is a link into the cargo cache).
cp -L "$bin" "$appdir/usr/bin/photo-pipeline"
cp -L "$dawn" "$appdir/usr/bin/libwebgpu_dawn.so"

# Libraries some desktops lack. Everything else comes from the system.
for lib in libjpeg.so.62 libgomp.so.1; do
    path="$(ldd "$bin" | awk -v l="$lib" '$1 == l { print $3 }')"
    [[ -n "$path" && -e "$path" ]] || { echo "can't find $lib for $bin" >&2; exit 1; }
    cp -L "$path" "$appdir/usr/lib/$lib"
done

# The launcher entry and icon, named by the app id so the window's icon
# and taskbar entry match.
entry="$appdir/usr/share/applications/$app_id.desktop"
sed -e '/^#/d' -e 's|^Exec=.*|Exec=photo-pipeline|' "assets/$app_id.desktop.in" > "$entry"
cp "$entry" "$appdir/$app_id.desktop"
cp "assets/icons/$app_id.svg" "$appdir/$app_id.svg"
for s in 16 22 32 48 64 128 256; do
    png="assets/icons/$s/$app_id.png"
    if [[ -e "$png" ]]; then
        install -Dm644 "$png" "$appdir/usr/share/icons/hicolor/${s}x${s}/apps/$app_id.png"
    fi
done
install -Dm644 "assets/icons/$app_id.svg" "$appdir/usr/share/icons/hicolor/scalable/apps/$app_id.svg"
ln -s "$app_id.svg" "$appdir/.DirIcon"

cat > "$appdir/AppRun" <<'EOF'
#!/bin/sh
# Start Photo Pipeline from inside the AppImage. The bundled libraries come
# first; the system's graphics stack is used as is.
here="$(dirname "$(readlink -f "$0")")"
export LD_LIBRARY_PATH="$here/usr/lib:$here/usr/bin${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$here/usr/bin/photo-pipeline" "$@"
EOF
chmod +x "$appdir/AppRun"

mkdir -p dist
out="dist/PhotoPipeline-$version-$commit-x86_64.AppImage"
ARCH=x86_64 "$tool" --no-appstream "$appdir" "$out"
echo "Built $out ($(du -h "$out" | cut -f1))"
