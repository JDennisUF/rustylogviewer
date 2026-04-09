use crate::config::AppConfig;
use crate::formatting::format_event_line;
use crate::line_rules::LineRules;
use crate::watcher::{LogEvent, PollingWatcher};
use anyhow::{Result, anyhow};
use eframe::egui::{self, Color32, FontId, RichText, TextEdit, TextStyle};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DEFAULT_WINDOW_WIDTH: f32 = 1400.0;
const DEFAULT_WINDOW_HEIGHT: f32 = 900.0;
const MAX_RECENT_CONFIGS: usize = 10;

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
    recent_configs: Vec<PathBuf>,
    state_file_path: Option<PathBuf>,
    status_message: String,
    events: VecDeque<DisplayEvent>,
    total_seen: u64,
    dropped: u64,
    suppressed_by_rules: u64,
    running: bool,
    watcher: Option<PollingWatcher>,
    rules: Option<LineRules>,
    last_poll_at: Instant,
    text_filter: String,
    source_filter: Option<String>,
    config_panel_visible: bool,
    last_applied_light_mode: Option<bool>,
    last_applied_font_size: Option<f32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct GuiStateFile {
    recent_configs: Vec<PathBuf>,
}

struct DisplayEvent {
    source: String,
    line: String,
    line_lower: String,
    with_ts: String,
    without_ts: String,
}

impl DisplayEvent {
    fn from_log(event: LogEvent) -> Self {
        let with_ts = format_event_line(&event, true);
        let without_ts = format_event_line(&event, false);
        let line_lower = event.line.to_lowercase();
        Self {
            source: event.source,
            line: event.line,
            line_lower,
            with_ts,
            without_ts,
        }
    }
}

impl GuiApp {
    fn new(initial_config_path: Option<PathBuf>) -> Self {
        let state_file_path = gui_state_file_path();
        let recent_configs = load_recent_configs(state_file_path.as_deref()).unwrap_or_default();
        let mut app = Self {
            config: AppConfig::default(),
            config_path: None,
            recent_configs,
            state_file_path,
            status_message: "Ready".to_string(),
            events: VecDeque::new(),
            total_seen: 0,
            dropped: 0,
            suppressed_by_rules: 0,
            running: false,
            watcher: None,
            rules: None,
            last_poll_at: Instant::now(),
            text_filter: String::new(),
            source_filter: None,
            config_panel_visible: true,
            last_applied_light_mode: None,
            last_applied_font_size: None,
        };

        if let Some(path) = select_startup_config(initial_config_path, &app.recent_configs) {
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
                self.push_recent_config(path.clone());
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
                self.push_recent_config(path.to_path_buf());
                self.status_message = format!("Saved {}", path.display());
            }
            Err(err) => {
                self.status_message = format!("Failed to save {}: {}", path.display(), err);
            }
        }
    }

    fn push_recent_config(&mut self, path: PathBuf) {
        self.recent_configs.retain(|existing| existing != &path);
        self.recent_configs.insert(0, path);
        if self.recent_configs.len() > MAX_RECENT_CONFIGS {
            self.recent_configs.truncate(MAX_RECENT_CONFIGS);
        }
        self.persist_recent_configs();
    }

    fn remove_recent_config_at(&mut self, index: usize) {
        if index < self.recent_configs.len() {
            self.recent_configs.remove(index);
            self.persist_recent_configs();
        }
    }

    fn clear_recent_configs(&mut self) {
        self.recent_configs.clear();
        self.persist_recent_configs();
    }

    fn persist_recent_configs(&self) {
        let Some(path) = self.state_file_path.as_ref() else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let payload = GuiStateFile {
            recent_configs: self.recent_configs.clone(),
        };
        if let Ok(toml) = toml::to_string_pretty(&payload) {
            let _ = std::fs::write(path, toml);
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
        self.status_message = "Stream started".to_string();
    }

    fn stop_stream(&mut self) {
        self.running = false;
        self.watcher = None;
        self.rules = None;
    }

    fn poll_if_due(&mut self) -> bool {
        if !self.running {
            return false;
        }
        let interval = Duration::from_millis(self.config.poll_interval_ms);
        if self.last_poll_at.elapsed() < interval {
            return false;
        }
        self.last_poll_at = Instant::now();

        let Some(watcher) = self.watcher.as_mut() else {
            return false;
        };
        let events = match watcher.poll() {
            Ok(events) => events,
            Err(err) => {
                self.status_message = format!("Watcher error: {}", err);
                self.stop_stream();
                return true;
            }
        };
        let Some(rules) = self.rules.as_ref() else {
            return false;
        };
        let (events, suppressed) = rules.partition_events(events);
        let mut changed = false;
        if suppressed > 0 {
            self.suppressed_by_rules += suppressed as u64;
            changed = true;
        }
        for event in events {
            self.total_seen += 1;
            self.events.push_back(DisplayEvent::from_log(event));
            changed = true;
            while self.events.len() > self.config.max_buffer_lines {
                self.events.pop_front();
                self.dropped += 1;
                changed = true;
            }
        }
        changed
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
        event: &DisplayEvent,
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
            event.line_lower.contains(needle)
        } else {
            event.line.contains(&self.text_filter)
        }
    }

    fn maybe_apply_visual_theme(&mut self, ctx: &egui::Context) {
        let base = self.config.gui_font_size.clamp(8.0, 40.0);
        let should_apply = self.last_applied_light_mode != Some(self.config.gui_light_mode)
            || self
                .last_applied_font_size
                .is_none_or(|prev| (prev - base).abs() > f32::EPSILON);
        if !should_apply {
            return;
        }

        if self.config.gui_light_mode {
            ctx.set_visuals(egui::Visuals::light());
        } else {
            ctx.set_visuals(egui::Visuals::dark());
        }

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
        self.last_applied_light_mode = Some(self.config.gui_light_mode);
        self.last_applied_font_size = Some(base);
    }
}

fn gui_state_file_path() -> Option<PathBuf> {
    let base = dirs::config_dir()?;
    Some(base.join("rustylogviewer").join("gui_state.toml"))
}

fn load_recent_configs(path: Option<&Path>) -> Result<Vec<PathBuf>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)?;
    let state: GuiStateFile = toml::from_str(&raw)?;
    let mut deduped = Vec::new();
    for entry in state.recent_configs {
        if entry.as_os_str().is_empty() {
            continue;
        }
        if deduped.iter().any(|existing| existing == &entry) {
            continue;
        }
        deduped.push(entry);
        if deduped.len() >= MAX_RECENT_CONFIGS {
            break;
        }
    }
    Ok(deduped)
}

fn select_startup_config(initial: Option<PathBuf>, recent: &[PathBuf]) -> Option<PathBuf> {
    initial.or_else(|| recent.first().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_recent_configs_missing_file_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("missing.toml");
        let recents = load_recent_configs(Some(&path)).expect("load recents");
        assert!(recents.is_empty());
    }

    #[test]
    fn load_recent_configs_dedupes_and_limits_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");
        let mut paths = Vec::new();
        for i in 0..(MAX_RECENT_CONFIGS + 5) {
            paths.push(PathBuf::from(format!("/tmp/config-{}.toml", i)));
        }
        paths.insert(3, PathBuf::from("/tmp/config-0.toml"));
        paths.insert(5, PathBuf::new());
        let content = toml::to_string(&GuiStateFile {
            recent_configs: paths,
        })
        .expect("serialize");
        std::fs::write(&path, content).expect("write state");

        let recents = load_recent_configs(Some(&path)).expect("load recents");
        assert_eq!(recents.first(), Some(&PathBuf::from("/tmp/config-0.toml")));
        assert!(recents.len() <= MAX_RECENT_CONFIGS);
        assert_eq!(
            recents
                .iter()
                .filter(|p| **p == PathBuf::from("/tmp/config-0.toml"))
                .count(),
            1
        );
        assert!(!recents.iter().any(|p| p.as_os_str().is_empty()));
    }

    #[test]
    fn select_startup_config_prefers_cli_then_mru() {
        let cli_path = PathBuf::from("/tmp/cli.toml");
        let mru = vec![PathBuf::from("/tmp/recent.toml")];

        assert_eq!(
            select_startup_config(Some(cli_path.clone()), &mru),
            Some(cli_path)
        );
        assert_eq!(
            select_startup_config(None, &mru),
            Some(PathBuf::from("/tmp/recent.toml"))
        );
        assert_eq!(select_startup_config(None, &[]), None);
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.maybe_apply_visual_theme(ctx);
        let polled_changed = self.poll_if_due();
        if self.running {
            let interval = Duration::from_millis(self.config.poll_interval_ms);
            let wait = interval.saturating_sub(self.last_poll_at.elapsed());
            ctx.request_repaint_after(wait);
        }
        if polled_changed {
            ctx.request_repaint();
        }
        let mut open_recent_from_menu: Option<PathBuf> = None;
        let mut clear_recent_from_menu = false;
        let recent_snapshot = self.recent_configs.clone();

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Open Config").clicked() {
                    self.open_config_picker();
                }
                ui.menu_button("Recent Configs", |ui| {
                    if recent_snapshot.is_empty() {
                        ui.label("No recent configs");
                        return;
                    }
                    for path in &recent_snapshot {
                        let label = path.display().to_string();
                        if ui.button(label).clicked() {
                            open_recent_from_menu = Some(path.clone());
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button("Clear Recent").clicked() {
                        clear_recent_from_menu = true;
                        ui.close();
                    }
                });
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
                let start_button = egui::Button::new(
                    RichText::new("Start").strong().color(Color32::WHITE),
                )
                .fill(if self.config.gui_light_mode {
                    Color32::from_rgb(46, 160, 67)
                } else {
                    Color32::from_rgb(38, 174, 96)
                });
                if ui.add_enabled(!self.running, start_button).clicked() {
                    self.start_stream();
                }

                let stop_button = if self.running {
                    egui::Button::new(RichText::new("Stop").strong().color(Color32::WHITE)).fill(
                        if self.config.gui_light_mode {
                            Color32::from_rgb(200, 55, 55)
                        } else {
                            Color32::from_rgb(190, 45, 45)
                        },
                    )
                } else {
                    egui::Button::new("Stop")
                };
                if ui.add_enabled(self.running, stop_button).clicked() {
                    self.stop_stream();
                    self.status_message = "Stream stopped".to_string();
                }
            });
        });

        if let Some(path) = open_recent_from_menu {
            self.open_config(path);
        }
        if clear_recent_from_menu {
            self.clear_recent_configs();
            self.status_message = "Cleared recent config list".to_string();
        }

        if self.config_panel_visible {
            let mut open_recent_from_panel: Option<PathBuf> = None;
            let mut remove_recent_idx: Option<usize> = None;
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

                    ui.colored_label(
                        Color32::from_rgb(150, 150, 150),
                        "Validation runs on Start/Save",
                    );

                    ui.separator();
                    ui.label(RichText::new("Recent Configs").strong());
                    if recent_snapshot.is_empty() {
                        ui.label("No recent configs");
                    } else {
                        for (idx, path) in recent_snapshot.iter().enumerate() {
                            ui.horizontal(|ui| {
                                if ui
                                    .button(path.file_name().map_or_else(
                                        || path.display().to_string(),
                                        |name| name.to_string_lossy().into_owned(),
                                    ))
                                    .clicked()
                                {
                                    open_recent_from_panel = Some(path.clone());
                                }
                                if ui.small_button("X").clicked() {
                                    remove_recent_idx = Some(idx);
                                }
                            });
                            ui.label(
                                RichText::new(path.display().to_string())
                                    .small()
                                    .color(Color32::GRAY),
                            );
                        }
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

            if let Some(path) = open_recent_from_panel {
                self.open_config(path);
            }
            if let Some(idx) = remove_recent_idx {
                self.remove_recent_config_at(idx);
            }
        }

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            let mode = if self.running { "live" } else { "stopped" };
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
                        let line = if self.config.show_timestamps {
                            &event.with_ts
                        } else {
                            &event.without_ts
                        };
                        ui.label(RichText::new(line).monospace());
                    }
                });
        });
    }
}
