use crate::config::AppConfig;
use crate::formatting::format_event_line;
use crate::line_rules::LineRules;
use crate::watcher::{LogEvent, PollingWatcher};
use anyhow::{Result, anyhow};
use eframe::egui::{self, Color32, FontId, RichText, TextEdit, TextStyle};
use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DEFAULT_WINDOW_WIDTH: f32 = 1400.0;
const DEFAULT_WINDOW_HEIGHT: f32 = 900.0;

pub fn run_gui(initial_config_path: Option<PathBuf>) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT]),
        ..Default::default()
    };

    eframe::run_native(
        "rustylogviewer",
        options,
        Box::new(move |_cc| Ok(Box::new(GuiApp::new(initial_config_path.clone())))),
    )
    .map_err(|err| anyhow!("failed to launch GUI: {}", err))?;
    Ok(())
}

struct GuiApp {
    config: AppConfig,
    config_path: Option<PathBuf>,
    status_message: String,
    events: VecDeque<LogEvent>,
    total_seen: u64,
    dropped: u64,
    suppressed_by_rules: u64,
    running: bool,
    paused: bool,
    watcher: Option<PollingWatcher>,
    rules: Option<LineRules>,
    last_poll_at: Instant,
    text_filter: String,
    source_filter: Option<String>,
    config_panel_visible: bool,
}

impl GuiApp {
    fn new(initial_config_path: Option<PathBuf>) -> Self {
        let mut app = Self {
            config: AppConfig::default(),
            config_path: None,
            status_message: "Ready".to_string(),
            events: VecDeque::new(),
            total_seen: 0,
            dropped: 0,
            suppressed_by_rules: 0,
            running: false,
            paused: false,
            watcher: None,
            rules: None,
            last_poll_at: Instant::now(),
            text_filter: String::new(),
            source_filter: None,
            config_panel_visible: true,
        };

        if let Some(path) = initial_config_path {
            app.open_config(path);
        }
        app
    }

    fn open_config_picker(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("TOML Config", &["toml"])
            .pick_file()
        else {
            return;
        };
        self.open_config(path);
    }

    fn open_config(&mut self, path: PathBuf) {
        match AppConfig::from_file(&path) {
            Ok(config) => {
                self.stop_stream();
                self.config = config;
                self.config_path = Some(path.clone());
                self.status_message = format!("Loaded {}", path.display());
            }
            Err(err) => {
                self.status_message = format!("Failed to load {}: {}", path.display(), err);
            }
        }
    }

    fn new_config(&mut self) {
        self.stop_stream();
        self.config = AppConfig::default();
        self.config_path = None;
        self.status_message =
            "Created new config (unsaved). Add files before starting.".to_string();
    }

    fn save_config(&mut self) {
        let Some(path) = self.config_path.clone() else {
            self.save_config_as();
            return;
        };
        self.save_to_path(&path);
    }

    fn save_config_as(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("rustylogviewer.toml")
            .add_filter("TOML Config", &["toml"])
            .save_file()
        else {
            return;
        };
        self.save_to_path(&path);
        self.config_path = Some(path);
    }

    fn save_to_path(&mut self, path: &Path) {
        if let Err(err) = self.config.validate() {
            self.status_message = format!("Cannot save invalid config: {}", err);
            return;
        }
        match self.config.write_to_file(path) {
            Ok(()) => {
                self.status_message = format!("Saved {}", path.display());
            }
            Err(err) => {
                self.status_message = format!("Failed to save {}: {}", path.display(), err);
            }
        }
    }

    fn start_stream(&mut self) {
        if let Err(err) = self.config.validate() {
            self.status_message = format!("Invalid config: {}", err);
            return;
        }
        let watcher = match PollingWatcher::new(
            self.config.tracked_files.clone(),
            self.config.max_line_len,
        ) {
            Ok(w) => w,
            Err(err) => {
                self.status_message = format!("Failed to initialize watcher: {}", err);
                return;
            }
        };
        let rules = match LineRules::new(&self.config.blacklist_regex, &self.config.whitelist_regex)
        {
            Ok(rules) => rules,
            Err(err) => {
                self.status_message = format!("Failed to initialize regex rules: {}", err);
                return;
            }
        };

        self.events.clear();
        self.total_seen = 0;
        self.dropped = 0;
        self.suppressed_by_rules = 0;
        self.last_poll_at = Instant::now();
        self.watcher = Some(watcher);
        self.rules = Some(rules);
        self.running = true;
        self.paused = false;
        self.status_message = "Stream started".to_string();
    }

    fn stop_stream(&mut self) {
        self.running = false;
        self.paused = false;
        self.watcher = None;
        self.rules = None;
    }

    fn poll_if_due(&mut self) {
        if !self.running || self.paused {
            return;
        }
        let interval = Duration::from_millis(self.config.poll_interval_ms);
        if self.last_poll_at.elapsed() < interval {
            return;
        }
        self.last_poll_at = Instant::now();

        let Some(watcher) = self.watcher.as_mut() else {
            return;
        };
        let events = match watcher.poll() {
            Ok(events) => events,
            Err(err) => {
                self.status_message = format!("Watcher error: {}", err);
                self.stop_stream();
                return;
            }
        };
        let Some(rules) = self.rules.as_ref() else {
            return;
        };
        let (events, suppressed) = rules.partition_events(events);
        self.suppressed_by_rules += suppressed as u64;
        for event in events {
            self.total_seen += 1;
            self.events.push_back(event);
            while self.events.len() > self.config.max_buffer_lines {
                self.events.pop_front();
                self.dropped += 1;
            }
        }
    }

    fn available_sources(&self) -> Vec<String> {
        let mut names = BTreeSet::new();
        for path in &self.config.tracked_files {
            if let Some(name) = path.file_name() {
                names.insert(name.to_string_lossy().into_owned());
            } else {
                names.insert(path.display().to_string());
            }
        }
        for event in &self.events {
            names.insert(event.source.clone());
        }
        names.into_iter().collect()
    }

    fn display_matches_filters(
        &self,
        event: &LogEvent,
        lower_text_filter: &Option<String>,
    ) -> bool {
        if let Some(source) = &self.source_filter {
            if &event.source != source {
                return false;
            }
        }
        if self.text_filter.is_empty() {
            return true;
        }
        if let Some(needle) = lower_text_filter {
            event.line.to_lowercase().contains(needle)
        } else {
            event.line.contains(&self.text_filter)
        }
    }

    fn apply_visual_theme(&self, ctx: &egui::Context) {
        if self.config.gui_light_mode {
            ctx.set_visuals(egui::Visuals::light());
        } else {
            ctx.set_visuals(egui::Visuals::dark());
        }

        let base = self.config.gui_font_size.clamp(8.0, 40.0);
        let mut style = (*ctx.style()).clone();
        style.text_styles.insert(
            TextStyle::Small,
            FontId::proportional((base - 2.0).max(8.0)),
        );
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(base));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(base));
        style
            .text_styles
            .insert(TextStyle::Monospace, FontId::monospace(base));
        style
            .text_styles
            .insert(TextStyle::Heading, FontId::proportional(base + 4.0));
        ctx.set_style(style);
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_visual_theme(ctx);
        self.poll_if_due();
        if self.running && !self.paused {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        let validation_error = self.config.validate().err().map(|e| e.to_string());

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Open Config").clicked() {
                    self.open_config_picker();
                }
                if ui.button("New Config").clicked() {
                    self.new_config();
                }
                if ui.button("Save").clicked() {
                    self.save_config();
                }
                if ui.button("Save As").clicked() {
                    self.save_config_as();
                }
                ui.separator();
                let toggle_label = if self.config_panel_visible {
                    "Hide Configuration"
                } else {
                    "Show Configuration"
                };
                if ui.button(toggle_label).clicked() {
                    self.config_panel_visible = !self.config_panel_visible;
                }
                ui.separator();
                let can_start = validation_error.is_none();
                if ui
                    .add_enabled(!self.running && can_start, egui::Button::new("Start"))
                    .clicked()
                {
                    self.start_stream();
                }
                if ui
                    .add_enabled(self.running, egui::Button::new("Stop"))
                    .clicked()
                {
                    self.stop_stream();
                    self.status_message = "Stream stopped".to_string();
                }
                if ui
                    .add_enabled(
                        self.running,
                        egui::Button::new(if self.paused { "Resume" } else { "Pause" }),
                    )
                    .clicked()
                {
                    self.paused = !self.paused;
                }
            });
        });

        if self.config_panel_visible {
            egui::SidePanel::left("config_sidebar")
                .resizable(true)
                .min_width(340.0)
                .show(ctx, |ui| {
                    ui.heading("Configuration");
                    if let Some(path) = &self.config_path {
                        ui.label(format!("File: {}", path.display()));
                    } else {
                        ui.label("File: (unsaved)");
                    }

                    if let Some(error) = &validation_error {
                        ui.colored_label(
                            Color32::from_rgb(220, 110, 110),
                            format!("Invalid: {}", error),
                        );
                    } else {
                        ui.colored_label(Color32::from_rgb(120, 200, 120), "Config valid");
                    }

                    ui.separator();
                    ui.label(RichText::new("General").strong());
                    ui.horizontal(|ui| {
                        ui.label("Poll (ms)");
                        ui.add(
                            egui::DragValue::new(&mut self.config.poll_interval_ms)
                                .speed(10.0)
                                .range(1..=120_000),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Max buffer lines");
                        ui.add(
                            egui::DragValue::new(&mut self.config.max_buffer_lines)
                                .speed(100.0)
                                .range(1..=1_000_000),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Max line length");
                        ui.add(
                            egui::DragValue::new(&mut self.config.max_line_len)
                                .speed(10.0)
                                .range(1..=65_536),
                        );
                    });
                    ui.checkbox(&mut self.config.show_timestamps, "Show timestamps");
                    ui.checkbox(&mut self.config.gui_light_mode, "Light mode");
                    ui.horizontal(|ui| {
                        ui.label("GUI font size");
                        ui.add(
                            egui::DragValue::new(&mut self.config.gui_font_size)
                                .speed(0.25)
                                .range(8.0..=40.0),
                        );
                    });
                    ui.checkbox(
                        &mut self.config.case_insensitive_text_filter,
                        "Case-insensitive text filter",
                    );

                    ui.separator();
                    ui.label(RichText::new("Tracked Files").strong());
                    let mut remove_file_idx = None;
                    for (idx, path) in self.config.tracked_files.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            let mut value = path.display().to_string();
                            if ui
                                .add(TextEdit::singleline(&mut value).desired_width(220.0))
                                .changed()
                            {
                                *path = PathBuf::from(value);
                            }
                            if ui.small_button("X").clicked() {
                                remove_file_idx = Some(idx);
                            }
                        });
                    }
                    if let Some(idx) = remove_file_idx {
                        self.config.tracked_files.remove(idx);
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Add File").clicked() {
                            if let Some(path) = rfd::FileDialog::new().pick_file() {
                                self.config.tracked_files.push(path);
                            }
                        }
                        if ui.button("Add Empty").clicked() {
                            self.config.tracked_files.push(PathBuf::new());
                        }
                    });

                    ui.separator();
                    ui.label(RichText::new("Blacklist Regex").strong());
                    let mut remove_blacklist_idx = None;
                    for (idx, pattern) in self.config.blacklist_regex.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add(TextEdit::singleline(pattern).desired_width(220.0));
                            if ui.small_button("X").clicked() {
                                remove_blacklist_idx = Some(idx);
                            }
                        });
                    }
                    if let Some(idx) = remove_blacklist_idx {
                        self.config.blacklist_regex.remove(idx);
                    }
                    if ui.button("Add Blacklist Pattern").clicked() {
                        self.config.blacklist_regex.push(String::new());
                    }

                    ui.separator();
                    ui.label(RichText::new("Whitelist Regex").strong());
                    let mut remove_whitelist_idx = None;
                    for (idx, pattern) in self.config.whitelist_regex.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add(TextEdit::singleline(pattern).desired_width(220.0));
                            if ui.small_button("X").clicked() {
                                remove_whitelist_idx = Some(idx);
                            }
                        });
                    }
                    if let Some(idx) = remove_whitelist_idx {
                        self.config.whitelist_regex.remove(idx);
                    }
                    if ui.button("Add Whitelist Pattern").clicked() {
                        self.config.whitelist_regex.push(String::new());
                    }
                });
        }

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            let mode = if self.running {
                if self.paused { "paused" } else { "live" }
            } else {
                "stopped"
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("mode={}", mode));
                ui.label(format!("seen={}", self.total_seen));
                ui.label(format!("dropped={}", self.dropped));
                ui.label(format!("suppressed={}", self.suppressed_by_rules));
                ui.label(format!("retained={}", self.events.len()));
                ui.separator();
                ui.label(&self.status_message);
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Source");
                egui::ComboBox::from_id_salt("source-filter")
                    .selected_text(self.source_filter.as_deref().unwrap_or("All"))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.source_filter, None, "All");
                        for source in self.available_sources() {
                            ui.selectable_value(
                                &mut self.source_filter,
                                Some(source.clone()),
                                source,
                            );
                        }
                    });

                ui.label("Text");
                ui.add(TextEdit::singleline(&mut self.text_filter).desired_width(240.0));
                if ui.button("Clear").clicked() {
                    self.text_filter.clear();
                    self.source_filter = None;
                }
            });

            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    let lower_text_filter = if self.config.case_insensitive_text_filter
                        && !self.text_filter.is_empty()
                    {
                        Some(self.text_filter.to_lowercase())
                    } else {
                        None
                    };
                    for event in &self.events {
                        if !self.display_matches_filters(event, &lower_text_filter) {
                            continue;
                        }
                        let line = format_event_line(event, self.config.show_timestamps);
                        ui.label(RichText::new(line).monospace());
                    }
                });
        });
    }
}
