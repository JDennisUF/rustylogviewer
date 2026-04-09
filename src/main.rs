mod cli;
mod config;

use anyhow::Result;
use clap::Parser;
use cli::CliArgs;
use config::AppConfig;

fn main() -> Result<()> {
    let cli = CliArgs::parse();
    let config = AppConfig::from_cli(cli)?;
    println!("{}", config.summary_string());
    Ok(())
}
