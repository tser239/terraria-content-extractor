mod app;
mod detect;
mod extract;

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, mpsc};

use extract::{Options, Progress, WorkerMsg};

const APP_ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

/// Matches the `.desktop` file name, so Wayland compositors and taskbars can
/// associate the window with its launcher entry.
const APP_ID: &str = "terraria-content-extractor";

/// Largest icon edge passed to the windowing system.
///
/// A 256x256 RGBA image is slightly larger than the maximum X11 request size,
/// so winit's `_NET_WM_ICON` update would fail silently and no icon would show.
/// 192 fits comfortably and is still plenty for the title bar and taskbar.
const MAX_ICON_EDGE: u32 = 192;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.first().map(String::as_str) == Some("--cli") {
        std::process::exit(run_cli(&args[1..]));
    }
    if matches!(args.first().map(String::as_str), Some("-V" | "--version")) {
        print_version();
        return Ok(());
    }
    if matches!(args.first().map(String::as_str), Some("-h" | "--help")) {
        print_help();
        return Ok(());
    }

    detach_console();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 760.0])
            .with_min_inner_size([760.0, 560.0])
            .with_title("Terraria Content Extractor")
            .with_app_id(APP_ID)
            .with_icon(app_icon()),
        event_loop_builder: force_x11_event_loop(),
        ..Default::default()
    };

    eframe::run_native(
        "Terraria Content Extractor",
        options,
        Box::new(|context| Ok(Box::new(app::ExtractorApp::new(context)))),
    )
}

fn app_icon() -> eframe::egui::IconData {
    let source = eframe::icon_data::from_png_bytes(APP_ICON_PNG).unwrap_or_else(|error| {
        eprintln!("warning: could not decode the embedded icon: {error}");
        eframe::egui::IconData::default()
    });
    shrink_icon(source)
}

/// Detaches from the console window that Windows allocates for console-subsystem
/// binaries.
///
/// Rust targets the console subsystem by default, which keeps `--cli` output and
/// exit codes behaving like a normal command-line tool. GUI launches do not need
/// that console, so it is closed right before the window is created. Debug
/// builds keep it so logs and panics stay visible.
fn detach_console() {
    #[cfg(all(windows, not(debug_assertions)))]
    // SAFETY: FreeConsole only detaches this process from its console.
    unsafe {
        windows_sys::Win32::System::Console::FreeConsole();
    }
}

/// Forces the X11 backend when running on Wayland.
///
/// Wayland compositors ignore per-window icons, and winit has no support for
/// `xdg-toplevel-icon-v1`, so a native Wayland window can only get an icon from
/// an installed `.desktop` file. To stay portable and still show the icon in the
/// title bar and taskbar, the app runs through XWayland when it is available.
/// Set `TCX_NATIVE_WAYLAND=1` to keep the native Wayland backend instead.
#[cfg(target_os = "linux")]
fn force_x11_event_loop() -> Option<eframe::EventLoopBuilderHook> {
    let on_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let xwayland_available = std::env::var_os("DISPLAY").is_some();
    let keep_wayland = std::env::var_os("TCX_NATIVE_WAYLAND").is_some();
    if !on_wayland || !xwayland_available || keep_wayland {
        return None;
    }

    Some(Box::new(|builder| {
        use winit::platform::x11::EventLoopBuilderExtX11 as _;
        builder.with_x11();
    }))
}

#[cfg(not(target_os = "linux"))]
fn force_x11_event_loop() -> Option<eframe::EventLoopBuilderHook> {
    None
}

fn shrink_icon(icon: eframe::egui::IconData) -> eframe::egui::IconData {
    let longest = icon.width.max(icon.height);
    if longest <= MAX_ICON_EDGE {
        return icon;
    }

    let (width, height, rgba) = (icon.width, icon.height, icon.rgba);
    let Some(image) = image::RgbaImage::from_raw(width, height, rgba) else {
        return eframe::egui::IconData::default();
    };
    let scale = f64::from(MAX_ICON_EDGE) / f64::from(longest);
    let width = (f64::from(image.width()) * scale).round().max(1.0) as u32;
    let height = (f64::from(image.height()) * scale).round().max(1.0) as u32;
    let scaled =
        image::imageops::resize(&image, width, height, image::imageops::FilterType::Lanczos3);

    eframe::egui::IconData {
        width,
        height,
        rgba: scaled.into_raw(),
    }
}

fn run_cli(args: &[String]) -> i32 {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut quiet = false;
    let mut options = Options {
        skip_existing: false,
        ..Options::default()
    };

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--no-music" => options.extract_music = false,
            "--no-fonts" => options.extract_fonts = false,
            "--keep-premultiplied" => options.unpremultiply = false,
            "--skip-existing" => options.skip_existing = true,
            "--verbose" => options.verbose = true,
            "--quiet" => quiet = true,
            "--jobs" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    eprintln!("--jobs requires a number");
                    return 2;
                };
                let Some(jobs) = parse_jobs(value) else {
                    eprintln!("invalid --jobs value: {value}");
                    return 2;
                };
                options.jobs = Some(jobs);
            }
            value if let Some(raw) = value.strip_prefix("--jobs=") => {
                let Some(jobs) = parse_jobs(raw) else {
                    eprintln!("invalid --jobs value: {raw}");
                    return 2;
                };
                options.jobs = Some(jobs);
            }
            "-V" | "--version" => {
                print_version();
                return 0;
            }
            "-h" | "--help" => {
                print_help();
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("unknown option: {other}");
                return 2;
            }
            value => {
                if input.is_none() {
                    input = Some(PathBuf::from(value));
                } else if output.is_none() {
                    output = Some(PathBuf::from(value));
                } else {
                    eprintln!("unexpected argument: {value}");
                    return 2;
                }
            }
        }
        index += 1;
    }

    let (Some(input), Some(output)) = (input, output) else {
        eprintln!("usage: terraria-content-extractor --cli <input> <output> [options]");
        return 2;
    };

    let content = match extract::resolve_content_dir(&input) {
        Ok(content) => content,
        Err(error) => {
            eprintln!("{error:#}");
            return 2;
        }
    };

    let (sender, receiver) = mpsc::channel();
    let progress = Arc::new(Progress::default());
    let cancel = Arc::new(AtomicBool::new(false));

    let worker = std::thread::spawn(move || {
        extract::run_extraction(content, output, options, progress, cancel, sender);
    });

    let mut summary = None;
    while let Ok(message) = receiver.recv() {
        match message {
            WorkerMsg::Log(line) => {
                if quiet
                    && matches!(
                        line.level,
                        extract::LogLevel::Info
                            | extract::LogLevel::Success
                            | extract::LogLevel::Verbose
                    )
                {
                    continue;
                }
                let level = match line.level {
                    extract::LogLevel::Info => "info",
                    extract::LogLevel::Success => "ok",
                    extract::LogLevel::Warn => "warn",
                    extract::LogLevel::Error => "error",
                    extract::LogLevel::Verbose => "trace",
                };
                println!("[{}] {:>5}  {}", line.time, level, line.text);
            }
            WorkerMsg::Finished(finished) => summary = Some(finished),
        }
    }

    if worker.join().is_err() {
        eprintln!("extraction thread panicked");
        return 1;
    }

    match summary {
        Some(summary) => {
            if summary.errors > 0 {
                eprintln!("{} file(s) failed", summary.errors);
                1
            } else {
                0
            }
        }
        None => 1,
    }
}

fn print_version() {
    println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
}

fn parse_jobs(value: &str) -> Option<usize> {
    value.parse::<usize>().ok().filter(|jobs| *jobs > 0)
}

fn print_help() {
    println!(
        "Terraria Content Extractor\n\
         \n\
         GUI:  terraria-content-extractor\n\
         CLI:  terraria-content-extractor --cli <input> <output> [options]\n\
         \n\
         <input> can be the Terraria folder or its Content folder.\n\
         \n\
         Options:\n\
         \x20 --no-music             skip Wave Bank.xwb\n\
         \x20 --no-fonts             skip font atlas extraction\n\
         \x20 --keep-premultiplied   leave alpha premultiplied\n\
         \x20 --skip-existing        leave files that already exist untouched\n\
         \x20 --verbose              log every file\n\
         \x20 --quiet                log only warnings and errors (CLI)\n\
         \x20 --jobs N               worker threads (default: available CPUs)\n\
         \x20 --version, -V          print the version and exit\n"
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn embedded_icon_decodes() {
        let icon = super::app_icon();
        assert_eq!((icon.width, icon.height), (128, 128));
        assert_eq!(
            icon.rgba.len(),
            (icon.width * icon.height * 4) as usize,
            "icon must be RGBA8"
        );
    }

    #[test]
    fn job_counts_must_be_positive() {
        assert_eq!(super::parse_jobs("1"), Some(1));
        assert_eq!(super::parse_jobs("32"), Some(32));
        assert_eq!(super::parse_jobs("0"), None);
        assert_eq!(super::parse_jobs("-1"), None);
        assert_eq!(super::parse_jobs("many"), None);
    }
}
