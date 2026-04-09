use crate::config::AppConfig;
use crate::formatting::format_event_line;
use crate::watcher::{LogEvent, PollingWatcher};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph};
use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

pub fn run_tui(config: AppConfig) -> Result<()> {
    let poll_interval = Duration::from_millis(config.poll_interval_ms);
    let mut watcher = PollingWatcher::new(config.tracked_files.clone(), config.max_line_len)?;
    let mut state = TuiState::new(&config);

    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let _cleanup = TerminalCleanup;
    let mut terminal = ratatui::Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let mut next_poll = Instant::now();
    let mut dirty = true;
    loop {
        if !state.paused && Instant::now() >= next_poll {
            let events = watcher.poll()?;
            if !events.is_empty() {
                state.push_events(events, config.max_buffer_lines);
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
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if state.handle_key(key.code) {
            break;
        }
        dirty = true;
    }
    Ok(())
}

struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

fn render(frame: &mut Frame<'_>, state: &TuiState, config: &AppConfig) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(frame.area());

    let header = Paragraph::new(state.header_line(config))
        .style(Style::default().fg(Color::Gray))
        .block(Block::default());
    frame.render_widget(header, areas[0]);

    let lines = state.visible_lines(config.show_timestamps, areas[1].height as usize);
    let content = if lines.is_empty() {
        let empty_text = if state.events.is_empty() {
            "Waiting for new log lines..."
        } else {
            "No lines match active filters."
        };
        Paragraph::new(empty_text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default())
    } else {
        Paragraph::new(lines).block(Block::default())
    };
    frame.render_widget(content, areas[1]);
}

#[derive(Debug)]
struct TuiState {
    events: VecDeque<LogEvent>,
    total_events_seen: u64,
    dropped_events: u64,
    paused: bool,
    scroll_offset: usize,
    sources: Vec<String>,
    active_source_filter_idx: Option<usize>,
    text_filter: String,
    search_input: String,
    input_mode: InputMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    Search,
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

        Self {
            events: VecDeque::with_capacity(config.max_buffer_lines),
            total_events_seen: 0,
            dropped_events: 0,
            paused: false,
            scroll_offset: 0,
            sources,
            active_source_filter_idx: None,
            text_filter: String::new(),
            search_input: String::new(),
            input_mode: InputMode::Normal,
        }
    }

    fn push_events(&mut self, events: Vec<LogEvent>, max_buffer_lines: usize) {
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
            KeyCode::Char('/') => {
                self.input_mode = InputMode::Search;
                self.search_input = self.text_filter.clone();
                false
            }
            KeyCode::Char('c') => {
                self.text_filter.clear();
                self.scroll_offset = 0;
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
                self.text_filter = self.search_input.trim().to_string();
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

    fn header_line(&self, config: &AppConfig) -> String {
        if self.input_mode == InputMode::Search {
            return format!(
                "search:/{}  Enter apply  Esc cancel  Backspace delete",
                self.search_input
            );
        }

        let source_filter_label = match self.active_source_filter_idx {
            Some(idx) => format!("filter={}", self.sources[idx]),
            None => "filter=all".to_string(),
        };
        let text_filter_label = if self.text_filter.is_empty() {
            "text=off".to_string()
        } else {
            format!("text={}", self.text_filter)
        };
        format!(
            "rustylogviewer  files={}  poll={}ms  lines={}  seen={}  dropped={}  {}  {}  {}  keys:q p j/k g/G f / c",
            self.sources.len(),
            config.poll_interval_ms,
            self.events.len(),
            self.total_events_seen,
            self.dropped_events,
            if self.paused { "paused" } else { "live" },
            source_filter_label,
            text_filter_label
        )
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
        let text_match = self.text_filter.is_empty() || event.line.contains(&self.text_filter);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn test_config() -> AppConfig {
        AppConfig {
            poll_interval_ms: 1000,
            tracked_files: vec![PathBuf::from("/tmp/a.log"), PathBuf::from("/tmp/b.log")],
            max_buffer_lines: 100,
            max_line_len: 256,
            show_timestamps: true,
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
            config.max_buffer_lines,
        );
        state.text_filter = "alpha".to_string();
        assert_eq!(state.filtered_len(), 1);

        state.handle_key(KeyCode::Char('c'));
        assert!(state.text_filter.is_empty());
        assert_eq!(state.filtered_len(), 2);
    }
}
