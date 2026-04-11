use clap::{ArgAction, Parser};
use std::path::PathBuf;

#[derive(Debug, Default, Parser)]
#[command(version, about = "Compact multi-file log tail viewer")]
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
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "gui")]
    pub print_config_only: bool,

    /// Run without TUI and print events to stdout.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "gui")]
    pub headless: bool,

    /// Run with graphical desktop UI.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "headless")]
    pub gui: bool,

    /// Match text filters case-insensitively (default behavior).
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "case_sensitive_filter")]
    pub case_insensitive_filter: bool,

    /// Match text filters case-sensitively.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "case_insensitive_filter")]
    pub case_sensitive_filter: bool,

    /// Regex pattern to suppress matching lines. Can be passed multiple times.
    #[arg(long = "blacklist-regex", value_name = "REGEX")]
    pub blacklist_regex: Vec<String>,

    /// Regex pattern to force-keep matching lines, even if blacklisted.
    #[arg(long = "whitelist-regex", value_name = "REGEX")]
    pub whitelist_regex: Vec<String>,

    /// Files to track. If present, overrides `tracked_files` from config.
    #[arg(value_name = "FILE")]
    pub files: Vec<PathBuf>,
}
