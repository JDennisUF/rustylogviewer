use anyhow::{Context, Result, bail};
use clap::Parser;
use logtrak::cli::CliArgs;
use logtrak::config::AppConfig;
use logtrak::formatting::format_event_line;
use logtrak::gui;
use logtrak::line_rules::LineRules;
use logtrak::ui;
use logtrak::watcher::PollingWatcher;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn main() -> Result<()> {
    let mut cli = CliArgs::parse();
    ensure_cli_default_config(&mut cli)?;

    if cli.gui {
        return gui::run_gui(cli.config.clone());
    }

    let config = AppConfig::from_cli_allow_empty_files(&cli)?;

    if cli.print_config_only {
        println!("{}", config.summary_string());
        return Ok(());
    }

    if cli.headless {
        run_headless(config)
    } else {
        ui::run_tui(config, cli.config.clone())
    }
}

fn run_headless(config: AppConfig) -> Result<()> {
    println!("{}", config.summary_string());
    println!("Starting headless watcher. Press Ctrl-C to exit.");

    let mut watcher = PollingWatcher::with_file_enabled(
        config.tracked_files.clone(),
        config.tracked_file_enabled_map(),
        config.max_line_len,
    )?;
    let rules = LineRules::new(&config.blacklist_regex, &config.whitelist_regex)?;
    loop {
        let events = watcher.poll()?;
        for warning in watcher.take_status_messages() {
            eprintln!("{}", warning);
        }
        let (events, _suppressed) = rules.partition_events(events);
        for event in events {
            println!("{}", format_event_line(&event, config.show_timestamps));
        }
        std::thread::sleep(Duration::from_millis(config.poll_interval_ms));
    }
}

fn ensure_cli_default_config(cli: &mut CliArgs) -> Result<()> {
    if cli.config.is_some() {
        return Ok(());
    }

    let config_dir = cli_binary_dir()?;
    if let Some(found) = discover_first_app_config_in_dir(&config_dir)? {
        cli.config = Some(found);
        return Ok(());
    }

    let created = create_default_config_in_dir(&config_dir)?;
    cli.config = Some(created);
    Ok(())
}

fn cli_binary_dir() -> Result<PathBuf> {
    if let Ok(exe_path) = std::env::current_exe()
        && let Some(parent) = exe_path.parent()
    {
        return Ok(parent.to_path_buf());
    }
    std::env::current_dir().context("failed to resolve current working directory")
}

fn discover_first_app_config_in_dir(dir: &Path) -> Result<Option<PathBuf>> {
    let mut candidates = Vec::new();
    let read_dir =
        std::fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))?;
    for entry in read_dir {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if !path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
        {
            continue;
        }
        candidates.push(path);
    }

    candidates.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
    for path in candidates {
        if is_logtrak_app_config_file(&path)? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn is_logtrak_app_config_file(path: &Path) -> Result<bool> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let value: toml::Value = match toml::from_str(&raw) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    let Some(table) = value.as_table() else {
        return Ok(false);
    };

    const APP_KEYS: &[&str] = &[
        "poll_interval_ms",
        "tracked_files",
        "max_buffer_lines",
        "max_line_len",
        "show_timestamps",
        "gui_light_mode",
        "gui_theme",
        "gui_word_wrap",
        "gui_font_size",
        "case_insensitive_text_filter",
        "blacklist_regex",
        "whitelist_regex",
    ];
    if !table.keys().any(|key| APP_KEYS.contains(&key.as_str())) {
        return Ok(false);
    }

    Ok(AppConfig::from_file_allow_empty_files(path).is_ok())
}

fn create_default_config_in_dir(dir: &Path) -> Result<PathBuf> {
    let path = dir.join("default.toml");
    if path.exists() {
        if AppConfig::from_file_allow_empty_files(&path).is_ok() {
            return Ok(path);
        }
        bail!(
            "default config exists but is invalid for this app: {}",
            path.display()
        );
    }

    let config = AppConfig::default();
    config
        .write_to_file(&path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_first_app_config_in_dir_selects_sorted_valid_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("a.toml");
        let second = dir.path().join("z.toml");
        let unrelated = dir.path().join("notes.toml");

        std::fs::write(&second, "poll_interval_ms = 1200\n").expect("write second");
        std::fs::write(&first, "show_timestamps = true\n").expect("write first");
        std::fs::write(&unrelated, "team = \"ops\"\n").expect("write unrelated");

        let found = discover_first_app_config_in_dir(dir.path()).expect("discover");
        assert_eq!(found, Some(first));
    }

    #[test]
    fn create_default_config_in_dir_writes_default_toml() {
        let dir = tempfile::tempdir().expect("tempdir");

        let path = create_default_config_in_dir(dir.path()).expect("create default");
        assert_eq!(path, dir.path().join("default.toml"));

        let config = AppConfig::from_file_allow_empty_files(&path).expect("load default config");
        assert!(config.tracked_files.is_empty());
        assert_eq!(config.poll_interval_ms, 1_000);
    }
}
