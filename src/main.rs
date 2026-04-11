use anyhow::Result;
use clap::Parser;
use logtrak::cli::CliArgs;
use logtrak::config::AppConfig;
use logtrak::formatting::format_event_line;
use logtrak::gui;
use logtrak::line_rules::LineRules;
use logtrak::ui;
use logtrak::watcher::PollingWatcher;
use std::time::Duration;

fn main() -> Result<()> {
    let cli = CliArgs::parse();
    if cli.gui {
        return gui::run_gui(cli.config.clone());
    }

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
    let rules = LineRules::new(&config.blacklist_regex, &config.whitelist_regex)?;
    loop {
        let events = watcher.poll()?;
        let (events, _suppressed) = rules.partition_events(events);
        for event in events {
            println!("{}", format_event_line(&event, config.show_timestamps));
        }
        std::thread::sleep(Duration::from_millis(config.poll_interval_ms));
    }
}
