#!/usr/bin/env bash
# Add Photo Pipeline to the desktop: a launcher entry and the app icon, so
# the taskbar, title bar and Alt+Tab show it instead of the generic Wayland
# logo.  Installs for this user only (~/.local/share); nothing needs root.
#
#   ./install.sh              install (or refresh) the entry and the icon
#   ./install.sh --uninstall  remove them again
#
# The entry starts run.sh from this folder, so rebuilds are picked up.
set -euo pipefail

here="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
# Must match APP_ID in src/app/mod.rs: KWin and Plasma find the entry, and
# through it the icon, by the window's app id.
app_id=io.github.bealsbe.PhotoPipeline
data="${XDG_DATA_HOME:-$HOME/.local/share}"
apps="$data/applications"
icons="$data/icons/hicolor"
sizes=(16 22 32 48 64 128 256)

uninstall=0
for arg in "$@"; do
    case "$arg" in
        --uninstall) uninstall=1 ;;
        -h|--help) sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "Unknown option: $arg (try --help)" >&2; exit 2 ;;
    esac
done

refresh() {
    # KWin reads the entry directly, but Plasma's taskbar and launcher go
    # through the service cache: rebuild it now rather than waiting for kded
    # to notice.  The other caches are refreshed only if they already exist;
    # creating one that other installers don't keep up to date would hide
    # their icons later.
    if [[ -f "$apps/mimeinfo.cache" ]] && command -v update-desktop-database >/dev/null; then
        update-desktop-database -q "$apps" || true
    fi
    if [[ -f "$icons/icon-theme.cache" ]] && command -v gtk-update-icon-cache >/dev/null; then
        gtk-update-icon-cache -q -f -t "$icons" || true
    fi
    if command -v kbuildsycoca6 >/dev/null; then
        kbuildsycoca6 >/dev/null 2>&1 || true
    fi
}

if ((uninstall)); then
    rm -f "$apps/$app_id.desktop" "$icons/scalable/apps/$app_id.svg"
    for s in "${sizes[@]}"; do rm -f "$icons/${s}x${s}/apps/$app_id.png"; done
    refresh
    echo "Removed the Photo Pipeline launcher and icon."
    exit 0
fi

# Exec takes one double-quoted argument.  Inside it ", `, $ and \ are
# escaped with a backslash, and that backslash is escaped again because the
# value is itself a string.
exec_arg() {
    local s=$1 out="" c i
    for ((i = 0; i < ${#s}; i++)); do
        c=${s:i:1}
        case $c in
            '\') out+='\\\\' ;;
            '"' | '`' | '$') out+='\\'"$c" ;;
            *) out+=$c ;;
        esac
    done
    printf '"%s"' "$out"
}

# The spec's %% for a literal % is not honoured by KDE's or GLib's launchers.
if [[ $here == *[%$'\n']* ]]; then
    echo "Can't add a launcher for a folder whose path has a % or a line break in it." >&2
    echo "Move this folder somewhere else and run install.sh again." >&2
    exit 1
fi
exec_line="Exec=$(exec_arg "$here/run.sh")"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
entry="$tmp/$app_id.desktop"
{
    echo "# Added by $here/install.sh; ./install.sh --uninstall removes it."
    started=0
    while IFS= read -r line; do
        # The template's own header comments are for this repository.
        if ((!started)); then
            [[ $line == \#* ]] && continue
            started=1
        fi
        [[ $line == "Exec=@EXEC@" ]] && line=$exec_line
        printf '%s\n' "$line"
    done < "$here/assets/$app_id.desktop.in"
} > "$entry"
if command -v desktop-file-validate >/dev/null; then
    desktop-file-validate "$entry"
fi

install -Dm644 "$here/assets/icons/$app_id.svg" "$icons/scalable/apps/$app_id.svg"
for s in "${sizes[@]}"; do
    install -Dm644 "$here/assets/icons/$s/$app_id.png" "$icons/${s}x${s}/apps/$app_id.png"
done
install -Dm644 "$entry" "$apps/$app_id.desktop"

refresh
echo "Installed the Photo Pipeline launcher: $apps/$app_id.desktop"
if [[ ! -x "$here/target/release/photo-pipeline" ]]; then
    echo "The app isn't built yet: run ./build.sh before starting it from the menu."
fi
echo "If the app is open, restart it to pick up the icon."
