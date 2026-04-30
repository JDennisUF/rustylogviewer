use crate::cli::CliArgs;
use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

const DEFAULT_POLL_INTERVAL_MS: u64 = 1_000;
const DEFAULT_MAX_BUFFER_LINES: usize = 10_000;
const DEFAULT_MAX_LINE_LEN: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuiTheme {
    DefaultDark,
    Light,
    HighContrast,
    OceanBlue,
    ShadesOfPurple,
    Novare,
    NovareLight,
    Dracula,
    Nord,
    SolarizedDark,
    SolarizedLight,
    OneDark,
}

impl GuiTheme {
    pub const ALL: [GuiTheme; 12] = [
        GuiTheme::DefaultDark,
        GuiTheme::Light,
        GuiTheme::HighContrast,
        GuiTheme::OceanBlue,
        GuiTheme::ShadesOfPurple,
        GuiTheme::Novare,
        GuiTheme::NovareLight,
        GuiTheme::Dracula,
        GuiTheme::Nord,
        GuiTheme::SolarizedDark,
        GuiTheme::SolarizedLight,
        GuiTheme::OneDark,
    ];

    pub fn all() -> &'static [GuiTheme] {
        &Self::ALL
    }

    pub fn display_name(self) -> &'static str {
        match self {
            GuiTheme::DefaultDark => "Default Dark",
            GuiTheme::Light => "Light",
            GuiTheme::HighContrast => "High Contrast",
            GuiTheme::OceanBlue => "Ocean Blue",
            GuiTheme::ShadesOfPurple => "Shades of Purple",
            GuiTheme::Novare => "Novare Dark",
            GuiTheme::NovareLight => "Novare Light",
            GuiTheme::Dracula => "Dracula",
            GuiTheme::Nord => "Nord",
            GuiTheme::SolarizedDark => "Solarized Dark",
            GuiTheme::SolarizedLight => "Solarized Light",
            GuiTheme::OneDark => "One Dark",
        }
    }

    pub fn config_key(self) -> &'static str {
        match self {
            GuiTheme::DefaultDark => "default_dark",
            GuiTheme::Light => "light",
            GuiTheme::HighContrast => "high_contrast",
            GuiTheme::OceanBlue => "ocean_blue",
            GuiTheme::ShadesOfPurple => "shades_of_purple",
            GuiTheme::Novare => "novare",
            GuiTheme::NovareLight => "novare_light",
            GuiTheme::Dracula => "dracula",
            GuiTheme::Nord => "nord",
            GuiTheme::SolarizedDark => "solarized_dark",
            GuiTheme::SolarizedLight => "solarized_light",
            GuiTheme::OneDark => "one_dark",
        }
    }

    pub fn is_light(self) -> bool {
        matches!(
            self,
            GuiTheme::Light | GuiTheme::SolarizedLight | GuiTheme::NovareLight
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AppConfig {
    pub poll_interval_ms: u64,
    pub tracked_files: Vec<PathBuf>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub tracked_file_enabled: BTreeMap<String, bool>,
    pub max_buffer_lines: usize,
    pub max_line_len: usize,
    pub show_timestamps: bool,
    pub gui_light_mode: bool,
    pub gui_theme: GuiTheme,
    pub gui_word_wrap: bool,
    pub gui_font_size: f32,
    pub case_insensitive_text_filter: bool,
    pub blacklist_regex: Vec<String>,
    pub whitelist_regex: Vec<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
            tracked_files: Vec::new(),
            tracked_file_enabled: BTreeMap::new(),
            max_buffer_lines: DEFAULT_MAX_BUFFER_LINES,
            max_line_len: DEFAULT_MAX_LINE_LEN,
            show_timestamps: true,
            gui_light_mode: false,
            gui_theme: GuiTheme::DefaultDark,
            gui_word_wrap: true,
            gui_font_size: 14.0,
            case_insensitive_text_filter: true,
            blacklist_regex: Vec::new(),
            whitelist_regex: Vec::new(),
        }
    }
}

impl AppConfig {
    pub fn from_cli(cli: &CliArgs) -> Result<Self> {
        let file_cfg = match cli.config.as_ref() {
            Some(path) => Some(load_config_file(path)?),
            None => None,
        };
        let config = merge_config(file_cfg, cli)?;
        config.validate()?;
        Ok(config)
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let cli = CliArgs::default();
        let config = merge_config(Some(load_config_file(path)?), &cli)?;
        config.validate()?;
        Ok(config)
    }

    pub fn from_cli_allow_empty_files(cli: &CliArgs) -> Result<Self> {
        let file_cfg = match cli.config.as_ref() {
            Some(path) => Some(load_config_file(path)?),
            None => None,
        };
        let config = merge_config(file_cfg, cli)?;
        validate_allowing_empty_tracked_files(&config)?;
        Ok(config)
    }

    pub fn from_file_allow_empty_files(path: &Path) -> Result<Self> {
        let cli = CliArgs::default();
        let config = merge_config(Some(load_config_file(path)?), &cli)?;
        validate_allowing_empty_tracked_files(&config)?;
        Ok(config)
    }

    pub fn to_toml_string(&self) -> Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    pub fn write_to_file(&self, path: &Path) -> Result<()> {
        let content = self.to_toml_string()?;
        std::fs::write(path, content)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    pub fn summary_string(&self) -> String {
        let mut summary = String::new();
        let _ = writeln!(summary, "Effective configuration:");
        let _ = writeln!(summary, "  poll_interval_ms: {}", self.poll_interval_ms);
        let _ = writeln!(summary, "  max_buffer_lines: {}", self.max_buffer_lines);
        let _ = writeln!(summary, "  max_line_len: {}", self.max_line_len);
        let _ = writeln!(summary, "  show_timestamps: {}", self.show_timestamps);
        let _ = writeln!(summary, "  gui_light_mode: {}", self.gui_light_mode);
        let _ = writeln!(summary, "  gui_theme: {}", self.gui_theme.config_key());
        let _ = writeln!(summary, "  gui_word_wrap: {}", self.gui_word_wrap);
        let _ = writeln!(summary, "  gui_font_size: {}", self.gui_font_size);
        let _ = writeln!(
            summary,
            "  case_insensitive_text_filter: {}",
            self.case_insensitive_text_filter
        );
        let _ = writeln!(
            summary,
            "  blacklist_regex count: {}",
            self.blacklist_regex.len()
        );
        for pattern in &self.blacklist_regex {
            let _ = writeln!(summary, "    - {}", pattern);
        }
        let _ = writeln!(
            summary,
            "  whitelist_regex count: {}",
            self.whitelist_regex.len()
        );
        for pattern in &self.whitelist_regex {
            let _ = writeln!(summary, "    - {}", pattern);
        }
        let _ = writeln!(summary, "  tracked_files ({}):", self.tracked_files.len());
        for path in &self.tracked_files {
            let _ = writeln!(summary, "    - {}", path.display());
        }
        if !self.tracked_file_enabled.is_empty() {
            let _ = writeln!(
                summary,
                "  tracked_file_enabled ({}):",
                self.tracked_file_enabled.len()
            );
            for (path, enabled) in &self.tracked_file_enabled {
                let _ = writeln!(summary, "    - {} => {}", path, enabled);
            }
        }
        summary
    }

    pub fn tracked_file_enabled_map(&self) -> HashMap<PathBuf, bool> {
        self.tracked_file_enabled
            .iter()
            .map(|(path, enabled)| (PathBuf::from(path), *enabled))
            .collect()
    }

    pub fn set_tracked_file_enabled(&mut self, path: &Path, enabled: bool) {
        self.tracked_file_enabled
            .insert(path.display().to_string(), enabled);
    }

    pub fn validate(&self) -> Result<()> {
        if self.poll_interval_ms == 0 {
            return Err(ConfigValidationError::InvalidPollInterval(self.poll_interval_ms).into());
        }
        if self.max_buffer_lines == 0 {
            return Err(ConfigValidationError::InvalidMaxBufferLines(self.max_buffer_lines).into());
        }
        if self.max_line_len == 0 {
            return Err(ConfigValidationError::InvalidMaxLineLength(self.max_line_len).into());
        }
        if !self.gui_font_size.is_finite() || self.gui_font_size <= 0.0 {
            return Err(ConfigValidationError::InvalidGuiFontSize(self.gui_font_size).into());
        }
        if self.tracked_files.is_empty() {
            return Err(ConfigValidationError::NoTrackedFiles.into());
        }
        for path in &self.tracked_files {
            if path.as_os_str().is_empty() {
                bail!(ConfigValidationError::EmptyPath);
            }
        }
        for pattern in &self.blacklist_regex {
            Regex::new(pattern).map_err(|err| ConfigValidationError::InvalidRegex {
                kind: "blacklist",
                pattern: pattern.clone(),
                message: err.to_string(),
            })?;
        }
        for pattern in &self.whitelist_regex {
            Regex::new(pattern).map_err(|err| ConfigValidationError::InvalidRegex {
                kind: "whitelist",
                pattern: pattern.clone(),
                message: err.to_string(),
            })?;
        }
        Ok(())
    }
}

fn validate_allowing_empty_tracked_files(config: &AppConfig) -> Result<()> {
    match config.validate() {
        Ok(()) => Ok(()),
        Err(err)
            if matches!(
                err.downcast_ref::<ConfigValidationError>(),
                Some(ConfigValidationError::NoTrackedFiles)
            ) =>
        {
            Ok(())
        }
        Err(err) => Err(err),
    }
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    poll_interval_ms: Option<u64>,
    tracked_files: Option<Vec<PathBuf>>,
    tracked_file_enabled: Option<BTreeMap<String, bool>>,
    max_buffer_lines: Option<usize>,
    max_line_len: Option<usize>,
    show_timestamps: Option<bool>,
    gui_light_mode: Option<bool>,
    gui_theme: Option<GuiTheme>,
    gui_word_wrap: Option<bool>,
    gui_font_size: Option<f32>,
    case_insensitive_text_filter: Option<bool>,
    blacklist_regex: Option<Vec<String>>,
    whitelist_regex: Option<Vec<String>>,
}

#[derive(Debug, Error)]
pub enum ConfigValidationError {
    #[error("poll_interval_ms must be > 0, got {0}")]
    InvalidPollInterval(u64),
    #[error("max_buffer_lines must be > 0, got {0}")]
    InvalidMaxBufferLines(usize),
    #[error("max_line_len must be > 0, got {0}")]
    InvalidMaxLineLength(usize),
    #[error("gui_font_size must be > 0 and finite, got {0}")]
    InvalidGuiFontSize(f32),
    #[error("no tracked files configured; pass FILE arguments or set tracked_files in config")]
    NoTrackedFiles,
    #[error("tracked files contains an empty path")]
    EmptyPath,
    #[error("{kind} regex `{pattern}` is invalid: {message}")]
    InvalidRegex {
        kind: &'static str,
        pattern: String,
        message: String,
    },
}

fn load_config_file(path: &Path) -> Result<FileConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let cfg: FileConfig =
        toml::from_str(&raw).with_context(|| format!("invalid TOML in {}", path.display()))?;
    Ok(cfg)
}

fn merge_config(file_cfg: Option<FileConfig>, cli: &CliArgs) -> Result<AppConfig> {
    let mut config = AppConfig::default();

    if let Some(file_cfg) = file_cfg {
        if let Some(v) = file_cfg.poll_interval_ms {
            config.poll_interval_ms = v;
        }
        if let Some(v) = file_cfg.max_buffer_lines {
            config.max_buffer_lines = v;
        }
        if let Some(v) = file_cfg.max_line_len {
            config.max_line_len = v;
        }
        if let Some(v) = file_cfg.show_timestamps {
            config.show_timestamps = v;
        }
        if let Some(v) = file_cfg.gui_light_mode {
            config.gui_light_mode = v;
            config.gui_theme = if v {
                GuiTheme::Light
            } else {
                GuiTheme::DefaultDark
            };
        }
        if let Some(v) = file_cfg.gui_theme {
            config.gui_theme = v;
            config.gui_light_mode = v.is_light();
        }
        if let Some(v) = file_cfg.gui_word_wrap {
            config.gui_word_wrap = v;
        }
        if let Some(v) = file_cfg.gui_font_size {
            config.gui_font_size = v;
        }
        if let Some(v) = file_cfg.case_insensitive_text_filter {
            config.case_insensitive_text_filter = v;
        }
        if let Some(v) = file_cfg.blacklist_regex {
            config.blacklist_regex = v;
        }
        if let Some(v) = file_cfg.whitelist_regex {
            config.whitelist_regex = v;
        }
        if let Some(v) = file_cfg.tracked_files {
            config.tracked_files = v;
        }
        if let Some(v) = file_cfg.tracked_file_enabled {
            config.tracked_file_enabled = v;
        }
    }

    if let Some(v) = cli.poll_ms {
        config.poll_interval_ms = v;
    }
    if let Some(v) = cli.max_buffer_lines {
        config.max_buffer_lines = v;
    }
    if let Some(v) = cli.max_line_len {
        config.max_line_len = v;
    }
    if cli.show_timestamps {
        config.show_timestamps = true;
    }
    if cli.no_timestamps {
        config.show_timestamps = false;
    }
    if cli.case_insensitive_filter {
        config.case_insensitive_text_filter = true;
    }
    if cli.case_sensitive_filter {
        config.case_insensitive_text_filter = false;
    }
    if !cli.blacklist_regex.is_empty() {
        config.blacklist_regex = cli.blacklist_regex.clone();
    }
    if !cli.whitelist_regex.is_empty() {
        config.whitelist_regex = cli.whitelist_regex.clone();
    }
    if !cli.files.is_empty() {
        config.tracked_files = cli.files.clone();
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn write_config(content: &str) -> NamedTempFile {
        let file = NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), content).expect("write config");
        file
    }

    #[test]
    fn file_config_loads_and_validates() {
        let file = write_config(
            r#"
poll_interval_ms = 2000
max_buffer_lines = 7777
max_line_len = 256
show_timestamps = false
gui_light_mode = true
gui_font_size = 16.0
gui_word_wrap = false
case_insensitive_text_filter = false
blacklist_regex = ["DEBUG.*"]
whitelist_regex = ["DEBUG.*critical"]
tracked_files = ["./a.log", "./b.log"]
tracked_file_enabled = { "./a.log" = true, "./b.log" = false }
"#,
        );
        let cli = CliArgs {
            config: Some(file.path().to_path_buf()),
            poll_ms: None,
            max_buffer_lines: None,
            max_line_len: None,
            show_timestamps: false,
            no_timestamps: false,
            print_config_only: false,
            headless: false,
            gui: false,
            case_insensitive_filter: false,
            case_sensitive_filter: false,
            blacklist_regex: Vec::new(),
            whitelist_regex: Vec::new(),
            files: Vec::new(),
        };
        let config = AppConfig::from_cli(&cli).expect("valid config");

        assert_eq!(config.poll_interval_ms, 2000);
        assert_eq!(config.max_buffer_lines, 7777);
        assert_eq!(config.max_line_len, 256);
        assert!(!config.show_timestamps);
        assert!(config.gui_light_mode);
        assert_eq!(config.gui_theme, GuiTheme::Light);
        assert!(!config.gui_word_wrap);
        assert_eq!(config.gui_font_size, 16.0);
        assert!(!config.case_insensitive_text_filter);
        assert_eq!(config.blacklist_regex, vec!["DEBUG.*".to_string()]);
        assert_eq!(config.whitelist_regex, vec!["DEBUG.*critical".to_string()]);
        assert_eq!(config.tracked_files.len(), 2);
        assert_eq!(
            config.tracked_file_enabled.get("./b.log").copied(),
            Some(false)
        );
    }

    #[test]
    fn cli_overrides_file_config() {
        let file = write_config(
            r#"
poll_interval_ms = 2000
max_buffer_lines = 7777
max_line_len = 256
show_timestamps = false
gui_light_mode = true
gui_font_size = 15.0
case_insensitive_text_filter = false
blacklist_regex = ["x"]
whitelist_regex = ["y"]
tracked_files = ["./a.log", "./b.log"]
"#,
        );
        let cli = CliArgs {
            config: Some(file.path().to_path_buf()),
            poll_ms: Some(1500),
            max_buffer_lines: Some(100),
            max_line_len: Some(80),
            show_timestamps: true,
            no_timestamps: false,
            print_config_only: false,
            headless: false,
            gui: false,
            case_insensitive_filter: true,
            case_sensitive_filter: false,
            blacklist_regex: vec!["ERROR".to_string()],
            whitelist_regex: vec!["ERROR keep".to_string()],
            files: vec![PathBuf::from("/tmp/override.log")],
        };
        let config = AppConfig::from_cli(&cli).expect("valid config");

        assert_eq!(config.poll_interval_ms, 1500);
        assert_eq!(config.max_buffer_lines, 100);
        assert_eq!(config.max_line_len, 80);
        assert!(config.show_timestamps);
        assert!(config.gui_light_mode);
        assert_eq!(config.gui_theme, GuiTheme::Light);
        assert!(config.gui_word_wrap);
        assert_eq!(config.gui_font_size, 15.0);
        assert!(config.case_insensitive_text_filter);
        assert_eq!(config.blacklist_regex, vec!["ERROR".to_string()]);
        assert_eq!(config.whitelist_regex, vec!["ERROR keep".to_string()]);
        assert_eq!(
            config.tracked_files,
            vec![PathBuf::from("/tmp/override.log")]
        );
    }

    #[test]
    fn requires_at_least_one_file() {
        let cli = CliArgs {
            config: None,
            poll_ms: None,
            max_buffer_lines: None,
            max_line_len: None,
            show_timestamps: false,
            no_timestamps: false,
            print_config_only: false,
            headless: false,
            gui: false,
            case_insensitive_filter: false,
            case_sensitive_filter: false,
            blacklist_regex: Vec::new(),
            whitelist_regex: Vec::new(),
            files: Vec::new(),
        };

        let err = AppConfig::from_cli(&cli).expect_err("expected missing files validation error");
        assert!(err.to_string().contains("no tracked files configured"));
    }

    #[test]
    fn allow_empty_files_loader_accepts_no_tracked_files() {
        let file = write_config(
            r#"
poll_interval_ms = 1000
max_buffer_lines = 100
max_line_len = 128
show_timestamps = true
"#,
        );
        let cli = CliArgs {
            config: Some(file.path().to_path_buf()),
            poll_ms: None,
            max_buffer_lines: None,
            max_line_len: None,
            show_timestamps: false,
            no_timestamps: false,
            print_config_only: false,
            headless: false,
            gui: false,
            case_insensitive_filter: false,
            case_sensitive_filter: false,
            blacklist_regex: Vec::new(),
            whitelist_regex: Vec::new(),
            files: Vec::new(),
        };

        let config = AppConfig::from_cli_allow_empty_files(&cli).expect("allow empty files");
        assert!(config.tracked_files.is_empty());
    }

    #[test]
    fn rejects_invalid_regex() {
        let cli = CliArgs {
            config: None,
            poll_ms: None,
            max_buffer_lines: None,
            max_line_len: None,
            show_timestamps: false,
            no_timestamps: false,
            print_config_only: false,
            headless: false,
            gui: false,
            case_insensitive_filter: false,
            case_sensitive_filter: false,
            blacklist_regex: vec!["(".to_string()],
            whitelist_regex: Vec::new(),
            files: vec![PathBuf::from("/tmp/app.log")],
        };

        let err = AppConfig::from_cli(&cli).expect_err("invalid regex should fail");
        assert!(err.to_string().contains("regex"));
    }

    #[test]
    fn rejects_invalid_gui_font_size() {
        let file = write_config(
            r#"
poll_interval_ms = 1000
max_buffer_lines = 100
max_line_len = 128
show_timestamps = true
gui_light_mode = false
gui_font_size = 0.0
tracked_files = ["/tmp/app.log"]
"#,
        );
        let cli = CliArgs {
            config: Some(file.path().to_path_buf()),
            poll_ms: None,
            max_buffer_lines: None,
            max_line_len: None,
            show_timestamps: false,
            no_timestamps: false,
            print_config_only: false,
            headless: false,
            gui: false,
            case_insensitive_filter: false,
            case_sensitive_filter: false,
            blacklist_regex: Vec::new(),
            whitelist_regex: Vec::new(),
            files: Vec::new(),
        };

        let err = AppConfig::from_cli(&cli).expect_err("invalid gui font size should fail");
        assert!(err.to_string().contains("gui_font_size"));
    }

    #[test]
    fn gui_theme_overrides_legacy_gui_light_mode() {
        let file = write_config(
            r#"
poll_interval_ms = 1000
max_buffer_lines = 100
max_line_len = 128
show_timestamps = true
gui_light_mode = true
gui_theme = "shades_of_purple"
gui_font_size = 14.0
tracked_files = ["/tmp/app.log"]
"#,
        );
        let cli = CliArgs {
            config: Some(file.path().to_path_buf()),
            poll_ms: None,
            max_buffer_lines: None,
            max_line_len: None,
            show_timestamps: false,
            no_timestamps: false,
            print_config_only: false,
            headless: false,
            gui: false,
            case_insensitive_filter: false,
            case_sensitive_filter: false,
            blacklist_regex: Vec::new(),
            whitelist_regex: Vec::new(),
            files: Vec::new(),
        };

        let config = AppConfig::from_cli(&cli).expect("valid config");
        assert_eq!(config.gui_theme, GuiTheme::ShadesOfPurple);
        assert!(!config.gui_light_mode);
    }

    #[test]
    fn parses_additional_popular_themes() {
        let file = write_config(
            r#"
poll_interval_ms = 1000
max_buffer_lines = 100
max_line_len = 128
show_timestamps = true
gui_theme = "dracula"
gui_font_size = 14.0
tracked_files = ["/tmp/app.log"]
"#,
        );
        let cli = CliArgs {
            config: Some(file.path().to_path_buf()),
            poll_ms: None,
            max_buffer_lines: None,
            max_line_len: None,
            show_timestamps: false,
            no_timestamps: false,
            print_config_only: false,
            headless: false,
            gui: false,
            case_insensitive_filter: false,
            case_sensitive_filter: false,
            blacklist_regex: Vec::new(),
            whitelist_regex: Vec::new(),
            files: Vec::new(),
        };

        let config = AppConfig::from_cli(&cli).expect("valid config");
        assert_eq!(config.gui_theme, GuiTheme::Dracula);
        assert!(!config.gui_light_mode);
    }

    #[test]
    fn novare_theme_parses_and_is_dark_mode_compat() {
        let file = write_config(
            r#"
poll_interval_ms = 1000
max_buffer_lines = 100
max_line_len = 128
show_timestamps = true
gui_theme = "novare"
gui_font_size = 14.0
tracked_files = ["/tmp/app.log"]
"#,
        );
        let cli = CliArgs {
            config: Some(file.path().to_path_buf()),
            poll_ms: None,
            max_buffer_lines: None,
            max_line_len: None,
            show_timestamps: false,
            no_timestamps: false,
            print_config_only: false,
            headless: false,
            gui: false,
            case_insensitive_filter: false,
            case_sensitive_filter: false,
            blacklist_regex: Vec::new(),
            whitelist_regex: Vec::new(),
            files: Vec::new(),
        };

        let config = AppConfig::from_cli(&cli).expect("valid config");
        assert_eq!(config.gui_theme, GuiTheme::Novare);
        assert!(!config.gui_light_mode);
    }

    #[test]
    fn novare_light_theme_sets_light_mode_compat_flag() {
        let file = write_config(
            r#"
poll_interval_ms = 1000
max_buffer_lines = 100
max_line_len = 128
show_timestamps = true
gui_theme = "novare_light"
gui_font_size = 14.0
tracked_files = ["/tmp/app.log"]
"#,
        );
        let cli = CliArgs {
            config: Some(file.path().to_path_buf()),
            poll_ms: None,
            max_buffer_lines: None,
            max_line_len: None,
            show_timestamps: false,
            no_timestamps: false,
            print_config_only: false,
            headless: false,
            gui: false,
            case_insensitive_filter: false,
            case_sensitive_filter: false,
            blacklist_regex: Vec::new(),
            whitelist_regex: Vec::new(),
            files: Vec::new(),
        };

        let config = AppConfig::from_cli(&cli).expect("valid config");
        assert_eq!(config.gui_theme, GuiTheme::NovareLight);
        assert!(config.gui_light_mode);
    }

    #[test]
    fn solarized_light_sets_light_mode_compat_flag() {
        let file = write_config(
            r#"
poll_interval_ms = 1000
max_buffer_lines = 100
max_line_len = 128
show_timestamps = true
gui_theme = "solarized_light"
gui_font_size = 14.0
tracked_files = ["/tmp/app.log"]
"#,
        );
        let cli = CliArgs {
            config: Some(file.path().to_path_buf()),
            poll_ms: None,
            max_buffer_lines: None,
            max_line_len: None,
            show_timestamps: false,
            no_timestamps: false,
            print_config_only: false,
            headless: false,
            gui: false,
            case_insensitive_filter: false,
            case_sensitive_filter: false,
            blacklist_regex: Vec::new(),
            whitelist_regex: Vec::new(),
            files: Vec::new(),
        };

        let config = AppConfig::from_cli(&cli).expect("valid config");
        assert_eq!(config.gui_theme, GuiTheme::SolarizedLight);
        assert!(config.gui_light_mode);
    }
}
