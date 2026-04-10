#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use anyhow::{Result, bail};
use clap::Parser;
use rustylogviewer::cli::CliArgs;
use rustylogviewer::gui;

fn main() -> Result<()> {
    let cli = CliArgs::parse();

    if cli.headless || cli.print_config_only {
        bail!("rustylogviewer.exe supports GUI mode only; use rustylogviewer-cli.exe for CLI options");
    }

    gui::run_gui(cli.config)
}
