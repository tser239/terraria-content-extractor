pub mod wav;
pub mod xnb;
pub mod xwb;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use walkdir::WalkDir;

#[derive(Clone, Debug)]
pub struct Options {
    pub extract_music: bool,
    pub extract_fonts: bool,
    pub unpremultiply: bool,
    pub skip_existing: bool,
    pub verbose: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            extract_music: true,
            extract_fonts: true,
            unpremultiply: true,
            skip_existing: true,
            verbose: false,
        }
    }
}

#[derive(Debug, Default)]
pub struct Progress {
    pub total: AtomicUsize,
    pub done: AtomicUsize,
    pub images: AtomicUsize,
    pub sounds: AtomicUsize,
    pub fonts: AtomicUsize,
    pub font_pages: AtomicUsize,
    pub music: AtomicUsize,
    pub skipped: AtomicUsize,
    pub errors: AtomicUsize,
    pub bytes_written: AtomicU64,
}

impl Progress {
    pub fn summary(&self, cancelled: bool, elapsed_secs: f64) -> Summary {
        Summary {
            images: self.images.load(Ordering::Relaxed),
            sounds: self.sounds.load(Ordering::Relaxed),
            fonts: self.fonts.load(Ordering::Relaxed),
            font_pages: self.font_pages.load(Ordering::Relaxed),
            music: self.music.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
            elapsed_secs,
            cancelled,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub images: usize,
    pub sounds: usize,
    pub fonts: usize,
    pub font_pages: usize,
    pub music: usize,
    pub skipped: usize,
    pub errors: usize,
    pub bytes_written: u64,
    pub elapsed_secs: f64,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Success,
    Warn,
    Error,
    Verbose,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub level: LogLevel,
    pub time: String,
    pub text: String,
}

pub enum WorkerMsg {
    Log(LogLine),
    Finished(Summary),
}

#[derive(Debug)]
pub enum Outcome {
    Image {
        width: u32,
        height: u32,
        bytes: u64,
    },
    Sound {
        bytes: u64,
    },
    Font {
        name: String,
        pages: usize,
        bytes: u64,
    },
    /// Deliberately skipped (already exists, feature disabled, ...).
    Skipped {
        reason: String,
    },
    /// A recognised asset that cannot be converted to a useful file type.
    Unsupported {
        reason: String,
    },
}

pub(crate) fn log(tx: &Sender<WorkerMsg>, level: LogLevel, text: impl Into<String>) {
    let _ = tx.send(WorkerMsg::Log(LogLine {
        level,
        time: chrono::Local::now().format("%H:%M:%S").to_string(),
        text: text.into(),
    }));
}

/// Accepts either the Terraria root folder or its `Content` folder.
pub fn resolve_content_dir(selected: &Path) -> Result<PathBuf> {
    if !selected.is_dir() {
        bail!("{} is not a directory", selected.display());
    }

    let content = selected.join("Content");
    if content.is_dir() && looks_like_content(&content) {
        return Ok(content);
    }
    if looks_like_content(selected) {
        return Ok(selected.to_path_buf());
    }
    bail!(
        "{} does not look like a Terraria folder (expected a Content subfolder or an Images/Sounds/Fonts folder)",
        selected.display()
    );
}

fn looks_like_content(dir: &Path) -> bool {
    dir.join("Images").is_dir()
        || dir.join("Sounds").is_dir()
        || dir.join("Fonts").is_dir()
        || dir.join("Wave Bank.xwb").is_file()
}

pub fn run_extraction(
    content: PathBuf,
    output: PathBuf,
    options: Options,
    progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
    tx: Sender<WorkerMsg>,
) {
    let started = Instant::now();

    log(
        &tx,
        LogLevel::Info,
        format!("Content directory: {}", content.display()),
    );
    log(
        &tx,
        LogLevel::Info,
        format!("Output directory:  {}", output.display()),
    );

    if let Err(e) = std::fs::create_dir_all(&output) {
        log(
            &tx,
            LogLevel::Error,
            format!("Could not create output directory: {e:#}"),
        );
        progress.errors.fetch_add(1, Ordering::Relaxed);
        let summary = progress.summary(false, started.elapsed().as_secs_f64());
        let _ = tx.send(WorkerMsg::Finished(summary));
        return;
    }

    let mut files = Vec::new();
    for entry in WalkDir::new(&content).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("xnb"))
            && let Ok(rel) = path.strip_prefix(&content)
        {
            files.push(rel.to_path_buf());
        }
    }
    files.sort();

    let wave_bank = content.join("Wave Bank.xwb");
    let music_present = options.extract_music && wave_bank.is_file();
    let total = files.len() + usize::from(music_present);
    progress.total.store(total, Ordering::Relaxed);

    log(
        &tx,
        LogLevel::Info,
        format!(
            "Found {} XNB file(s){}",
            files.len(),
            if music_present {
                " and Wave Bank.xwb"
            } else {
                ""
            }
        ),
    );

    if !options.extract_music {
        log(
            &tx,
            LogLevel::Info,
            "Music extraction is disabled; Wave Bank.xwb will not be read.",
        );
    } else if !wave_bank.is_file() {
        log(
            &tx,
            LogLevel::Warn,
            format!("No Wave Bank.xwb found at {}", wave_bank.display()),
        );
    }
    if !options.extract_fonts {
        log(
            &tx,
            LogLevel::Info,
            "Font extraction is disabled; font XNBs will be skipped.",
        );
    }

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 16);

    let process_xnbs = || {
        files.par_iter().for_each(|rel| {
            if cancel.load(Ordering::Relaxed) {
                return;
            }

            let source = content.join(rel);
            match xnb::extract(&source, rel, &output, &options) {
                Ok(Outcome::Image {
                    width,
                    height,
                    bytes,
                }) => {
                    progress.images.fetch_add(1, Ordering::Relaxed);
                    progress.bytes_written.fetch_add(bytes, Ordering::Relaxed);
                    if options.verbose {
                        log(
                            &tx,
                            LogLevel::Verbose,
                            format!("PNG  {} ({}x{})", rel.display(), width, height),
                        );
                    }
                }
                Ok(Outcome::Sound { bytes }) => {
                    progress.sounds.fetch_add(1, Ordering::Relaxed);
                    progress.bytes_written.fetch_add(bytes, Ordering::Relaxed);
                    if options.verbose {
                        log(&tx, LogLevel::Verbose, format!("WAV  {}", rel.display()));
                    }
                }
                Ok(Outcome::Font { name, pages, bytes }) => {
                    progress.fonts.fetch_add(1, Ordering::Relaxed);
                    progress.font_pages.fetch_add(pages, Ordering::Relaxed);
                    progress.bytes_written.fetch_add(bytes, Ordering::Relaxed);
                    log(
                        &tx,
                        LogLevel::Success,
                        format!("Font {name}: {pages} atlas page(s) + glyph JSON"),
                    );
                }
                Ok(Outcome::Skipped { reason }) => {
                    progress.skipped.fetch_add(1, Ordering::Relaxed);
                    if options.verbose {
                        log(
                            &tx,
                            LogLevel::Verbose,
                            format!("SKIP {} ({reason})", rel.display()),
                        );
                    }
                }
                Ok(Outcome::Unsupported { reason }) => {
                    progress.skipped.fetch_add(1, Ordering::Relaxed);
                    log(
                        &tx,
                        LogLevel::Warn,
                        format!("SKIP {} ({reason})", rel.display()),
                    );
                }
                Err(e) => {
                    progress.errors.fetch_add(1, Ordering::Relaxed);
                    log(
                        &tx,
                        LogLevel::Error,
                        format!("FAIL {}: {e:#}", rel.display()),
                    );
                }
            }

            progress.done.fetch_add(1, Ordering::Relaxed);
        });
    };

    match rayon::ThreadPoolBuilder::new().num_threads(threads).build() {
        Ok(pool) => pool.install(process_xnbs),
        Err(error) => {
            log(
                &tx,
                LogLevel::Warn,
                format!(
                    "Could not build a {threads}-thread pool ({error}); using the default pool."
                ),
            );
            process_xnbs();
        }
    }

    if music_present {
        if cancel.load(Ordering::Relaxed) {
            progress.done.fetch_add(1, Ordering::Relaxed);
        } else {
            log(
                &tx,
                LogLevel::Info,
                "Extracting music from Wave Bank.xwb...",
            );
            match xwb::extract_music(&content, &output, &options, &cancel, progress.as_ref(), &tx) {
                Ok(stats) => {
                    progress.music.store(stats.exported, Ordering::Relaxed);
                    progress.skipped.fetch_add(stats.skipped, Ordering::Relaxed);
                    progress.errors.fetch_add(stats.errors, Ordering::Relaxed);
                    progress
                        .bytes_written
                        .fetch_add(stats.bytes_written, Ordering::Relaxed);
                    log(
                        &tx,
                        LogLevel::Success,
                        format!(
                            "Music: {} track(s) written{}",
                            stats.exported,
                            if stats.skipped > 0 {
                                format!(", {} skipped", stats.skipped)
                            } else {
                                String::new()
                            }
                        ),
                    );
                }
                Err(e) => {
                    progress.errors.fetch_add(1, Ordering::Relaxed);
                    log(
                        &tx,
                        LogLevel::Error,
                        format!("Music extraction failed: {e:#}"),
                    );
                }
            }
            progress.done.fetch_add(1, Ordering::Relaxed);
        }
    }

    let cancelled = cancel.load(Ordering::Relaxed);
    let summary = progress.summary(cancelled, started.elapsed().as_secs_f64());
    log(
        &tx,
        if summary.errors > 0 || cancelled {
            LogLevel::Warn
        } else {
            LogLevel::Success
        },
        format!(
            "{}: {} image(s), {} sound(s), {} font(s) / {} page(s), {} music track(s), {} skipped, {} error(s) in {}",
            if cancelled { "Cancelled" } else { "Finished" },
            summary.images,
            summary.sounds,
            summary.fonts,
            summary.font_pages,
            summary.music,
            summary.skipped,
            summary.errors,
            format_duration(summary.elapsed_secs),
        ),
    );

    let _ = tx.send(WorkerMsg::Finished(summary));
}

pub fn format_duration(secs: f64) -> String {
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        let m = (secs / 60.0).floor() as u64;
        let s = secs - m as f64 * 60.0;
        format!("{m}m {s:.0}s")
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn format_number(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Resolve a relative output path, replacing the extension.
pub(crate) fn with_extension(rel: &Path, ext: &str) -> PathBuf {
    let mut p = rel.to_path_buf();
    p.set_extension(ext);
    p
}

/// Checked helper for reading a whole file with a useful error message.
pub(crate) fn read_file(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("reading {}", path.display()))
}
