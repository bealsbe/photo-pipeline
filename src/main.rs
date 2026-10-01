//! Photo Pipeline — keyboard-driven RAW+JPG culling and library organisation.

mod analysis;
mod app;
#[cfg(test)]
mod bench;
#[cfg(test)]
mod quality_eval;
mod bus;
mod exif;
mod exif_panel;
mod imaging;
mod model;
mod ops;
mod profile;
mod scan;
mod settings;
mod ui;
mod util;

use app::App;

fn main() -> iced::Result {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 3 && args[1] == "--dump-scan" {
        dump_scan(std::path::Path::new(&args[2]));
        return Ok(());
    }
    profile::init();
    #[cfg(feature = "profiler")]
    let sampler = profile::sampler();
    {
        let _t = profile::span("startup", "fonts");
        configure_fonts();
    }
    let result = iced::daemon(App::boot, App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .antialiasing(true)
        .run();
    profile::report();
    #[cfg(feature = "profiler")]
    sampler.finish();
    result
}

/// The bundled typeface becomes the default sans-serif; monospace comes
/// from the desktop (fc-match).  Characters Barlow lacks fall back to the
/// system's fonts as usual.
fn configure_fonts() {
    const FONTS: [&[u8]; 4] = [
        include_bytes!("../assets/fonts/BarlowPipeline-Regular.ttf"),
        include_bytes!("../assets/fonts/BarlowPipeline-Medium.ttf"),
        include_bytes!("../assets/fonts/BarlowPipeline-SemiBold.ttf"),
        include_bytes!("../assets/fonts/BarlowPipeline-Bold.ttf"),
    ];
    let query = |pattern: &str| -> Option<String> {
        let out = std::process::Command::new("fc-match").args([pattern, "-f", "%{family[0]}"]).output().ok()?;
        let name = String::from_utf8(out.stdout).ok()?.trim().to_string();
        (!name.is_empty()).then_some(name)
    };
    let mono = query("monospace").unwrap_or_else(|| "Noto Sans Mono".into());
    if let Ok(mut fs) = iced::advanced::graphics::text::font_system().write() {
        let db = fs.raw().db_mut();
        for data in FONTS {
            db.load_font_data(data.to_vec());
        }
        db.set_sans_serif_family(ui::theme::FAMILY);
        db.set_monospace_family(mono);
    }
}

/// Parity check against the Python app: one line per scanned file with the
/// capture time, disk-cache key and embedded-preview size.
fn dump_scan(root: &std::path::Path) {
    let (bus, rx) = bus::channel();
    let _h = scan::start(1, root.to_path_buf(), bus, false);
    let mut stream = Box::pin(bus::stream(&rx));
    let dc = imaging::thumbs::DiskCache::new(300);
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().expect("runtime");
    rt.block_on(async {
        use iced::futures::StreamExt;
        let mut lines = Vec::new();
        'outer: while let Some(batch) = stream.next().await {
            for ev in batch {
                match ev {
                    bus::BgEvent::ScanBatch { records, .. } => {
                        for r in records.iter() {
                            let ct = r.capture_time.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or("None".into());
                            let prev = exif::jpeg_preview_bytes(&r.path, 300).map(|(d, _)| d.len()).unwrap_or(0);
                            lines.push(format!(
                                "{}\t{}\t{}\t{}",
                                r.path.display(),
                                ct,
                                dc.digest(&r.path, r.modified.timestamp, r.file_size),
                                prev
                            ));
                        }
                    }
                    bus::BgEvent::ScanComplete { .. } | bus::BgEvent::ScanError { .. } => break 'outer,
                    _ => {}
                }
            }
        }
        lines.sort();
        for l in lines {
            println!("{l}");
        }
    });
}
