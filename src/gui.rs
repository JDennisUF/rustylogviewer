use crate::config::{AppConfig, GuiTheme};
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
const EXE_QUICK_START_LINES: &[&str] = &[
    r#"rustylogviewer.exe --gui"#,
    r#"rustylogviewer.exe --gui --config ".\configs\team.toml""#,
    r#"rustylogviewer.exe --headless --config ".\configs\team.toml""#,
    r#"rustylogviewer.exe --print-config-only --config ".\configs\team.toml""#,
    r#"rustylogviewer.exe "C:\logs\app.log" "C:\logs\worker.log""#,
];
const EXE_OPTION_LINES: &[&str] = &[
    "--gui                      Open graphical desktop window",
    "--headless                 Print matching events to stdout (no GUI/TUI)",
    "--config <PATH>            Load TOML config file",
    "--print-config-only        Validate config, print effective config, and exit",
    "--poll-ms <N>              Override poll interval in milliseconds",
    "--max-buffer-lines <N>     Override in-memory retained line limit",
    "--max-line-len <N>         Override per-line truncation limit",
    "--show-timestamps          Force timestamps on output",
    "--no-timestamps            Disable timestamps on output",
    "--case-insensitive-filter  Text filter matches case-insensitively",
    "--case-sensitive-filter    Text filter matches case-sensitively",
    "--blacklist-regex <REGEX>  Suppress matching lines (repeatable)",
    "--whitelist-regex <REGEX>  Force-keep matching lines (repeatable)",
    "<FILE>...                  Log files to watch (overrides config tracked_files)",
];
const WINDOWS_SHORTCUT_LINES: &[&str] = &[
    r#"1. Right-click desktop, choose New > Shortcut"#,
    r#"2. Target example: "C:\tools\rustylogviewer.exe" --gui --config "C:\tools\configs\team.toml""#,
    r#"3. Set Start in to the folder containing the exe/configs"#,
    r#"4. Create one shortcut per config so users can launch the right view quickly"#,
];
const CARGO_QUICK_START_LINES: &[&str] = &[
    "cargo run -- --gui",
    "cargo run -- --gui --config ./rustylogviewer.toml",
    "cargo run -- --headless --config ./rustylogviewer.toml",
    "cargo run -- --print-config-only --config ./rustylogviewer.toml",
    "cargo run -- ./app.log ./worker.log",
];

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
    active_blacklist_regex: Vec<String>,
    active_whitelist_regex: Vec<String>,
    last_poll_at: Instant,
    text_filter: String,
    source_filter: Option<String>,
    config_panel_visible: bool,
    last_applied_theme: Option<GuiTheme>,
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
        let mut recent_configs =
            load_recent_configs(state_file_path.as_deref()).unwrap_or_default();
        let discovered_configs = discover_startup_configs();
        merge_discovered_configs(&mut recent_configs, discovered_configs);
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
            active_blacklist_regex: Vec::new(),
            active_whitelist_regex: Vec::new(),
            last_poll_at: Instant::now(),
            text_filter: String::new(),
            source_filter: None,
            config_panel_visible: true,
            last_applied_theme: None,
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

        while self.events.len() > self.config.max_buffer_lines {
            self.events.pop_front();
            self.dropped += 1;
        }
        self.last_poll_at = Instant::now();
        self.watcher = Some(watcher);
        self.rules = Some(rules);
        self.active_blacklist_regex = self.config.blacklist_regex.clone();
        self.active_whitelist_regex = self.config.whitelist_regex.clone();
        self.running = true;
        self.status_message = "Stream started (existing lines preserved)".to_string();
    }

    fn stop_stream(&mut self) {
        self.running = false;
        self.watcher = None;
        self.rules = None;
        self.active_blacklist_regex.clear();
        self.active_whitelist_regex.clear();
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
        let should_apply = self.last_applied_theme != Some(self.config.gui_theme)
            || self
                .last_applied_font_size
                .is_none_or(|prev| (prev - base).abs() > f32::EPSILON);
        if !should_apply {
            return;
        }

        ctx.set_visuals(visuals_for_theme(self.config.gui_theme));

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
        self.last_applied_theme = Some(self.config.gui_theme);
        self.last_applied_font_size = Some(base);
    }

    fn maybe_reload_rules_while_running(&mut self) {
        if !self.running {
            return;
        }
        if self.config.blacklist_regex == self.active_blacklist_regex
            && self.config.whitelist_regex == self.active_whitelist_regex
        {
            return;
        }

        match LineRules::new(&self.config.blacklist_regex, &self.config.whitelist_regex) {
            Ok(rules) => {
                self.rules = Some(rules);
                self.active_blacklist_regex = self.config.blacklist_regex.clone();
                self.active_whitelist_regex = self.config.whitelist_regex.clone();
                self.status_message = "Applied updated regex rules".to_string();
            }
            Err(err) => {
                self.status_message = format!("Regex update failed: {}", err);
            }
        }
    }

    fn clear_displayed_logs(&mut self) {
        self.events.clear();
        self.total_seen = 0;
        self.dropped = 0;
        self.suppressed_by_rules = 0;
        self.status_message = "Cleared displayed log output".to_string();
    }

    fn visible_log_copy_payload(&self) -> (usize, String) {
        let lower_text_filter =
            if self.config.case_insensitive_text_filter && !self.text_filter.is_empty() {
                Some(self.text_filter.to_lowercase())
            } else {
                None
            };

        let mut lines = String::new();
        let mut count = 0usize;
        for event in &self.events {
            if !self.display_matches_filters(event, &lower_text_filter) {
                continue;
            }
            let line = if self.config.show_timestamps {
                &event.with_ts
            } else {
                &event.without_ts
            };
            if count > 0 {
                lines.push('\n');
            }
            lines.push_str(line);
            count += 1;
        }
        (count, lines)
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

fn merge_discovered_configs(recent_configs: &mut Vec<PathBuf>, discovered_configs: Vec<PathBuf>) {
    for path in discovered_configs {
        if recent_configs.iter().any(|existing| existing == &path) {
            continue;
        }
        recent_configs.push(path);
    }
}

fn discover_startup_configs() -> Vec<PathBuf> {
    let search_roots = default_config_search_roots();
    discover_app_configs_in_roots(&search_roots)
}

fn default_config_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            roots.push(parent.to_path_buf());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if !roots.iter().any(|existing| existing == &cwd) {
            roots.push(cwd);
        }
    }
    roots
}

fn discover_app_configs_in_roots(search_roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut discovered = Vec::new();
    for root in search_roots {
        let read_dir = match std::fs::read_dir(root) {
            Ok(read_dir) => read_dir,
            Err(_) => continue,
        };

        let mut root_configs = Vec::new();
        for entry in read_dir.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let is_toml = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
            if !is_toml {
                continue;
            }
            if AppConfig::from_file(&path).is_ok() {
                root_configs.push(path);
            }
        }

        root_configs.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
        for path in root_configs {
            if discovered.iter().any(|existing| existing == &path) {
                continue;
            }
            discovered.push(path);
        }
    }
    discovered
}

fn select_startup_config(initial: Option<PathBuf>, recent: &[PathBuf]) -> Option<PathBuf> {
    initial.or_else(|| recent.first().cloned())
}

fn visuals_for_theme(theme: GuiTheme) -> egui::Visuals {
    match theme {
        GuiTheme::DefaultDark => egui::Visuals::dark(),
        GuiTheme::Light => egui::Visuals::light(),
        GuiTheme::HighContrast => {
            let mut visuals = egui::Visuals::dark();
            visuals.override_text_color = Some(Color32::from_rgb(255, 255, 255));
            visuals.hyperlink_color = Color32::from_rgb(120, 210, 255);
            visuals.selection.bg_fill = Color32::from_rgb(255, 214, 0);
            visuals.selection.stroke.color = Color32::from_rgb(0, 0, 0);
            visuals.panel_fill = Color32::from_rgb(0, 0, 0);
            visuals.window_fill = Color32::from_rgb(8, 8, 8);
            visuals.faint_bg_color = Color32::from_rgb(18, 18, 18);
            visuals.extreme_bg_color = Color32::from_rgb(0, 0, 0);
            visuals.code_bg_color = Color32::from_rgb(14, 14, 14);
            visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(24, 24, 24);
            visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(46, 46, 46);
            visuals.widgets.active.weak_bg_fill = Color32::from_rgb(255, 214, 0);
            visuals.widgets.open.weak_bg_fill = Color32::from_rgb(255, 214, 0);
            visuals.widgets.active.fg_stroke.color = Color32::from_rgb(0, 0, 0);
            visuals.widgets.open.fg_stroke.color = Color32::from_rgb(0, 0, 0);
            visuals
        }
        GuiTheme::OceanBlue => {
            let mut visuals = egui::Visuals::dark();
            visuals.override_text_color = Some(Color32::from_rgb(220, 235, 250));
            visuals.hyperlink_color = Color32::from_rgb(102, 194, 255);
            visuals.selection.bg_fill = Color32::from_rgb(46, 95, 138);
            visuals.selection.stroke.color = Color32::from_rgb(225, 243, 255);
            visuals.panel_fill = Color32::from_rgb(18, 26, 36);
            visuals.window_fill = Color32::from_rgb(20, 31, 43);
            visuals.faint_bg_color = Color32::from_rgb(23, 36, 50);
            visuals.extreme_bg_color = Color32::from_rgb(13, 20, 30);
            visuals.code_bg_color = Color32::from_rgb(17, 29, 43);
            visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(29, 52, 74);
            visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(36, 66, 92);
            visuals.widgets.active.weak_bg_fill = Color32::from_rgb(45, 80, 112);
            visuals.widgets.open.weak_bg_fill = Color32::from_rgb(45, 80, 112);
            visuals
        }
        GuiTheme::ShadesOfPurple => {
            let mut visuals = egui::Visuals::dark();
            visuals.override_text_color = Some(Color32::from_rgb(236, 225, 255));
            visuals.hyperlink_color = Color32::from_rgb(203, 166, 255);
            visuals.selection.bg_fill = Color32::from_rgb(96, 63, 145);
            visuals.selection.stroke.color = Color32::from_rgb(242, 230, 255);
            visuals.panel_fill = Color32::from_rgb(31, 20, 51);
            visuals.window_fill = Color32::from_rgb(38, 24, 63);
            visuals.faint_bg_color = Color32::from_rgb(45, 30, 72);
            visuals.extreme_bg_color = Color32::from_rgb(24, 16, 40);
            visuals.code_bg_color = Color32::from_rgb(42, 27, 69);
            visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(68, 45, 106);
            visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(87, 58, 133);
            visuals.widgets.active.weak_bg_fill = Color32::from_rgb(102, 68, 154);
            visuals.widgets.open.weak_bg_fill = Color32::from_rgb(102, 68, 154);
            visuals
        }
        GuiTheme::Novare => {
            let mut visuals = egui::Visuals::dark();
            visuals.override_text_color = Some(Color32::from_rgb(226, 232, 247));
            visuals.hyperlink_color = Color32::from_rgb(109, 219, 212);
            visuals.selection.bg_fill = Color32::from_rgb(116, 99, 184);
            visuals.selection.stroke.color = Color32::from_rgb(236, 241, 255);
            visuals.panel_fill = Color32::from_rgb(14, 24, 38);
            visuals.window_fill = Color32::from_rgb(18, 31, 49);
            visuals.faint_bg_color = Color32::from_rgb(25, 40, 61);
            visuals.extreme_bg_color = Color32::from_rgb(10, 17, 29);
            visuals.code_bg_color = Color32::from_rgb(16, 28, 45);
            visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(44, 63, 90);
            visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(57, 81, 113);
            visuals.widgets.active.weak_bg_fill = Color32::from_rgb(83, 128, 166);
            visuals.widgets.open.weak_bg_fill = Color32::from_rgb(83, 128, 166);
            visuals
        }
        GuiTheme::NovareLight => {
            let mut visuals = egui::Visuals::light();
            visuals.override_text_color = Some(Color32::from_rgb(32, 73, 84));
            visuals.hyperlink_color = Color32::from_rgb(41, 169, 143);
            visuals.selection.bg_fill = Color32::from_rgb(120, 206, 186);
            visuals.selection.stroke.color = Color32::from_rgb(17, 50, 57);
            visuals.panel_fill = Color32::from_rgb(244, 253, 250);
            visuals.window_fill = Color32::from_rgb(250, 255, 253);
            visuals.faint_bg_color = Color32::from_rgb(231, 247, 241);
            visuals.extreme_bg_color = Color32::from_rgb(221, 240, 233);
            visuals.code_bg_color = Color32::from_rgb(233, 247, 241);
            visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(214, 237, 230);
            visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(196, 229, 219);
            visuals.widgets.active.weak_bg_fill = Color32::from_rgb(84, 199, 171);
            visuals.widgets.open.weak_bg_fill = Color32::from_rgb(84, 199, 171);
            visuals
        }
        GuiTheme::Dracula => {
            let mut visuals = egui::Visuals::dark();
            visuals.override_text_color = Some(Color32::from_rgb(248, 248, 242));
            visuals.hyperlink_color = Color32::from_rgb(139, 233, 253);
            visuals.selection.bg_fill = Color32::from_rgb(98, 114, 164);
            visuals.selection.stroke.color = Color32::from_rgb(248, 248, 242);
            visuals.panel_fill = Color32::from_rgb(40, 42, 54);
            visuals.window_fill = Color32::from_rgb(43, 46, 64);
            visuals.faint_bg_color = Color32::from_rgb(52, 55, 76);
            visuals.extreme_bg_color = Color32::from_rgb(31, 33, 43);
            visuals.code_bg_color = Color32::from_rgb(49, 52, 70);
            visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(68, 71, 90);
            visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(80, 84, 106);
            visuals.widgets.active.weak_bg_fill = Color32::from_rgb(98, 114, 164);
            visuals.widgets.open.weak_bg_fill = Color32::from_rgb(98, 114, 164);
            visuals
        }
        GuiTheme::Nord => {
            let mut visuals = egui::Visuals::dark();
            visuals.override_text_color = Some(Color32::from_rgb(216, 222, 233));
            visuals.hyperlink_color = Color32::from_rgb(136, 192, 208);
            visuals.selection.bg_fill = Color32::from_rgb(94, 129, 172);
            visuals.selection.stroke.color = Color32::from_rgb(236, 239, 244);
            visuals.panel_fill = Color32::from_rgb(46, 52, 64);
            visuals.window_fill = Color32::from_rgb(52, 60, 74);
            visuals.faint_bg_color = Color32::from_rgb(59, 66, 82);
            visuals.extreme_bg_color = Color32::from_rgb(42, 48, 60);
            visuals.code_bg_color = Color32::from_rgb(52, 58, 72);
            visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(67, 76, 94);
            visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(79, 90, 110);
            visuals.widgets.active.weak_bg_fill = Color32::from_rgb(94, 129, 172);
            visuals.widgets.open.weak_bg_fill = Color32::from_rgb(94, 129, 172);
            visuals
        }
        GuiTheme::SolarizedDark => {
            let mut visuals = egui::Visuals::dark();
            visuals.override_text_color = Some(Color32::from_rgb(131, 148, 150));
            visuals.hyperlink_color = Color32::from_rgb(38, 139, 210);
            visuals.selection.bg_fill = Color32::from_rgb(7, 54, 66);
            visuals.selection.stroke.color = Color32::from_rgb(238, 232, 213);
            visuals.panel_fill = Color32::from_rgb(0, 43, 54);
            visuals.window_fill = Color32::from_rgb(3, 50, 62);
            visuals.faint_bg_color = Color32::from_rgb(7, 54, 66);
            visuals.extreme_bg_color = Color32::from_rgb(0, 34, 44);
            visuals.code_bg_color = Color32::from_rgb(0, 49, 61);
            visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(20, 73, 83);
            visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(32, 87, 99);
            visuals.widgets.active.weak_bg_fill = Color32::from_rgb(38, 139, 210);
            visuals.widgets.open.weak_bg_fill = Color32::from_rgb(38, 139, 210);
            visuals
        }
        GuiTheme::SolarizedLight => {
            let mut visuals = egui::Visuals::light();
            visuals.override_text_color = Some(Color32::from_rgb(88, 110, 117));
            visuals.hyperlink_color = Color32::from_rgb(38, 139, 210);
            visuals.selection.bg_fill = Color32::from_rgb(238, 232, 213);
            visuals.selection.stroke.color = Color32::from_rgb(88, 110, 117);
            visuals.panel_fill = Color32::from_rgb(253, 246, 227);
            visuals.window_fill = Color32::from_rgb(250, 243, 224);
            visuals.faint_bg_color = Color32::from_rgb(247, 240, 220);
            visuals.extreme_bg_color = Color32::from_rgb(238, 232, 213);
            visuals.code_bg_color = Color32::from_rgb(238, 232, 213);
            visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(242, 236, 217);
            visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(232, 226, 207);
            visuals.widgets.active.weak_bg_fill = Color32::from_rgb(181, 137, 0);
            visuals.widgets.open.weak_bg_fill = Color32::from_rgb(181, 137, 0);
            visuals
        }
        GuiTheme::OneDark => {
            let mut visuals = egui::Visuals::dark();
            visuals.override_text_color = Some(Color32::from_rgb(171, 178, 191));
            visuals.hyperlink_color = Color32::from_rgb(97, 175, 239);
            visuals.selection.bg_fill = Color32::from_rgb(62, 68, 82);
            visuals.selection.stroke.color = Color32::from_rgb(220, 223, 228);
            visuals.panel_fill = Color32::from_rgb(40, 44, 52);
            visuals.window_fill = Color32::from_rgb(44, 49, 58);
            visuals.faint_bg_color = Color32::from_rgb(46, 52, 62);
            visuals.extreme_bg_color = Color32::from_rgb(34, 37, 44);
            visuals.code_bg_color = Color32::from_rgb(39, 43, 51);
            visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(62, 68, 82);
            visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(73, 80, 96);
            visuals.widgets.active.weak_bg_fill = Color32::from_rgb(82, 90, 108);
            visuals.widgets.open.weak_bg_fill = Color32::from_rgb(82, 90, 108);
            visuals
        }
    }
}

fn start_button_color(theme: GuiTheme) -> Color32 {
    match theme {
        GuiTheme::DefaultDark => Color32::from_rgb(38, 174, 96),
        GuiTheme::Light => Color32::from_rgb(46, 160, 67),
        GuiTheme::HighContrast => Color32::from_rgb(255, 214, 0),
        GuiTheme::OceanBlue => Color32::from_rgb(38, 132, 190),
        GuiTheme::ShadesOfPurple => Color32::from_rgb(126, 87, 194),
        GuiTheme::Novare => Color32::from_rgb(88, 214, 184),
        GuiTheme::NovareLight => Color32::from_rgb(72, 196, 165),
        GuiTheme::Dracula => Color32::from_rgb(80, 220, 141),
        GuiTheme::Nord => Color32::from_rgb(136, 192, 208),
        GuiTheme::SolarizedDark => Color32::from_rgb(42, 161, 152),
        GuiTheme::SolarizedLight => Color32::from_rgb(133, 153, 0),
        GuiTheme::OneDark => Color32::from_rgb(152, 195, 121),
    }
}

fn stop_button_color(theme: GuiTheme) -> Color32 {
    match theme {
        GuiTheme::DefaultDark => Color32::from_rgb(190, 45, 45),
        GuiTheme::Light => Color32::from_rgb(200, 55, 55),
        GuiTheme::HighContrast => Color32::from_rgb(255, 106, 106),
        GuiTheme::OceanBlue => Color32::from_rgb(181, 77, 77),
        GuiTheme::ShadesOfPurple => Color32::from_rgb(168, 86, 124),
        GuiTheme::Novare => Color32::from_rgb(167, 146, 214),
        GuiTheme::NovareLight => Color32::from_rgb(55, 150, 129),
        GuiTheme::Dracula => Color32::from_rgb(255, 85, 85),
        GuiTheme::Nord => Color32::from_rgb(191, 97, 106),
        GuiTheme::SolarizedDark => Color32::from_rgb(220, 50, 47),
        GuiTheme::SolarizedLight => Color32::from_rgb(203, 75, 22),
        GuiTheme::OneDark => Color32::from_rgb(224, 108, 117),
    }
}

fn render_startup_help(ui: &mut egui::Ui) {
    ui.heading("Command Line Quick Start");
    ui.label("Most users run the .exe directly.");

    ui.separator();
    ui.label(RichText::new("Windows .exe commands").strong());
    for line in EXE_QUICK_START_LINES {
        ui.label(RichText::new(*line).monospace());
    }

    ui.separator();
    ui.label(RichText::new("CLI options").strong());
    for line in EXE_OPTION_LINES {
        ui.label(RichText::new(*line).monospace());
    }

    ui.separator();
    ui.label(RichText::new("Windows shortcut setup").strong());
    for line in WINDOWS_SHORTCUT_LINES {
        ui.label(RichText::new(*line).monospace());
    }

    ui.separator();
    ui.label(RichText::new("Cargo commands (developers)").strong());
    for line in CARGO_QUICK_START_LINES {
        ui.label(RichText::new(*line).monospace());
    }
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

    #[test]
    fn discover_app_configs_in_roots_filters_and_orders() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a.toml");
        let b = dir.path().join("b.toml");
        let invalid = dir.path().join("not-app.toml");
        let non_toml = dir.path().join("notes.txt");

        std::fs::write(
            &b,
            r#"
tracked_files = ["/tmp/two.log"]
"#,
        )
        .expect("write b");
        std::fs::write(
            &a,
            r#"
tracked_files = ["/tmp/one.log"]
"#,
        )
        .expect("write a");
        std::fs::write(&invalid, "poll_interval_ms = 1000").expect("write invalid");
        std::fs::write(&non_toml, "tracked_files = [\"/tmp/nope.log\"]").expect("write text");

        let discovered = discover_app_configs_in_roots(&[dir.path().to_path_buf()]);
        assert_eq!(discovered, vec![a, b]);
    }

    #[test]
    fn merge_discovered_configs_keeps_mru_order_and_appends_unique() {
        let mut recent_configs = vec![
            PathBuf::from("/tmp/recent.toml"),
            PathBuf::from("/tmp/shared.toml"),
        ];
        let discovered = vec![
            PathBuf::from("/tmp/shared.toml"),
            PathBuf::from("/tmp/discovered.toml"),
        ];

        merge_discovered_configs(&mut recent_configs, discovered);

        assert_eq!(
            recent_configs,
            vec![
                PathBuf::from("/tmp/recent.toml"),
                PathBuf::from("/tmp/shared.toml"),
                PathBuf::from("/tmp/discovered.toml"),
            ]
        );
    }

    #[test]
    fn startup_help_lists_core_exe_options() {
        let options_text = EXE_OPTION_LINES.join("\n");
        let usage_text = EXE_QUICK_START_LINES.join("\n");
        let shortcut_text = WINDOWS_SHORTCUT_LINES.join("\n");

        assert!(usage_text.contains("rustylogviewer.exe --gui"));
        assert!(options_text.contains("--config <PATH>"));
        assert!(options_text.contains("--blacklist-regex <REGEX>"));
        assert!(shortcut_text.contains("New > Shortcut"));
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.maybe_apply_visual_theme(ctx);
        self.maybe_reload_rules_while_running();
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
                let start_button =
                    egui::Button::new(RichText::new("Start").strong().color(Color32::WHITE))
                        .fill(start_button_color(self.config.gui_theme));
                if ui.add_enabled(!self.running, start_button).clicked() {
                    self.start_stream();
                }

                let stop_button = if self.running {
                    egui::Button::new(RichText::new("Stop").strong().color(Color32::WHITE))
                        .fill(stop_button_color(self.config.gui_theme))
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
                    ui.checkbox(&mut self.config.gui_word_wrap, "Word Wrap");

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

                    ui.separator();
                    ui.label(RichText::new("Theme").strong());
                    let previous_theme = self.config.gui_theme;
                    egui::ComboBox::from_id_salt("gui-theme-selector")
                        .selected_text(self.config.gui_theme.display_name())
                        .show_ui(ui, |ui| {
                            for theme in GuiTheme::all() {
                                ui.selectable_value(
                                    &mut self.config.gui_theme,
                                    *theme,
                                    theme.display_name(),
                                );
                            }
                        });
                    if self.config.gui_theme != previous_theme {
                        self.status_message =
                            format!("Theme: {}", self.config.gui_theme.display_name());
                    }
                    self.config.gui_light_mode = self.config.gui_theme.is_light();
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
                ui.label(format!("dropped(buffer)={}", self.dropped));
                ui.label(format!("suppressed(regex)={}", self.suppressed_by_rules));
                ui.label(format!("retained={}", self.events.len()));
                ui.separator();
                ui.label(&self.status_message);
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let show_startup_help = !self.running && self.events.is_empty();
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
                if ui.button("Copy Visible").clicked() {
                    let (line_count, payload) = self.visible_log_copy_payload();
                    if line_count == 0 {
                        self.status_message = "No visible log lines to copy".to_string();
                    } else {
                        ui.ctx().copy_text(payload);
                        self.status_message =
                            format!("Copied {} visible log lines to clipboard", line_count);
                    }
                }
                if ui.button("Clear Logs").clicked() {
                    self.clear_displayed_logs();
                }
            });

            ui.separator();
            if show_startup_help {
                render_startup_help(ui);
                ui.separator();
            }
            let log_scroll_area = if self.config.gui_word_wrap {
                egui::ScrollArea::vertical()
            } else {
                egui::ScrollArea::both()
            };
            log_scroll_area
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
                        if self.config.gui_word_wrap {
                            ui.label(RichText::new(line).monospace());
                        } else {
                            ui.add(
                                egui::Label::new(RichText::new(line).monospace())
                                    .wrap_mode(egui::TextWrapMode::Extend),
                            );
                        }
                    }
                });
        });
    }
}
