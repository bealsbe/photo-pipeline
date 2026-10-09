//! Builds the vendored LibRaw 0.22 (the version rawpy ships, so RAW support
//! matches the Python app) plus the small C shim in native/libraw_shim.cpp,
//! and the vendored llama.cpp with libmtmd (Vulkan, pinned commits) plus
//! native/judge_shim.cpp for the fursuit close-call judge.
//!
//! Optional codecs follow rawpy's build: zlib (deflate DNG) and libjpeg
//! (lossy DNG) are enabled when pkg-config can find them, and OpenMP is used
//! for demosaicing when the compiler supports it.
//!
//! libjpeg(-turbo) itself is required: native/jpeg_shim.c decodes every JPEG
//! the app shows, as Qt did for the Python app.

use std::path::Path;

const LIBRAW_SOURCES: &[&str] = &[
    "src/libraw_c_api.cpp",
    "src/libraw_datastream.cpp",
    "src/decoders/canon_600.cpp",
    "src/decoders/crx.cpp",
    "src/decoders/pana8.cpp",
    "src/decoders/decoders_dcraw.cpp",
    "src/decoders/sonycc.cpp",
    "src/decompressors/losslessjpeg.cpp",
    "src/decoders/decoders_libraw_dcrdefs.cpp",
    "src/decoders/olympus14.cpp",
    "src/decoders/decoders_libraw.cpp",
    "src/decoders/dng.cpp",
    "src/decoders/fp_dng.cpp",
    "src/decoders/fuji_compressed.cpp",
    "src/decoders/generic.cpp",
    "src/decoders/kodak_decoders.cpp",
    "src/decoders/load_mfbacks.cpp",
    "src/decoders/smal.cpp",
    "src/decoders/unpack_thumb.cpp",
    "src/decoders/unpack.cpp",
    "src/demosaic/aahd_demosaic.cpp",
    "src/demosaic/ahd_demosaic.cpp",
    "src/demosaic/dcb_demosaic.cpp",
    "src/demosaic/dht_demosaic.cpp",
    "src/demosaic/misc_demosaic.cpp",
    "src/demosaic/xtrans_demosaic.cpp",
    "src/integration/dngsdk_glue.cpp",
    "src/integration/rawspeed_glue.cpp",
    "src/metadata/adobepano.cpp",
    "src/metadata/canon.cpp",
    "src/metadata/ciff.cpp",
    "src/metadata/cr3_parser.cpp",
    "src/metadata/epson.cpp",
    "src/metadata/exif_gps.cpp",
    "src/metadata/fuji.cpp",
    "src/metadata/identify_tools.cpp",
    "src/metadata/identify.cpp",
    "src/metadata/kodak.cpp",
    "src/metadata/leica.cpp",
    "src/metadata/makernotes.cpp",
    "src/metadata/mediumformat.cpp",
    "src/metadata/minolta.cpp",
    "src/metadata/misc_parsers.cpp",
    "src/metadata/nikon.cpp",
    "src/metadata/normalize_model.cpp",
    "src/metadata/olympus.cpp",
    "src/metadata/hasselblad_model.cpp",
    "src/metadata/p1.cpp",
    "src/metadata/pentax.cpp",
    "src/metadata/samsung.cpp",
    "src/metadata/sony.cpp",
    "src/metadata/tiff.cpp",
    "src/postprocessing/aspect_ratio.cpp",
    "src/postprocessing/dcraw_process.cpp",
    "src/postprocessing/mem_image.cpp",
    "src/postprocessing/postprocessing_aux.cpp",
    "src/postprocessing/postprocessing_utils_dcrdefs.cpp",
    "src/postprocessing/postprocessing_utils.cpp",
    "src/preprocessing/ext_preprocess.cpp",
    "src/preprocessing/raw2image.cpp",
    "src/preprocessing/subtract_black.cpp",
    "src/tables/cameralist.cpp",
    "src/tables/colorconst.cpp",
    "src/tables/colordata.cpp",
    "src/tables/wblists.cpp",
    "src/utils/curves.cpp",
    "src/utils/decoder_info.cpp",
    "src/utils/init_close_utils.cpp",
    "src/utils/open.cpp",
    "src/utils/phaseone_processing.cpp",
    "src/utils/read_utils.cpp",
    "src/utils/thumb_utils.cpp",
    "src/utils/utils_dcraw.cpp",
    "src/utils/utils_libraw.cpp",
    "src/write/apply_profile.cpp",
    "src/write/file_write.cpp",
    "src/write/tiff_writer.cpp",
    "src/x3f/x3f_parse_process.cpp",
    "src/x3f/x3f_utils_patched.cpp",
];

fn main() {
    let root = Path::new("vendor/LibRaw");
    println!("cargo:rerun-if-changed=native/libraw_shim.cpp");
    println!("cargo:rerun-if-changed=native/jpeg_shim.c");
    println!("cargo:rerun-if-changed=build.rs");
    // ONNX Runtime's WebGPU provider needs libwebgpu_dawn.so, which `ort`'s
    // copy-dylibs puts beside the binary (target/<profile>/).  A runpath of
    // the binary's own folder finds it however the app is launched (run.sh,
    // the desktop entry, the --ml-worker helper); `$ORIGIN/..` serves test
    // binaries, which live one level down in deps/.  The `$` reaches the
    // linker as is: no shell is involved.
    #[cfg(target_os = "linux")]
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN:$ORIGIN/..");

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .include(root)
        // LibRaw is unusably slow unoptimised, even in debug builds.
        .opt_level(3)
        .debug(false)
        .warnings(false)
        .extra_warnings(false)
        .flag_if_supported("-w")
        .flag_if_supported("-pthread");

    let zlib = pkg_config::Config::new().cargo_metadata(false).probe("zlib");
    if let Ok(lib) = &zlib {
        build.define("USE_ZLIB", None);
        for p in &lib.include_paths {
            build.include(p);
        }
    }
    let jpeg = pkg_config::Config::new().cargo_metadata(false).probe("libjpeg");
    let jpeg = match jpeg {
        Ok(lib) => lib,
        Err(e) => panic!("libjpeg (libjpeg-turbo) development files are required: {e}"),
    };
    build.define("USE_JPEG", None);
    for p in &jpeg.include_paths {
        build.include(p);
    }

    let mut jpeg_shim = cc::Build::new();
    jpeg_shim.file("native/jpeg_shim.c").opt_level(3).debug(false).warnings(false);
    for p in &jpeg.include_paths {
        jpeg_shim.include(p);
    }
    jpeg_shim.compile("jpegshim");
    let jpeg = Ok::<_, pkg_config::Error>(jpeg);

    let openmp = build.is_flag_supported("-fopenmp").unwrap_or(false)
        && !build.get_compiler().is_like_msvc();
    if openmp {
        build.flag("-fopenmp");
    }

    for src in LIBRAW_SOURCES {
        build.file(root.join(src));
    }
    build.file("native/libraw_shim.cpp");
    build.compile("rawshim");

    // The close-call judge's runtime (fursuit matching v2).
    build_llama();

    // Static archives must precede the libraries they depend on.
    for lib in [&zlib, &jpeg].into_iter().flatten() {
        for p in &lib.link_paths {
            println!("cargo:rustc-link-search=native={}", p.display());
        }
        for l in &lib.libs {
            println!("cargo:rustc-link-lib={l}");
        }
    }

    if openmp {
        println!("cargo:rustc-link-lib=gomp");
        // libgomp's dev symlink lives in the compiler's own library dir.
        if let Ok(out) = std::process::Command::new(
            build.get_compiler().path(),
        )
        .arg("-print-file-name=libgomp.so")
        .output()
        {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if let Some(dir) = Path::new(&p).parent() {
                if dir.is_dir() {
                    println!("cargo:rustc-link-search=native={}", dir.display());
                }
            }
        }
    }
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("apple") {
        println!("cargo:rustc-link-lib=c++");
    } else if !target.contains("msvc") {
        println!("cargo:rustc-link-lib=stdc++");
    }
}

// ---- the close-call judge: llama.cpp and libmtmd (fursuit matching v2) ----

/// The llama.cpp commit the judge is built from (`vendor/llama.cpp`, a
/// submodule): the one the research measured Gemma 4 on.
const LLAMA_COMMIT: &str = "de7fa0a3c6a2e1b4cd9f22eb8d6bf5b12dbdb63b";
/// SPIRV-Headers, which llama.cpp's Vulkan backend needs at build time
/// (`vendor/SPIRV-Headers`, a submodule; headers only).
const SPIRV_HEADERS_COMMIT: &str = "86f980c731e62ae4eaf383d320449d71687936bf";

/// Fail the build when a submodule isn't at its pinned commit (a git
/// checkout; a source tree without git metadata is taken as it is).
fn check_commit(dir: &Path, commit: &str) {
    let Ok(out) = std::process::Command::new("git").arg("-C").arg(dir).args(["rev-parse", "HEAD"]).output() else { return };
    if !out.status.success() {
        return;
    }
    let head = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if head != commit {
        panic!("{} is at {head}, not the pinned {commit}: run `git submodule update --init --depth 1 {}`", dir.display(), dir.display());
    }
}

fn run(cmd: &mut std::process::Command) {
    let status = cmd.status().unwrap_or_else(|e| panic!("couldn't run {cmd:?}: {e}"));
    if !status.success() {
        panic!("{cmd:?} failed ({status})");
    }
}

/// Build llama.cpp (Vulkan) and libmtmd as static archives in OUT_DIR,
/// reproducibly from the pinned commits (no -march=native, nothing
/// downloaded), then the shim the judge helper calls, and link them.
fn build_llama() {
    let llama = Path::new("vendor/llama.cpp");
    let spirv = Path::new("vendor/SPIRV-Headers");
    for dir in [llama, spirv] {
        if !dir.join("CMakeLists.txt").is_file() {
            panic!("{} is missing: run `git submodule update --init --depth 1 {}` (./build.sh does)", dir.display(), dir.display());
        }
    }
    check_commit(llama, LLAMA_COMMIT);
    check_commit(spirv, SPIRV_HEADERS_COMMIT);
    println!("cargo:rerun-if-changed=native/judge_shim.cpp");
    println!("cargo:rerun-if-changed=vendor/llama.cpp/CMakeLists.txt");
    println!("cargo:rerun-if-changed=vendor/llama.cpp/include/llama.h");
    println!("cargo:rerun-if-changed=vendor/llama.cpp/tools/mtmd/mtmd.h");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let jobs = std::env::var("NUM_JOBS").unwrap_or_else(|_| "8".into());
    let generator = if std::process::Command::new("ninja").arg("--version").output().is_ok_and(|o| o.status.success()) { "Ninja" } else { "Unix Makefiles" };
    let spirv_prefix = out.join("spirv");
    run(std::process::Command::new("cmake")
        .arg("-S").arg(spirv)
        .arg("-B").arg(out.join("spirv-build"))
        .arg("-G").arg(generator)
        .arg(format!("-DCMAKE_INSTALL_PREFIX={}", spirv_prefix.display()))
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg("-DSPIRV_HEADERS_ENABLE_TESTS=OFF")
        .arg("-DSPIRV_HEADERS_ENABLE_INSTALL=ON")
        .stdout(std::process::Stdio::null()));
    run(std::process::Command::new("cmake").arg("--install").arg(out.join("spirv-build")).stdout(std::process::Stdio::null()));
    let build = out.join("llama-build");
    let mut cfg = std::process::Command::new("cmake");
    cfg.arg("-S").arg(llama).arg("-B").arg(&build).arg("-G").arg(generator);
    for d in [
        "CMAKE_BUILD_TYPE=Release",
        "BUILD_SHARED_LIBS=OFF",
        "CMAKE_POSITION_INDEPENDENT_CODE=ON",
        "GGML_VULKAN=ON",
        "GGML_NATIVE=OFF",
        "GGML_OPENMP=OFF",
        "GGML_CCACHE=OFF",
        "LLAMA_BUILD_MTMD=ON",
        "LLAMA_BUILD_COMMON=OFF",
        "LLAMA_BUILD_TESTS=OFF",
        "LLAMA_BUILD_TOOLS=OFF",
        "LLAMA_BUILD_EXAMPLES=OFF",
        "LLAMA_BUILD_SERVER=OFF",
        "LLAMA_BUILD_APP=OFF",
        "LLAMA_OPENSSL=OFF",
        "LLAMA_CURL=OFF",
        "LLAMA_SUBPROCESS=OFF",
        "MTMD_VIDEO=OFF",
    ] {
        cfg.arg(format!("-D{d}"));
    }
    cfg.arg(format!("-DCMAKE_PREFIX_PATH={}", spirv_prefix.display()));
    // ggml-vulkan finds the package but includes <spirv/unified1/...>
    // without its target: the headers' folder goes on the include path.
    let inc = format!("-I{}", spirv_prefix.join("include").display());
    cfg.arg(format!("-DCMAKE_CXX_FLAGS={inc}")).arg(format!("-DCMAKE_C_FLAGS={inc}"));
    cfg.stdout(std::process::Stdio::null());
    run(&mut cfg);
    run(std::process::Command::new("cmake").arg("--build").arg(&build).args(["--config", "Release", "-j", &jobs, "--target", "mtmd", "llama"]).stdout(std::process::Stdio::null()));

    // The shim, against the headers of the same tree.
    let mut shim = cc::Build::new();
    shim.cpp(true)
        .std("c++17")
        .file("native/judge_shim.cpp")
        .include(llama.join("include"))
        .include(llama.join("ggml/include"))
        .include(llama.join("tools/mtmd"))
        .opt_level(2)
        .debug(false)
        .warnings(false);
    shim.compile("judgeshim");

    // The archives, the ones that use others first.
    let mut archives: Vec<std::path::PathBuf> = Vec::new();
    fn walk(dir: &Path, found: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, found);
            } else if p.extension().is_some_and(|x| x == "a") {
                found.push(p);
            }
        }
    }
    walk(&build, &mut archives);
    let order = ["mtmd", "llama", "ggml", "ggml-vulkan", "ggml-cpu", "ggml-base"];
    let name_of = |p: &Path| p.file_stem().and_then(|s| s.to_str()).and_then(|s| s.strip_prefix("lib")).map(str::to_string);
    let mut names: Vec<(usize, String, std::path::PathBuf)> = archives
        .iter()
        .filter_map(|p| {
            let n = name_of(p)?;
            // vendor helpers (hashing, image decoding) go between mtmd and llama.
            let rank = order.iter().position(|o| *o == n).map(|i| if i == 0 { 0 } else { i + 1 }).unwrap_or(1);
            Some((rank, n, p.parent()?.to_path_buf()))
        })
        .collect();
    names.sort();
    for (_, n, dir) in &names {
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=static={n}");
    }
    for needed in order {
        if !names.iter().any(|(_, n, _)| n == needed) {
            panic!("llama.cpp's build made no lib{needed}.a (found: {:?})", names.iter().map(|(_, n, _)| n).collect::<Vec<_>>());
        }
    }
    println!("cargo:rustc-link-lib=vulkan");
}
