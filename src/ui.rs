use crate::config::{AppConfig, GuiTheme};
use crate::formatting::format_event_line;
use crate::line_rules::LineRules;
use crate::watcher::{LogEvent, PollingWatcher};
use anyhow::Result;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph};
use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub fn run_tui(mut config: AppConfig, config_path: Option<PathBuf>) -> Result<()> {
    let poll_interval = Duration::from_millis(config.poll_interval_ms);
    let mut watcher = PollingWatcher::new(config.tracked_files.clone(), config.max_line_len)?;
    let rules = LineRules::new(&config.blacklist_regex, &config.whitelist_regex)?;
    let mut state = TuiState::new(&config);
    state.set_available_sources(watcher.active_sources());

    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
    let _cleanup = TerminalCleanup;
    let mut terminal = ratatui::Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let mut next_poll = Instant::now();
    let mut dirty = true;
    loop {
        if !state.paused && Instant::now() >= next_poll {
            let events = watcher.poll()?;
            state.set_available_sources(watcher.active_sources());
            let warnings = watcher.take_status_messages();
            if !warnings.is_empty() {
                state.set_status(warnings.join(" | "));
                dirty = true;
            }
            if !events.is_empty() {
                let (events, suppressed) = rules.partition_events(events);
                state.push_events(events, suppressed, config.max_buffer_lines);
                dirty = true;
            }
            next_poll = Instant::now() + poll_interval;
        }

        if dirty {
            terminal.draw(|frame| render(frame, &state, &config))?;
            dirty = false;
        }

        let timeout = if state.paused {
            Duration::from_millis(500)
        } else {
            next_poll
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(500))
        };

        if !event::poll(timeout)? {
            continue;
        }
        match event::read()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if state.handle_key(key.code) {
                    break;
                }
            }
            Event::Paste(text) => {
                state.handle_paste(text);
            }
            _ => continue,
        }

        if let Some(raw_path) = state.take_pending_add_tracked_file() {
            match parse_cli_input_path(&raw_path) {
                Some(path) => match watcher.add_file(path.clone()) {
                    Ok(true) => {
                        if !config
                            .tracked_files
                            .iter()
                            .any(|existing| existing == &path)
                        {
                            config.tracked_files.push(path.clone());
                        }
                        state.set_available_sources(watcher.active_sources());
                        state.add_tracked_file(path);
                    }
                    Ok(false) => {
                        state.set_status(format!("Already tracking {}", path.display()));
                    }
                    Err(err) => {
                        state.set_status(format!("Failed to track {}: {}", path.display(), err));
                    }
                },
                None => {
                    state.set_status("No file path entered".to_string());
                }
            }
        }
        sync_runtime_config_from_state(&mut config, &state);
        if state.take_save_requested() {
            save_runtime_config(&config, config_path.as_deref(), &mut state);
        }
        dirty = true;
    }
    Ok(())
}

struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
    }
}

fn render(frame: &mut Frame<'_>, state: &TuiState, config: &AppConfig) {
    let palette = tui_theme_palette(state.preview_theme());
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(frame.area());

    let header = Paragraph::new(state.header_line(config))
        .style(
            Style::default()
                .fg(palette.header_fg)
                .bg(palette.background)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default());
    frame.render_widget(header, areas[0]);

    let content = if state.input_mode == InputMode::ListTrackedFiles {
        let lines = state.tracked_file_lines();
        if lines.is_empty() {
            Paragraph::new("No tracked files configured.")
                .style(Style::default().fg(palette.muted_fg).bg(palette.background))
                .block(Block::default())
        } else {
            Paragraph::new(lines)
                .style(
                    Style::default()
                        .fg(palette.content_fg)
                        .bg(palette.background),
                )
                .block(Block::default())
        }
    } else if state.input_mode == InputMode::ThemePicker {
        Paragraph::new(state.theme_picker_lines())
            .style(
                Style::default()
                    .fg(palette.content_fg)
                    .bg(palette.background),
            )
            .block(Block::default())
    } else if state.input_mode == InputMode::Help {
        Paragraph::new(state.command_help_lines())
            .style(
                Style::default()
                    .fg(palette.content_fg)
                    .bg(palette.background),
            )
            .block(Block::default())
    } else {
        let lines = state.visible_lines(config.show_timestamps, areas[1].height as usize);
        if lines.is_empty() {
            let empty_text = if state.events.is_empty() {
                "Waiting for new log lines..."
            } else {
                "No lines match active filters."
            };
            Paragraph::new(empty_text)
                .style(Style::default().fg(palette.muted_fg).bg(palette.background))
                .block(Block::default())
        } else {
            Paragraph::new(lines)
                .style(
                    Style::default()
                        .fg(palette.content_fg)
                        .bg(palette.background),
                )
                .block(Block::default())
        }
    };
    frame.render_widget(content, areas[1]);
}

#[derive(Debug, Clone, Copy)]
struct TuiThemePalette {
    header_fg: Color,
    content_fg: Color,
    muted_fg: Color,
    accent_fg: Color,
    selected_fg: Color,
    selected_bg: Color,
    background: Color,
}

fn tui_theme_palette(theme: GuiTheme) -> TuiThemePalette {
    match theme {
        GuiTheme::DefaultDark => TuiThemePalette {
            header_fg: Color::Gray,
            content_fg: Color::White,
            muted_fg: Color::DarkGray,
            accent_fg: Color::Cyan,
            selected_fg: Color::Black,
            selected_bg: Color::Green,
            background: Color::Black,
        },
        GuiTheme::Light => TuiThemePalette {
            header_fg: Color::Black,
            content_fg: Color::Black,
            muted_fg: Color::Gray,
            accent_fg: Color::Blue,
            selected_fg: Color::White,
            selected_bg: Color::Blue,
            background: Color::White,
        },
        GuiTheme::HighContrast => TuiThemePalette {
            header_fg: Color::Yellow,
            content_fg: Color::White,
            muted_fg: Color::Gray,
            accent_fg: Color::LightCyan,
            selected_fg: Color::Black,
            selected_bg: Color::Yellow,
            background: Color::Black,
        },
        GuiTheme::OceanBlue => TuiThemePalette {
            header_fg: Color::Rgb(102, 194, 255),
            content_fg: Color::Rgb(220, 235, 250),
            muted_fg: Color::Rgb(132, 153, 176),
            accent_fg: Color::Rgb(82, 170, 232),
            selected_fg: Color::Rgb(220, 235, 250),
            selected_bg: Color::Rgb(45, 80, 112),
            background: Color::Rgb(18, 26, 36),
        },
        GuiTheme::ShadesOfPurple => TuiThemePalette {
            header_fg: Color::Rgb(203, 166, 255),
            content_fg: Color::Rgb(236, 225, 255),
            muted_fg: Color::Rgb(160, 135, 194),
            accent_fg: Color::Rgb(173, 130, 255),
            selected_fg: Color::Rgb(242, 230, 255),
            selected_bg: Color::Rgb(96, 63, 145),
            background: Color::Rgb(31, 20, 51),
        },
        GuiTheme::Novare => TuiThemePalette {
            header_fg: Color::Rgb(109, 219, 212),
            content_fg: Color::Rgb(226, 232, 247),
            muted_fg: Color::Rgb(140, 160, 186),
            accent_fg: Color::Rgb(116, 99, 184),
            selected_fg: Color::Rgb(236, 241, 255),
            selected_bg: Color::Rgb(83, 128, 166),
            background: Color::Rgb(14, 24, 38),
        },
        GuiTheme::NovareLight => TuiThemePalette {
            header_fg: Color::Rgb(32, 73, 84),
            content_fg: Color::Rgb(32, 73, 84),
            muted_fg: Color::Rgb(82, 131, 140),
            accent_fg: Color::Rgb(41, 169, 143),
            selected_fg: Color::Rgb(17, 50, 57),
            selected_bg: Color::Rgb(120, 206, 186),
            background: Color::Rgb(250, 255, 253),
        },
        GuiTheme::Dracula => TuiThemePalette {
            header_fg: Color::Rgb(139, 233, 253),
            content_fg: Color::Rgb(248, 248, 242),
            muted_fg: Color::Rgb(138, 146, 168),
            accent_fg: Color::Rgb(255, 121, 198),
            selected_fg: Color::Rgb(248, 248, 242),
            selected_bg: Color::Rgb(98, 114, 164),
            background: Color::Rgb(40, 42, 54),
        },
        GuiTheme::Nord => TuiThemePalette {
            header_fg: Color::Rgb(136, 192, 208),
            content_fg: Color::Rgb(216, 222, 233),
            muted_fg: Color::Rgb(143, 156, 182),
            accent_fg: Color::Rgb(129, 161, 193),
            selected_fg: Color::Rgb(236, 239, 244),
            selected_bg: Color::Rgb(94, 129, 172),
            background: Color::Rgb(46, 52, 64),
        },
        GuiTheme::SolarizedDark => TuiThemePalette {
            header_fg: Color::Rgb(38, 139, 210),
            content_fg: Color::Rgb(131, 148, 150),
            muted_fg: Color::Rgb(88, 110, 117),
            accent_fg: Color::Rgb(42, 161, 152),
            selected_fg: Color::Rgb(238, 232, 213),
            selected_bg: Color::Rgb(7, 54, 66),
            background: Color::Rgb(0, 43, 54),
        },
        GuiTheme::SolarizedLight => TuiThemePalette {
            header_fg: Color::Rgb(38, 139, 210),
            content_fg: Color::Rgb(88, 110, 117),
            muted_fg: Color::Rgb(147, 161, 161),
            accent_fg: Color::Rgb(181, 137, 0),
            selected_fg: Color::Rgb(88, 110, 117),
            selected_bg: Color::Rgb(238, 232, 213),
            background: Color::Rgb(253, 246, 227),
        },
        GuiTheme::OneDark => TuiThemePalette {
            header_fg: Color::Rgb(97, 175, 239),
            content_fg: Color::Rgb(171, 178, 191),
            muted_fg: Color::Rgb(128, 136, 150),
            accent_fg: Color::Rgb(198, 120, 221),
            selected_fg: Color::Rgb(220, 223, 228),
            selected_bg: Color::Rgb(62, 68, 82),
            background: Color::Rgb(40, 44, 52),
        },
    }
}

#[derive(Debug)]
struct TuiState {
    events: VecDeque<LogEvent>,
    total_events_seen: u64,
    dropped_events: u64,
    suppressed_by_rules: u64,
    paused: bool,
    scroll_offset: usize,
    sources: Vec<String>,
    tracked_files: Vec<String>,
    status_message: String,
    active_source_filter_idx: Option<usize>,
    text_filter: String,
    text_filter_folded: String,
    case_insensitive_text_filter: bool,
    active_theme: GuiTheme,
    theme_picker_idx: usize,
    search_input: String,
    add_file_input: String,
    pending_add_tracked_file: Option<String>,
    save_requested: bool,
    input_mode: InputMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    Search,
    AddTrackedFile,
    ThemePicker,
    ListTrackedFiles,
    Help,
}

impl TuiState {
    fn new(config: &AppConfig) -> Self {
        let sources = config
            .tracked_files
            .iter()
            .map(|path| {
                path.file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string())
            })
            .collect();
        let tracked_files = config
            .tracked_files
            .iter()
            .map(|path| path.display().to_string())
            .collect();

        Self {
            events: VecDeque::with_capacity(config.max_buffer_lines),
            total_events_seen: 0,
            dropped_events: 0,
            suppressed_by_rules: 0,
            paused: false,
            scroll_offset: 0,
            sources,
            tracked_files,
            status_message: String::new(),
            active_source_filter_idx: None,
            text_filter: String::new(),
            text_filter_folded: String::new(),
            case_insensitive_text_filter: config.case_insensitive_text_filter,
            active_theme: config.gui_theme,
            theme_picker_idx: theme_index(config.gui_theme),
            search_input: String::new(),
            add_file_input: String::new(),
            pending_add_tracked_file: None,
            save_requested: false,
            input_mode: InputMode::Normal,
        }
    }

    fn push_events(&mut self, events: Vec<LogEvent>, suppressed: usize, max_buffer_lines: usize) {
        self.suppressed_by_rules += suppressed as u64;
        for event in events {
            self.total_events_seen += 1;
            self.events.push_back(event);
            while self.events.len() > max_buffer_lines {
                self.events.pop_front();
                self.dropped_events += 1;
            }
        }
        if self.scroll_offset > 0 {
            self.scroll_offset = self
                .scroll_offset
                .min(self.filtered_len().saturating_sub(1));
        }
    }

    fn handle_key(&mut self, key: KeyCode) -> bool {
        if self.input_mode == InputMode::Search {
            return self.handle_search_key(key);
        }
        if self.input_mode == InputMode::AddTrackedFile {
            return self.handle_add_file_key(key);
        }
        if self.input_mode == InputMode::ThemePicker {
            return self.handle_theme_picker_key(key);
        }
        if self.input_mode == InputMode::ListTrackedFiles {
            return self.handle_tracked_files_key(key);
        }
        if self.input_mode == InputMode::Help {
            return self.handle_help_key(key);
        }

        match key {
            KeyCode::Char('q') => true,
            KeyCode::Char('p') => {
                self.paused = !self.paused;
                false
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let max_scroll = self.filtered_len().saturating_sub(1);
                self.scroll_offset = (self.scroll_offset + 1).min(max_scroll);
                false
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
                false
            }
            KeyCode::Char('g') => {
                self.scroll_offset = self.filtered_len().saturating_sub(1);
                false
            }
            KeyCode::Char('G') => {
                self.scroll_offset = 0;
                false
            }
            KeyCode::Char('f') => {
                self.cycle_filter();
                false
            }
            KeyCode::Char('i') => {
                self.case_insensitive_text_filter = !self.case_insensitive_text_filter;
                false
            }
            KeyCode::Char('/') => {
                self.input_mode = InputMode::Search;
                self.search_input = self.text_filter.clone();
                false
            }
            KeyCode::Char('c') => {
                self.set_text_filter(String::new());
                self.scroll_offset = 0;
                false
            }
            KeyCode::Char('x') => {
                self.clear_screen();
                false
            }
            KeyCode::Char('a') => {
                self.input_mode = InputMode::AddTrackedFile;
                self.add_file_input.clear();
                false
            }
            KeyCode::Char('s') => {
                self.save_requested = true;
                false
            }
            KeyCode::Char('l') => {
                self.input_mode = InputMode::ListTrackedFiles;
                false
            }
            KeyCode::Char('t') => {
                self.theme_picker_idx = theme_index(self.active_theme);
                self.input_mode = InputMode::ThemePicker;
                false
            }
            KeyCode::Char('?') => {
                self.input_mode = InputMode::Help;
                false
            }
            _ => false,
        }
    }

    fn handle_search_key(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Enter => {
                self.set_text_filter(self.search_input.trim().to_string());
                self.search_input.clear();
                self.input_mode = InputMode::Normal;
                self.scroll_offset = 0;
            }
            KeyCode::Backspace => {
                self.search_input.pop();
            }
            KeyCode::Char(c) => {
                self.search_input.push(c);
            }
            _ => {}
        }
        false
    }

    fn handle_add_file_key(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Esc => {
                self.add_file_input.clear();
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Enter => {
                self.pending_add_tracked_file = Some(self.add_file_input.trim().to_string());
                self.add_file_input.clear();
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Backspace => {
                self.add_file_input.pop();
            }
            KeyCode::Char(c) => {
                self.add_file_input.push(c);
            }
            _ => {}
        }
        false
    }

    fn handle_paste(&mut self, text: String) {
        if self.input_mode == InputMode::ThemePicker {
            return;
        }
        if self.input_mode == InputMode::AddTrackedFile {
            let sanitized = text.replace(['\n', '\r'], "");
            self.add_file_input.push_str(&sanitized);
            return;
        }

        let trimmed = text.trim();
        if !looks_like_path(trimmed) {
            return;
        }
        self.pending_add_tracked_file = Some(trimmed.to_string());
    }

    fn handle_tracked_files_key(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Char('q') => true,
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('l') => {
                self.input_mode = InputMode::Normal;
                false
            }
            KeyCode::Char('?') => {
                self.input_mode = InputMode::Help;
                false
            }
            _ => false,
        }
    }

    fn handle_theme_picker_key(&mut self, key: KeyCode) -> bool {
        let max_idx = GuiTheme::all().len().saturating_sub(1);
        match key {
            KeyCode::Char('q') => true,
            KeyCode::Esc | KeyCode::Char('t') => {
                self.input_mode = InputMode::Normal;
                false
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.theme_picker_idx = self.theme_picker_idx.saturating_sub(1);
                false
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.theme_picker_idx = (self.theme_picker_idx + 1).min(max_idx);
                false
            }
            KeyCode::Enter => {
                let selected = GuiTheme::all()[self.theme_picker_idx];
                self.active_theme = selected;
                self.set_status(format!("Theme: {}", selected.display_name()));
                self.input_mode = InputMode::Normal;
                false
            }
            _ => false,
        }
    }

    fn handle_help_key(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Char('q') => true,
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') => {
                self.input_mode = InputMode::Normal;
                false
            }
            _ => false,
        }
    }

    fn header_line(&self, config: &AppConfig) -> String {
        if self.input_mode == InputMode::Search {
            return format!(
                "search:/{}  Enter apply  Esc cancel  Backspace delete",
                self.search_input
            );
        }
        if self.input_mode == InputMode::AddTrackedFile {
            return format!(
                "add file:{}  Enter add  Esc cancel  Backspace delete",
                self.add_file_input
            );
        }
        if self.input_mode == InputMode::ListTrackedFiles {
            return format!(
                "tracked files  keys:l/Esc/Enter close  a add file  t themes  q quit  theme={}",
                self.active_theme.display_name()
            );
        }
        if self.input_mode == InputMode::ThemePicker {
            let preview = self.preview_theme();
            return format!(
                "theme picker  Up/Down select  Enter apply  Esc/t cancel  q quit  preview={}  saved={}",
                preview.display_name(),
                self.active_theme.display_name()
            );
        }
        if self.input_mode == InputMode::Help {
            return "help  keys:?/Esc/Enter close  q quit".to_string();
        }

        let source_filter_label = match self.active_source_filter_idx {
            Some(idx) => format!("filter={}", self.sources[idx]),
            None => "filter=all".to_string(),
        };
        format!(
            "logtrak  files={}  poll={}ms  lines={}  {}  {}{}  keys:q p j/k g/G f l a t s / c x i ?",
            self.sources.len(),
            config.poll_interval_ms,
            self.events.len(),
            if self.paused { "paused" } else { "live" },
            source_filter_label,
            if self.status_message.is_empty() {
                String::new()
            } else {
                format!("  status={}", self.status_message)
            },
        )
    }

    fn tracked_file_lines(&self) -> Vec<Line<'static>> {
        self.tracked_files
            .iter()
            .enumerate()
            .map(|(idx, path)| Line::from(format!("{:>2}. {}", idx + 1, path)))
            .collect()
    }

    fn command_help_lines(&self) -> Vec<Line<'static>> {
        vec![
            Line::from("CLI Command Help (single-letter keys)"),
            Line::from(""),
            Line::from("CLI exe: logtrak-cli.exe"),
            Line::from("GUI exe: logtrak.exe"),
            Line::from("Example: logtrak-cli.exe --config .\\logtrak.toml"),
            Line::from(""),
            Line::from("q  Quit"),
            Line::from("p  Pause/resume polling"),
            Line::from("j  Scroll toward newest lines"),
            Line::from("k  Scroll toward older lines"),
            Line::from("g  Jump to oldest retained lines"),
            Line::from("G  Jump to newest lines"),
            Line::from("f  Cycle source-file filter"),
            Line::from("l  Show tracked file list"),
            Line::from("a  Add a log file to track (type/paste path, Enter)"),
            Line::from("t  Open theme picker"),
            Line::from("s  Save active config to disk"),
            Line::from("c  Clear text filter"),
            Line::from("x  Clear displayed log lines"),
            Line::from("i  Toggle case-insensitive text filter"),
            Line::from("?  Toggle this help panel"),
            Line::from(""),
            Line::from("Close help: ? / Esc / Enter"),
        ]
    }

    fn visible_lines(&self, show_timestamps: bool, height: usize) -> Vec<Line<'static>> {
        let filtered = self.filtered_events();
        if filtered.is_empty() {
            return Vec::new();
        }
        let total = filtered.len();
        let clamped_offset = self.scroll_offset.min(total.saturating_sub(1));
        let end_exclusive = total.saturating_sub(clamped_offset);
        let start = end_exclusive.saturating_sub(height);

        filtered[start..end_exclusive]
            .iter()
            .map(|event| Line::from(format_event_line(event, show_timestamps)))
            .collect()
    }

    fn filtered_len(&self) -> usize {
        self.events
            .iter()
            .filter(|event| self.filter_matches(event))
            .count()
    }

    fn filtered_events(&self) -> Vec<&LogEvent> {
        self.events
            .iter()
            .filter(|event| self.filter_matches(event))
            .collect()
    }

    fn filter_matches(&self, event: &LogEvent) -> bool {
        let source_match = match self.active_source_filter_idx {
            Some(idx) => self
                .sources
                .get(idx)
                .is_some_and(|source| source == &event.source),
            None => true,
        };
        let text_match = if self.text_filter.is_empty() {
            true
        } else if self.case_insensitive_text_filter {
            event
                .line
                .to_lowercase()
                .contains(self.text_filter_folded.as_str())
        } else {
            event.line.contains(&self.text_filter)
        };
        source_match && text_match
    }

    fn cycle_filter(&mut self) {
        self.active_source_filter_idx = match self.active_source_filter_idx {
            None if !self.sources.is_empty() => Some(0),
            Some(idx) if idx + 1 < self.sources.len() => Some(idx + 1),
            _ => None,
        };
        self.scroll_offset = 0;
    }

    fn set_text_filter(&mut self, filter: String) {
        self.text_filter = filter;
        self.text_filter_folded = self.text_filter.to_lowercase();
    }

    fn set_available_sources(&mut self, sources: Vec<String>) {
        let selected_source = self
            .active_source_filter_idx
            .and_then(|idx| self.sources.get(idx).cloned());
        self.sources = sources;
        self.active_source_filter_idx =
            selected_source.and_then(|selected| self.sources.iter().position(|s| s == &selected));
    }

    fn take_pending_add_tracked_file(&mut self) -> Option<String> {
        self.pending_add_tracked_file.take()
    }

    fn add_tracked_file(&mut self, path: PathBuf) {
        self.tracked_files.push(path.display().to_string());
        self.set_status(format!("Now tracking {}", path.display()));
    }

    fn set_status(&mut self, message: String) {
        self.status_message = message;
    }

    fn clear_screen(&mut self) {
        self.events.clear();
        self.total_events_seen = 0;
        self.dropped_events = 0;
        self.suppressed_by_rules = 0;
        self.scroll_offset = 0;
        self.set_status("Cleared displayed log output".to_string());
    }

    fn take_save_requested(&mut self) -> bool {
        let requested = self.save_requested;
        self.save_requested = false;
        requested
    }

    fn preview_theme(&self) -> GuiTheme {
        if self.input_mode == InputMode::ThemePicker {
            GuiTheme::all()[self.theme_picker_idx]
        } else {
            self.active_theme
        }
    }

    fn theme_picker_lines(&self) -> Vec<Line<'static>> {
        let palette = tui_theme_palette(self.preview_theme());
        let mut lines = vec![
            Line::from("Theme Picker"),
            Line::from("Use Up/Down to select, Enter to apply, Esc to cancel."),
            Line::from(""),
        ];
        for (idx, theme) in GuiTheme::all().iter().enumerate() {
            let is_selected = idx == self.theme_picker_idx;
            let marker = if is_selected { ">" } else { " " };
            let label = format!("{} {}", marker, theme.display_name());
            let style = if is_selected {
                Style::default()
                    .fg(palette.selected_fg)
                    .bg(palette.selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else if *theme == self.active_theme {
                Style::default()
                    .fg(palette.accent_fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.content_fg)
            };
            lines.push(Line::styled(label, style));
        }
        lines
    }
}

fn sync_runtime_config_from_state(config: &mut AppConfig, state: &TuiState) {
    config.case_insensitive_text_filter = state.case_insensitive_text_filter;
    config.gui_theme = state.active_theme;
    config.gui_light_mode = state.active_theme.is_light();
}

fn save_runtime_config(config: &AppConfig, config_path: Option<&Path>, state: &mut TuiState) {
    let Some(path) = config_path else {
        state.set_status("No config path available to save".to_string());
        return;
    };

    match config.write_to_file(path) {
        Ok(()) => state.set_status(format!("Saved {}", path.display())),
        Err(err) => state.set_status(format!("Failed to save {}: {}", path.display(), err)),
    }
}

fn theme_index(theme: GuiTheme) -> usize {
    GuiTheme::all()
        .iter()
        .position(|value| *value == theme)
        .unwrap_or(0)
}

fn parse_cli_input_path(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        return Some(PathBuf::from(&trimmed[1..trimmed.len() - 1]));
    }
    Some(PathBuf::from(trimmed))
}

fn looks_like_path(input: &str) -> bool {
    if input.is_empty() {
        return false;
    }
    input.starts_with('/')
        || input.starts_with("./")
        || input.starts_with("../")
        || input.starts_with('~')
        || input.contains('\\')
        || input.chars().nth(1).is_some_and(|ch| ch == ':')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GuiTheme;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn test_config() -> AppConfig {
        AppConfig {
            poll_interval_ms: 1000,
            tracked_files: vec![PathBuf::from("/tmp/a.log"), PathBuf::from("/tmp/b.log")],
            max_buffer_lines: 100,
            max_line_len: 256,
            show_timestamps: true,
            gui_light_mode: false,
            gui_theme: GuiTheme::DefaultDark,
            gui_word_wrap: true,
            gui_font_size: 14.0,
            case_insensitive_text_filter: true,
            blacklist_regex: Vec::new(),
            whitelist_regex: Vec::new(),
        }
    }

    #[test]
    fn applies_text_filter_after_search_mode_submit() {
        let config = test_config();
        let mut state = TuiState::new(&config);
        state.push_events(
            vec![
                LogEvent {
                    ts: SystemTime::UNIX_EPOCH,
                    source: "a.log".to_string(),
                    line: "INFO app started".to_string(),
                },
                LogEvent {
                    ts: SystemTime::UNIX_EPOCH,
                    source: "b.log".to_string(),
                    line: "ERROR failed to bind".to_string(),
                },
            ],
            0,
            config.max_buffer_lines,
        );

        state.handle_key(KeyCode::Char('/'));
        state.handle_key(KeyCode::Char('E'));
        state.handle_key(KeyCode::Char('R'));
        state.handle_key(KeyCode::Char('R'));
        state.handle_key(KeyCode::Char('O'));
        state.handle_key(KeyCode::Char('R'));
        state.handle_key(KeyCode::Enter);

        assert_eq!(state.text_filter, "ERROR");
        assert_eq!(state.filtered_len(), 1);
    }

    #[test]
    fn clear_text_filter_restores_all_lines() {
        let config = test_config();
        let mut state = TuiState::new(&config);
        state.push_events(
            vec![
                LogEvent {
                    ts: SystemTime::UNIX_EPOCH,
                    source: "a.log".to_string(),
                    line: "alpha".to_string(),
                },
                LogEvent {
                    ts: SystemTime::UNIX_EPOCH,
                    source: "b.log".to_string(),
                    line: "beta".to_string(),
                },
            ],
            0,
            config.max_buffer_lines,
        );
        state.set_text_filter("alpha".to_string());
        assert_eq!(state.filtered_len(), 1);

        state.handle_key(KeyCode::Char('c'));
        assert!(state.text_filter.is_empty());
        assert_eq!(state.filtered_len(), 2);
    }

    #[test]
    fn x_clears_displayed_lines() {
        let config = test_config();
        let mut state = TuiState::new(&config);
        state.push_events(
            vec![LogEvent {
                ts: SystemTime::UNIX_EPOCH,
                source: "a.log".to_string(),
                line: "alpha".to_string(),
            }],
            0,
            config.max_buffer_lines,
        );
        assert_eq!(state.events.len(), 1);

        state.handle_key(KeyCode::Char('x'));

        assert!(state.events.is_empty());
        assert_eq!(state.total_events_seen, 0);
    }

    #[test]
    fn case_insensitive_filter_matches_different_case() {
        let config = test_config();
        let mut state = TuiState::new(&config);
        state.push_events(
            vec![LogEvent {
                ts: SystemTime::UNIX_EPOCH,
                source: "a.log".to_string(),
                line: "Error: failed to bind".to_string(),
            }],
            0,
            config.max_buffer_lines,
        );
        state.set_text_filter("error".to_string());
        assert_eq!(state.filtered_len(), 1);
    }

    #[test]
    fn case_sensitive_filter_can_be_toggled() {
        let config = test_config();
        let mut state = TuiState::new(&config);
        state.push_events(
            vec![LogEvent {
                ts: SystemTime::UNIX_EPOCH,
                source: "a.log".to_string(),
                line: "Error: failed to bind".to_string(),
            }],
            0,
            config.max_buffer_lines,
        );
        state.set_text_filter("error".to_string());
        assert_eq!(state.filtered_len(), 1);

        state.handle_key(KeyCode::Char('i'));
        assert_eq!(state.filtered_len(), 0);
    }

    #[test]
    fn l_opens_and_closes_tracked_files_view() {
        let config = test_config();
        let mut state = TuiState::new(&config);

        assert_eq!(state.input_mode, InputMode::Normal);
        state.handle_key(KeyCode::Char('l'));
        assert_eq!(state.input_mode, InputMode::ListTrackedFiles);
        state.handle_key(KeyCode::Esc);
        assert_eq!(state.input_mode, InputMode::Normal);
    }

    #[test]
    fn available_sources_replace_raw_pattern_entries() {
        let mut config = test_config();
        config.tracked_files = vec![PathBuf::from("/tmp/app_{today}.log")];

        let mut state = TuiState::new(&config);
        state.set_available_sources(vec![
            "app_20260425.log".to_string(),
            "app_0425.log".to_string(),
        ]);

        assert_eq!(
            state.sources,
            vec!["app_20260425.log".to_string(), "app_0425.log".to_string()]
        );
    }

    #[test]
    fn question_mark_toggles_help_view() {
        let config = test_config();
        let mut state = TuiState::new(&config);

        assert_eq!(state.input_mode, InputMode::Normal);
        state.handle_key(KeyCode::Char('?'));
        assert_eq!(state.input_mode, InputMode::Help);
        state.handle_key(KeyCode::Char('?'));
        assert_eq!(state.input_mode, InputMode::Normal);
    }

    #[test]
    fn parse_cli_input_path_trims_and_unquotes() {
        assert_eq!(
            parse_cli_input_path(r#"  "/tmp/a.log"  "#),
            Some(PathBuf::from("/tmp/a.log"))
        );
        assert_eq!(
            parse_cli_input_path("'/tmp/b.log'"),
            Some(PathBuf::from("/tmp/b.log"))
        );
        assert_eq!(
            parse_cli_input_path("/tmp/c.log"),
            Some(PathBuf::from("/tmp/c.log"))
        );
        assert_eq!(parse_cli_input_path("   "), None);
    }

    #[test]
    fn add_file_input_submits_pending_path() {
        let config = test_config();
        let mut state = TuiState::new(&config);

        state.handle_key(KeyCode::Char('a'));
        assert_eq!(state.input_mode, InputMode::AddTrackedFile);
        state.handle_key(KeyCode::Char('/'));
        state.handle_key(KeyCode::Char('t'));
        state.handle_key(KeyCode::Char('m'));
        state.handle_key(KeyCode::Char('p'));
        state.handle_key(KeyCode::Char('/'));
        state.handle_key(KeyCode::Char('n'));
        state.handle_key(KeyCode::Char('e'));
        state.handle_key(KeyCode::Char('w'));
        state.handle_key(KeyCode::Char('.'));
        state.handle_key(KeyCode::Char('l'));
        state.handle_key(KeyCode::Char('o'));
        state.handle_key(KeyCode::Char('g'));
        state.handle_key(KeyCode::Enter);

        assert_eq!(state.input_mode, InputMode::Normal);
        assert_eq!(
            state.take_pending_add_tracked_file(),
            Some("/tmp/new.log".to_string())
        );
    }

    #[test]
    fn paste_path_in_normal_mode_queues_add_file() {
        let config = test_config();
        let mut state = TuiState::new(&config);

        state.handle_paste("/tmp/from-paste.log".to_string());
        assert_eq!(
            state.take_pending_add_tracked_file(),
            Some("/tmp/from-paste.log".to_string())
        );
    }

    #[test]
    fn t_opens_theme_picker_and_enter_applies_selection() {
        let config = test_config();
        let mut state = TuiState::new(&config);
        let expected = GuiTheme::all()[1];

        state.handle_key(KeyCode::Char('t'));
        assert_eq!(state.input_mode, InputMode::ThemePicker);
        state.handle_key(KeyCode::Down);
        state.handle_key(KeyCode::Enter);

        assert_eq!(state.input_mode, InputMode::Normal);
        assert_eq!(state.active_theme, expected);
    }

    #[test]
    fn s_requests_save_and_resets_flag() {
        let config = test_config();
        let mut state = TuiState::new(&config);

        state.handle_key(KeyCode::Char('s'));
        assert!(state.take_save_requested());
        assert!(!state.take_save_requested());
    }
}
