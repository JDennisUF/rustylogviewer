#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use anyhow::{Result, bail};
use clap::Parser;
use logtrak::cli::CliArgs;
use logtrak::gui;

fn main() -> Result<()> {
    let cli = CliArgs::parse();

    if cli.headless || cli.print_config_only {
        bail!("logtrak.exe supports GUI mode only; use logtrak-cli.exe for CLI options");
    }

    gui::run_gui(cli.config)
}
