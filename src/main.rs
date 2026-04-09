mod cli;
mod config;
mod formatting;
mod ui;
mod watcher;

use anyhow::Result;
use clap::Parser;
use cli::CliArgs;
use config::AppConfig;
use formatting::format_event_line;
use std::time::Duration;
use watcher::PollingWatcher;

fn main() -> Result<()> {
    let cli = CliArgs::parse();
    let config = AppConfig::from_cli(&cli)?;

    if cli.print_config_only {
        println!("{}", config.summary_string());
        return Ok(());
    }

    if cli.headless {
        run_headless(config)
    } else {
        ui::run_tui(config)
    }
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
