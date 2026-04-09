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
    loop {
        if !state.paused && Instant::now() >= next_poll {
            let events = watcher.poll()?;
            state.push_events(events, config.max_buffer_lines);
            next_poll = Instant::now() + poll_interval;
        }

        terminal.draw(|frame| render(frame, &state, &config))?;

        let timeout = if state.paused {
            Duration::from_millis(250)
        } else {
            next_poll
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(250))
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
        Paragraph::new("Waiting for new log lines...")
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
    paused: bool,
    scroll_offset: usize,
    sources: Vec<String>,
    active_filter_idx: Option<usize>,
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
            paused: false,
            scroll_offset: 0,
            sources,
            active_filter_idx: None,
        }
    }

    fn push_events(&mut self, events: Vec<LogEvent>, max_buffer_lines: usize) {
        for event in events {
            self.events.push_back(event);
            while self.events.len() > max_buffer_lines {
                self.events.pop_front();
            }
        }
        if self.scroll_offset > 0 {
            self.scroll_offset = self
                .scroll_offset
                .min(self.filtered_len().saturating_sub(1));
        }
    }

    fn handle_key(&mut self, key: KeyCode) -> bool {
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
            _ => false,
        }
    }

    fn header_line(&self, config: &AppConfig) -> String {
        let filter_label = match self.active_filter_idx {
            Some(idx) => format!("filter={}", self.sources[idx]),
            None => "filter=all".to_string(),
        };
        format!(
            "rustylogviewer  files={}  poll={}ms  lines={}  {}  {}  keys:q quit p pause j/k scroll g/G f",
            self.sources.len(),
            config.poll_interval_ms,
            self.events.len(),
            if self.paused { "paused" } else { "live" },
            filter_label
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
        match self.active_filter_idx {
            Some(idx) => self
                .sources
                .get(idx)
                .is_some_and(|source| source == &event.source),
            None => true,
        }
    }

    fn cycle_filter(&mut self) {
        self.active_filter_idx = match self.active_filter_idx {
            None if !self.sources.is_empty() => Some(0),
            Some(idx) if idx + 1 < self.sources.len() => Some(idx + 1),
            _ => None,
        };
        self.scroll_offset = 0;
    }
}
