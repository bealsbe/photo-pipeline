#!/usr/bin/env bash
# Check the tracked files for things that belong to one machine rather than
# to the project: private network addresses, home-folder and /mnt paths
# other than the placeholders the tests use, agent scratch paths, e-mail
# addresses, ID numbers in the test data, screenshots or photos outside
# assets/, and local notes (*.local.md).
#
#   scripts/check-private.sh            check the tracked files as they are
#   scripts/check-private.sh --cached   check what is staged instead
#
# Linked as .git/hooks/pre-commit it checks what is staged.  Prints
# file:line for every hit and exits 1; exits 0 when there are none.
set -euo pipefail

# This file holds patterns only, never a value it looks for.  vendor/ is
# skipped: it is third-party code as published, its authors' addresses
# included.

# Placeholders that tests and docs may use (README, "Privacy check").
home_ok='me|u|user'
mnt_ok='nas|nasish|photos|usb|x|my|library|share'

cached=0
[[ "$(basename "$0")" == pre-commit ]] && cached=1
for arg in "$@"; do
    case "$arg" in
        --cached) cached=1 ;;
        -h|--help) sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "Unknown option: $arg (try --help)" >&2; exit 2 ;;
    esac
done

cd "$(git rev-parse --show-toplevel)"

found="$(mktemp)"
trap 'rm -f "$found"' EXIT

# Record each "file:line:match" line read from stdin as a hit of this kind.
report() {
    local kind=$1 file lno match
    while IFS=: read -r file lno match; do
        printf '%s:%s: %s: %s\n' "$file" "$lno" "$kind" "$match" >> "$found"
    done
}

# Tracked (or, with --cached, staged) paths matching the pathspecs.
tracked() {
    if ((cached)); then git ls-files --cached -- "$@"; else git ls-files -- "$@"; fi
}

# A tracked file's bytes: the staged copy with --cached.
content() {
    if ((cached)); then git show ":$1"; else cat -- "$1"; fi
}

# "file:line:match" for an extended regex over the tracked text files,
# then over the bytes of the test data (binary files, hence grep -a).
matches() {
    local pattern=$1 f
    local opts=(-nIoE)
    ((cached)) && opts+=(--cached)
    git grep "${opts[@]}" -e "$pattern" -- . ':!vendor' ':!testdata' || true
    while IFS= read -r f; do
        content "$f" | { grep -a -n -o -E -e "$pattern" || true; } | sed "s|^|$f:|"
    done < <(tracked testdata)
}

# Drop lines matching an extended regex (no error when none are left).
except() {
    grep -vE "$1" || true
}

# Private (RFC 1918) IPv4 addresses.  Docs use 192.0.2.x (RFC 5737).
matches '(^|[^0-9.])(10\.[0-9]{1,3}|172\.(1[6-9]|2[0-9]|3[01])|192\.168)\.[0-9]{1,3}\.[0-9]{1,3}' |
    sed -E 's/:[^0-9:]([0-9.]+)$/:\1/' | report 'private IP address'

# Home folders and /mnt paths other than the placeholders.
matches '/home/[A-Za-z0-9._-]+' | except ":/home/($home_ok)\$" | report 'home folder'
matches '/mnt/[A-Za-z0-9._-]+' | except ":/mnt/($mnt_ok)\$" | report 'mount point'

# Agent scratch folders (the brackets keep this line from matching itself).
matches '/tmp/claude[-][A-Za-z0-9._/-]*' | report 'scratch path'

# E-mail addresses, other than example.* domains and no-reply addresses.
matches '[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}' |
    except ':[^:]*(@example\.|noreply)[^:]*$' | report 'e-mail address'

# 128-bit IDs in the test data (XMP DocumentID / InstanceID, EXIF
# ImageUniqueID) that aren't all zeros: each names one real file.
while IFS= read -r f; do
    content "$f" |
        { grep -a -n -o -E '[0-9A-Fa-f]{8}-?([0-9A-Fa-f]{4}-?){3}[0-9A-Fa-f]{12}' || true; } |
        { grep -E ':.*[1-9A-Fa-f]' || true; } |
        sed "s|^|$f:|"
done < <(tracked testdata) | report 'file ID in test data'

# Screenshots and photos belong in assets/ (the app's icons) or nowhere.
tracked ':(icase)*.png' ':(icase)*.jpg' ':(icase)*.jpeg' ':(icase)*.heic' ':(icase)*.webp' \
        ':(icase)*.tif' ':(icase)*.tiff' ':(icase)*.arw' ':(icase)*.cr2' ':(icase)*.cr3' \
        ':(icase)*.nef' ':(icase)*.dng' ':(icase)*.raf' ':(icase)*.orf' ':(icase)*.rw2' |
    except '^assets/' | sed 's|$|:1:outside assets/|' | report 'image'

# Local notes name this machine's paths; .gitignore keeps them out.
tracked '*.local.md' | sed 's|$|:1:should stay untracked|' | report 'local notes'

if [[ -s "$found" ]]; then
    cat "$found"
    echo "check-private: $(wc -l < "$found") hit(s).  Use placeholders instead (README, \"Privacy check\")." >&2
    exit 1
fi
echo "check-private: no machine-specific values found."
