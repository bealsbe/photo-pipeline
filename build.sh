#!/usr/bin/env bash
# Build the Rust Photo Pipeline (optimised release binary).
#
#   ./build.sh          build target/release/photo-pipeline
#   ./build.sh --test   build, then run the unit tests
#
# The first build compiles the vendored LibRaw and llama.cpp (the fursuit
# close-call judge, with Vulkan) and takes several minutes; later builds
# only recompile what changed.
set -euo pipefail

cd "$(dirname "$(readlink -f "$0")")"

run_tests=0
for arg in "$@"; do
    case "$arg" in
        --test) run_tests=1 ;;
        -h|--help) sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "Unknown option: $arg (try --help)" >&2; exit 2 ;;
    esac
done

# rustup installs to ~/.cargo/bin, which a non-login shell may not have.
[[ -d "$HOME/.cargo/bin" ]] && PATH="$HOME/.cargo/bin:$PATH"

# ── Prerequisites ────────────────────────────────────────────────────────
# Package names are Fedora's; other distros use similar ones.
missing=()
command -v cargo      >/dev/null || missing+=("cargo / rustc           (rust cargo, or https://rustup.rs)")
command -v gcc        >/dev/null || missing+=("C compiler              (gcc)")
command -v g++        >/dev/null || missing+=("C++ compiler            (gcc-c++)")
command -v pkg-config >/dev/null || missing+=("pkg-config              (pkgconf-pkg-config)")
if command -v pkg-config >/dev/null; then
    pkg-config --exists zlib    || missing+=("zlib headers            (zlib-ng-compat-devel)")
    pkg-config --exists libjpeg || missing+=("libjpeg headers         (libjpeg-turbo-devel)")
fi
command -v cmake      >/dev/null || missing+=("CMake                   (cmake)")
command -v glslc      >/dev/null || missing+=("GLSL compiler           (glslc)")
command -v git        >/dev/null || missing+=("git                     (git)")
if command -v pkg-config >/dev/null; then
    pkg-config --exists vulkan || missing+=("Vulkan headers          (vulkan-headers, vulkan-loader-devel)")
fi
if command -v gcc >/dev/null && [[ "$(gcc -print-file-name=libgomp.so)" == "libgomp.so" ]]; then
    missing+=("OpenMP runtime          (libgomp)")
fi
if ((${#missing[@]})); then
    echo "Cannot build — missing:" >&2
    printf '  • %s\n' "${missing[@]}" >&2
    exit 1
fi

# Edition 2024 needs Rust 1.85 or newer.
rust_ver="$(rustc --version | awk '{print $2}')"
if [[ "$(printf '%s\n1.85.0\n' "$rust_ver" | sort -V | head -n1)" != "1.85.0" ]]; then
    echo "Rust $rust_ver is too old — 1.85 or newer is required." >&2
    exit 1
fi

# ── Vendored llama.cpp ───────────────────────────────────────────────────
# Submodules pinned to the commits the judge was measured on (build.rs
# refuses any other); fetched once, shallow.
for sub in vendor/llama.cpp vendor/SPIRV-Headers; do
    if [[ ! -f "$sub/CMakeLists.txt" ]]; then
        echo "Fetching $sub (pinned)…"
        git submodule update --init --depth 1 "$sub"
    fi
done

# ── Build ────────────────────────────────────────────────────────────────
echo "Building Photo Pipeline (release)…"
start=$SECONDS
cargo build --release
echo "Built target/release/photo-pipeline in $((SECONDS - start)) s"

# ── The binary must start on its own ─────────────────────────────────────
# ONNX Runtime's WebGPU provider lives in libwebgpu_dawn.so beside the
# binary, found through its runpath (build.rs).  Check it the way the
# desktop entry will meet it: no LD_LIBRARY_PATH, no cargo environment.
bin=target/release/photo-pipeline
if missing="$(env -u LD_LIBRARY_PATH ldd "$bin" | grep 'not found')"; then
    echo "The binary can't find its shared libraries:" >&2
    echo "$missing" >&2
    exit 1
fi
# The helper is the same binary with a flag; without models it must still
# load and answer with a plain "failed" line (it then exits 1), not crash.
reply="$(env -u LD_LIBRARY_PATH "$bin" --ml-worker --device cpu --models /nonexistent </dev/null 2>/dev/null || true)"
if [[ $reply != *'"reply":"failed"'* ]]; then
    echo "The search helper (photo-pipeline --ml-worker) doesn't start: ${reply:-no reply}" >&2
    exit 1
fi
# The judge helper likewise: without its model it says so and exits.
reply="$(env -u LD_LIBRARY_PATH "$bin" --judge-worker --model-dir /nonexistent </dev/null 2>/dev/null || true)"
if [[ $reply != *'"reply":"failed"'* ]]; then
    echo "The judge helper (photo-pipeline --judge-worker) doesn't start: ${reply:-no reply}" >&2
    exit 1
fi

if ((run_tests)); then
    echo "Running unit tests…"
    cargo test --release
fi
