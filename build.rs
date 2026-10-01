//! Builds the vendored LibRaw 0.22 (the version rawpy ships, so RAW support
//! matches the Python app) plus the small C shim in native/libraw_shim.cpp.
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
