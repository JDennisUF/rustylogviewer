mod cli;
mod config;
mod watcher;

use anyhow::Result;
use clap::Parser;
use cli::CliArgs;
use config::AppConfig;
use std::time::Duration;
use time::{OffsetDateTime, UtcOffset, format_description};
use watcher::PollingWatcher;

fn main() -> Result<()> {
    let cli = CliArgs::parse();
    let config = AppConfig::from_cli(&cli)?;

    if cli.print_config_only {
        println!("{}", config.summary_string());
        return Ok(());
    }

    run_headless(config)
}

fn run_headless(config: AppConfig) -> Result<()> {
    println!("{}", config.summary_string());
    println!("Starting headless watcher. Press Ctrl-C to exit.");

    let mut watcher = PollingWatcher::new(config.tracked_files.clone(), config.max_line_len)?;
    loop {
        let events = watcher.poll()?;
        for event in events {
            println!("{}", format_event_line(&event, config.show_timestamps));
        }
        std::thread::sleep(Duration::from_millis(config.poll_interval_ms));
    }
}

fn format_event_line(event: &watcher::LogEvent, show_timestamps: bool) -> String {
    if !show_timestamps {
        return format!("[{}] {}", event.source, event.line);
    }

    let time_fragment = local_hms(event.ts).unwrap_or_else(|| "??:??:??".to_string());
    format!("[{}] [{}] {}", time_fragment, event.source, event.line)
}

fn local_hms(ts: std::time::SystemTime) -> Option<String> {
    let fmt = format_description::parse("[hour]:[minute]:[second]").ok()?;
    let offset = UtcOffset::current_local_offset().ok()?;
    let datetime = OffsetDateTime::from(ts).to_offset(offset);
    datetime.format(&fmt).ok()
}
