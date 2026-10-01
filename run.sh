#!/usr/bin/env bash
# Run the Rust Photo Pipeline.
#
#   ./run.sh            start the app (builds first if there is no binary yet)
#   ./run.sh --rebuild  rebuild (picks up source changes), then start
#
# Any other arguments are passed through to the app.
set -euo pipefail

here="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
bin="$here/target/release/photo-pipeline"

args=()
rebuild=0
for arg in "$@"; do
    case "$arg" in
        --rebuild) rebuild=1 ;;
        -h|--help) sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) args+=("$arg") ;;
    esac
done

if ((rebuild)) || [[ ! -x "$bin" ]]; then
    "$here/build.sh"
fi

exec "$bin" "${args[@]}"
