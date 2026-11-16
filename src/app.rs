//! egui user interface.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;
use std::time::Instant;

use eframe::egui;

use crate::detect;
use crate::extract::{
    self, LogLevel, LogLine, Options, Progress, Summary, WorkerMsg, format_bytes, format_duration,
    format_number,
};

enum Status {
    Idle,
    Running,
    Finished(Summary),
}

pub struct ExtractorApp {
    input: String,
    output: String,
    options: Options,
    status: Status,
    progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
    receiver: Option<Receiver<WorkerMsg>>,
    worker: Option<JoinHandle<()>>,
    log: Vec<LogLine>,
    auto_scroll: bool,
    started_at: Option<Instant>,
    error: Option<String>,
}

impl ExtractorApp {
    pub fn new(context: &eframe::CreationContext<'_>) -> Self {
        context.egui_ctx.set_visuals(egui::Visuals::dark());
        context.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        });

        Self {
            input: default_input(),
            output: String::new(),
            options: Options::default(),
            status: Status::Idle,
            progress: Arc::new(Progress::default()),
            cancel: Arc::new(AtomicBool::new(false)),
            receiver: None,
            worker: None,
            log: Vec::new(),
            auto_scroll: true,
            started_at: None,
            error: None,
        }
    }

    fn start(&mut self) {
        self.error = None;

        let input = PathBuf::from(self.input.trim());
        let content = match extract::resolve_content_dir(&input) {
            Ok(content) => content,
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                return;
            }
        };

        let output = self.output.trim();
        if output.is_empty() {
            self.error = Some("Choose an output folder first.".into());
            return;
        }
        let output = PathBuf::from(output);

        self.log.clear();
        self.progress = Arc::new(Progress::default());
        self.cancel = Arc::new(AtomicBool::new(false));

        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.status = Status::Running;
        self.started_at = Some(Instant::now());

        let options = self.options.clone();
        let progress = Arc::clone(&self.progress);
        let cancel = Arc::clone(&self.cancel);

        self.worker = Some(std::thread::spawn(move || {
            extract::run_extraction(content, output, options, progress, cancel, sender);
        }));
    }

    fn request_cancel(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    fn poll_worker(&mut self) {
        let Some(receiver) = &self.receiver else {
            return;
        };

        let mut messages = Vec::new();
        loop {
            match receiver.try_recv() {
                Ok(message) => messages.push(message),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }

        let mut finished = None;
        for message in messages {
            match message {
                WorkerMsg::Log(line) => self.log.push(line),
                WorkerMsg::Finished(summary) => finished = Some(summary),
            }
        }

        if self.log.len() > 200_000 {
            let excess = self.log.len() - 200_000;
            self.log.drain(..excess);
        }

        if let Some(summary) = finished {
            self.status = Status::Finished(summary);
            self.receiver = None;
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn is_running(&self) -> bool {
        matches!(self.status, Status::Running)
    }

    fn copy_log(&self, context: &egui::Context) {
        let mut text = String::new();
        for line in &self.log {
            text.push_str(&format!("[{}] {}\n", line.time, line.text));
        }
        context.copy_text(text);
    }

    fn paths_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Terraria folder").strong().size(14.0));
        ui.horizontal(|ui| {
            // Lay out from the right: the button gets its space first, then the
            // text field fills whatever is left. Without this the text field
            // would consume the whole row and hide the button.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Browse…")
                    .on_hover_text("Choose the Terraria folder or its Content folder")
                    .clicked()
                    && let Some(folder) = pick_folder(&self.input)
                {
                    self.input = folder;
                }
                if ui
                    .button("Detect")
                    .on_hover_text("Search Steam and GOG folders for an installed Terraria")
                    .clicked()
                {
                    match detect::find_terraria() {
                        Some(folder) => {
                            self.input = folder.display().to_string();
                            self.error = None;
                        }
                        None => {
                            self.error = Some(
                                "No Terraria installation was found automatically; use Browse… to pick it."
                                    .into(),
                            );
                        }
                    }
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.input)
                        .desired_width(f32::INFINITY)
                        .hint_text("/path/to/Terraria"),
                );
            });
        });

        if self.input.trim().is_empty() {
            ui.label(
                egui::RichText::new(
                    "Terraria was not found automatically. Click Detect to search again, or Browse… to select the folder.",
                )
                .small()
                .color(egui::Color32::from_rgb(230, 190, 90)),
            );
        } else {
            match extract::resolve_content_dir(Path::new(self.input.trim())) {
                Ok(content) => {
                    ui.label(
                        egui::RichText::new(format!("Content: {}", content.display()))
                            .small()
                            .weak(),
                    );
                }
                Err(error) => {
                    ui.label(
                        egui::RichText::new(format!("Warning: {error}"))
                            .small()
                            .color(egui::Color32::from_rgb(230, 190, 90)),
                    );
                }
            }
        }

        ui.add_space(4.0);

        ui.label(egui::RichText::new("Output folder").strong().size(14.0));
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Browse…")
                    .on_hover_text("Choose where the extracted files should be written")
                    .clicked()
                    && let Some(folder) = pick_folder(&self.output)
                {
                    self.output = folder;
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.output)
                        .desired_width(f32::INFINITY)
                        .hint_text("/path/to/output"),
                );
            });
        });

        if self.output.trim().is_empty() {
            ui.label(
                egui::RichText::new(
                    "Choose a folder for the extracted files. Click Browse… to select one.",
                )
                .small()
                .weak(),
            );
        }
    }

    fn options_ui(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.label(egui::RichText::new("Options").strong());
            ui.add_space(2.0);
            ui.checkbox(
                &mut self.options.extract_music,
                "Extract music from Wave Bank.xwb",
            );
            ui.checkbox(
                &mut self.options.extract_fonts,
                "Extract font atlases + glyph JSON",
            );
            ui.checkbox(
                &mut self.options.unpremultiply,
                "Un-premultiply alpha (straight-alpha PNGs)",
            );
            ui.checkbox(
                &mut self.options.skip_existing,
                "Skip files that already exist (resume-friendly)",
            );
            ui.checkbox(
                &mut self.options.verbose,
                "Verbose logging (one line per file)",
            );
        });
    }

    fn controls_ui(&mut self, ui: &mut egui::Ui) {
        let can_start =
            !self.is_running() && !self.input.trim().is_empty() && !self.output.trim().is_empty();

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    can_start,
                    egui::Button::new(egui::RichText::new("Start extraction").strong()),
                )
                .clicked()
            {
                self.start();
            }

            if self.is_running() && ui.button("Cancel").clicked() {
                self.request_cancel();
            }

            if let Some(error) = &self.error {
                ui.label(egui::RichText::new(error).color(egui::Color32::from_rgb(235, 100, 100)));
            }
        });
    }

    fn progress_ui(&mut self, ui: &mut egui::Ui) {
        let total = self.progress.total.load(Ordering::Relaxed);
        let done = self.progress.done.load(Ordering::Relaxed);
        let fraction = if total > 0 {
            (done as f32 / total as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let text = if total > 0 {
            format!("{} / {} items", format_number(done), format_number(total))
        } else if self.is_running() {
            "Scanning…".to_string()
        } else {
            "Ready".to_string()
        };

        ui.add(
            egui::ProgressBar::new(fraction)
                .text(text)
                .animate(self.is_running()),
        );

        let elapsed = self
            .started_at
            .map(|started| started.elapsed().as_secs_f64())
            .unwrap_or(0.0);

        let images = self.progress.images.load(Ordering::Relaxed);
        let sounds = self.progress.sounds.load(Ordering::Relaxed);
        let fonts = self.progress.fonts.load(Ordering::Relaxed);
        let pages = self.progress.font_pages.load(Ordering::Relaxed);
        let music = self.progress.music.load(Ordering::Relaxed);
        let skipped = self.progress.skipped.load(Ordering::Relaxed);
        let errors = self.progress.errors.load(Ordering::Relaxed);
        let bytes = self.progress.bytes_written.load(Ordering::Relaxed);

        ui.horizontal_wrapped(|ui| {
            stat(ui, "Images", &format_number(images));
            stat(ui, "Sounds", &format_number(sounds));
            stat(ui, "Fonts", &format_number(fonts));
            stat(ui, "Pages", &format_number(pages));
            stat(ui, "Music", &format_number(music));
            stat(ui, "Skipped", &format_number(skipped));
            stat(ui, "Errors", &format_number(errors));
            stat(ui, "Written", &format_bytes(bytes));
            if self.is_running() {
                stat(ui, "Elapsed", &format_duration(elapsed));
            }
        });

        if let Status::Finished(summary) = &self.status {
            let color = if summary.errors > 0 {
                egui::Color32::from_rgb(230, 190, 90)
            } else {
                egui::Color32::from_rgb(110, 200, 120)
            };
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(format!(
                    "{} in {} - {} image(s), {} sound(s), {} font(s), {} music track(s), {} skipped, {} error(s), {} written",
                    if summary.cancelled { "Cancelled" } else { "Finished" },
                    format_duration(summary.elapsed_secs),
                    format_number(summary.images),
                    format_number(summary.sounds),
                    format_number(summary.fonts),
                    format_number(summary.music),
                    format_number(summary.skipped),
                    format_number(summary.errors),
                    format_bytes(summary.bytes_written),
                ))
                .color(color),
            );
        }
    }

    fn log_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("log_panel")
            .resizable(true)
            .default_size(300.0)
            .min_size(120.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Log").strong().size(14.0));
                    ui.label(
                        egui::RichText::new(format!("{} line(s)", format_number(self.log.len())))
                            .small()
                            .weak(),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Clear").clicked() {
                            self.log.clear();
                        }
                        if ui.button("Copy").clicked() {
                            self.copy_log(ui.ctx());
                        }
                        ui.checkbox(&mut self.auto_scroll, "Auto-scroll");
                    });
                });
                ui.separator();

                let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .stick_to_bottom(self.auto_scroll)
                    .show_rows(ui, row_height, self.log.len(), |ui, range| {
                        for index in range {
                            let line = &self.log[index];
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!("{}  {}", line.time, line.text))
                                        .monospace()
                                        .color(level_color(line.level)),
                                )
                                .truncate(),
                            );
                        }
                    });
            });
    }
}

impl eframe::App for ExtractorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_worker();

        if self.is_running() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }

        egui::Panel::top("header").show(ui, |ui| {
            ui.add_space(4.0);
            ui.heading("Terraria Content Extractor");
            ui.label(
                egui::RichText::new(
                    "Convert Terraria's XNB images, sounds and fonts, plus the music wave bank, into PNG, WAV and JSON.",
                )
                .weak(),
            );
            ui.add_space(4.0);
        });

        self.log_panel(ui);

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.paths_ui(ui);
                    ui.add_space(6.0);
                    self.options_ui(ui);
                    ui.add_space(6.0);
                    self.controls_ui(ui);
                    ui.add_space(6.0);
                    self.progress_ui(ui);
                });
        });
    }
}

fn stat(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(egui::RichText::new(format!("{label}:")).small().weak());
    ui.label(egui::RichText::new(value).small().strong());
    ui.add_space(6.0);
}

fn level_color(level: LogLevel) -> egui::Color32 {
    match level {
        LogLevel::Info => egui::Color32::from_rgb(175, 185, 195),
        LogLevel::Success => egui::Color32::from_rgb(115, 200, 125),
        LogLevel::Warn => egui::Color32::from_rgb(230, 190, 90),
        LogLevel::Error => egui::Color32::from_rgb(235, 100, 100),
        LogLevel::Verbose => egui::Color32::from_rgb(120, 132, 148),
    }
}

fn pick_folder(current: &str) -> Option<String> {
    let mut dialog = rfd::FileDialog::new().set_title("Select folder");
    let current = current.trim();
    if !current.is_empty() && Path::new(current).is_dir() {
        dialog = dialog.set_directory(current);
    }
    dialog.pick_folder().map(|path| path.display().to_string())
}

fn default_input() -> String {
    detect::find_terraria()
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}
