use clap::{ArgAction, Parser};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "rustylogviewer",
    version,
    about = "Compact multi-file log tail viewer"
)]
pub struct CliArgs {
    /// Optional TOML config file path.
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Poll interval in milliseconds.
    #[arg(long)]
    pub poll_ms: Option<u64>,

    /// Maximum number of recent lines retained in memory.
    #[arg(long)]
    pub max_buffer_lines: Option<usize>,

    /// Maximum line length retained per event.
    #[arg(long)]
    pub max_line_len: Option<usize>,

    /// Force timestamps on displayed lines.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "no_timestamps")]
    pub show_timestamps: bool,

    /// Disable timestamps on displayed lines.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "show_timestamps")]
    pub no_timestamps: bool,

    /// Validate configuration and exit.
    #[arg(long, action = ArgAction::SetTrue)]
    pub print_config_only: bool,

    /// Files to track. If present, overrides `tracked_files` from config.
    #[arg(value_name = "FILE")]
    pub files: Vec<PathBuf>,
}
