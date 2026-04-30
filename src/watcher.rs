use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use time::{Date, Month, OffsetDateTime};

const TODAY_FORMAT_NAMES: &[&str] = &[
    "yyyymmdd",
    "yyyy-mm-dd",
    "yyyy_mm_dd",
    "mmdd",
    "mm-dd",
    "mm_dd",
    "yymmdd",
    "yy-mm-dd",
    "yy_mm_dd",
];
const DATE_PLACEHOLDERS: &[&str] = &[
    "{today}",
    "{yyyymmdd}",
    "{yyyy-mm-dd}",
    "{yyyy_mm_dd}",
    "{yymmdd}",
    "{yy-mm-dd}",
    "{yy_mm_dd}",
    "{mmdd}",
    "{mm-dd}",
    "{mm_dd}",
    "{yyyy}",
    "{yy}",
    "{mm}",
    "{dd}",
];
const MAX_WILDCARD_MATCHES: usize = 200;

#[derive(Debug, Clone)]
pub struct LogEvent {
    pub ts: SystemTime,
    pub source: String,
    pub line: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedResolvedFileDescriptor {
    pub path: PathBuf,
    pub source_label: String,
    pub enabled: bool,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedPathDescriptor {
    pub raw_path: PathBuf,
    pub is_dynamic: bool,
    pub resolved_files: Vec<TrackedResolvedFileDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodayPatternSuggestion {
    pub original_path: PathBuf,
    pub pattern_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PollingWatcher {
    tracked_files: Vec<TrackedPathState>,
    max_line_len: usize,
    file_enabled: HashMap<PathBuf, bool>,
    unreadable_files: HashMap<PathBuf, String>,
    status_messages: Vec<String>,
}

impl PollingWatcher {
    pub fn new(paths: Vec<PathBuf>, max_line_len: usize) -> Result<Self> {
        Self::with_file_enabled(paths, HashMap::new(), max_line_len)
    }

    pub fn with_file_enabled(
        paths: Vec<PathBuf>,
        file_enabled: HashMap<PathBuf, bool>,
        max_line_len: usize,
    ) -> Result<Self> {
        let mut tracked_files = Vec::with_capacity(paths.len());
        for path in paths {
            tracked_files.push(TrackedPathState::new(path)?);
        }
        let mut unreadable_files = HashMap::new();
        let mut status_messages = Vec::new();
        let today = current_local_date();
        for state in &mut tracked_files {
            state.sync_active_files(
                today,
                &file_enabled,
                &mut unreadable_files,
                &mut status_messages,
            )?;
            state.last_resolution_warnings.clear();
        }
        status_messages.clear();
        Ok(Self {
            tracked_files,
            max_line_len,
            file_enabled,
            unreadable_files,
            status_messages,
        })
    }

    pub fn poll(&mut self) -> Result<Vec<LogEvent>> {
        self.status_messages.clear();
        let mut out = Vec::new();
        let today = current_local_date();
        let file_enabled = &self.file_enabled;
        for state in &mut self.tracked_files {
            state.poll(
                today,
                self.max_line_len,
                file_enabled,
                &mut out,
                &mut self.unreadable_files,
                &mut self.status_messages,
            )?;
        }
        Ok(out)
    }

    pub fn take_status_messages(&mut self) -> Vec<String> {
        std::mem::take(&mut self.status_messages)
    }

    pub fn active_sources(&self) -> Vec<String> {
        let mut sources = self
            .tracked_files
            .iter()
            .flat_map(|state| {
                state
                    .active_files
                    .iter()
                    .filter(|file| file_is_present(&file.path))
                    .map(|file| file.source_label.clone())
            })
            .collect::<Vec<_>>();
        sources.sort();
        sources.dedup();
        sources
    }

    pub fn active_file_paths(&self) -> Vec<String> {
        let mut paths = self
            .tracked_files
            .iter()
            .flat_map(|state| {
                state
                    .active_files
                    .iter()
                    .filter(|file| file_is_present(&file.path))
                    .map(|file| file.path.display().to_string())
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
    }

    pub fn set_file_enabled(&mut self, path: &Path, enabled: bool) -> Result<()> {
        self.file_enabled.insert(path.to_path_buf(), enabled);
        let today = current_local_date();
        let file_enabled = &self.file_enabled;
        for state in &mut self.tracked_files {
            state.sync_active_files(
                today,
                file_enabled,
                &mut self.unreadable_files,
                &mut self.status_messages,
            )?;
        }
        Ok(())
    }

    pub fn tracked_path_descriptors(&self) -> Vec<TrackedPathDescriptor> {
        self.tracked_files
            .iter()
            .map(|state| state.descriptor(&self.file_enabled))
            .collect()
    }

    pub fn add_file(&mut self, path: PathBuf) -> Result<bool> {
        if self
            .tracked_files
            .iter()
            .any(|state| state.raw_path == path)
        {
            return Ok(false);
        }
        self.tracked_files.push(TrackedPathState::new(path)?);
        Ok(true)
    }
}

pub fn describe_tracked_paths(
    raw_paths: &[PathBuf],
    file_enabled: &HashMap<PathBuf, bool>,
) -> Result<Vec<TrackedPathDescriptor>> {
    raw_paths
        .iter()
        .cloned()
        .map(|raw_path| {
            let state = TrackedPathState::new(raw_path)?;
            Ok(state.descriptor(file_enabled))
        })
        .collect()
}

pub fn suggest_today_pattern(path: &Path) -> Option<TodayPatternSuggestion> {
    suggest_today_pattern_for_date(path)
}

#[derive(Debug, Clone)]
struct TrackedPathState {
    raw_path: PathBuf,
    resolver: PathResolver,
    active_files: Vec<TrackedFileState>,
    last_resolution_warnings: HashSet<String>,
}

impl TrackedPathState {
    fn new(raw_path: PathBuf) -> Result<Self> {
        let resolver = PathResolver::new(&raw_path);
        let active_files = resolver
            .resolve_paths_for_date(current_local_date())
            .paths
            .into_iter()
            .map(TrackedFileState::new)
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            raw_path,
            resolver,
            active_files,
            last_resolution_warnings: HashSet::new(),
        })
    }

    fn poll(
        &mut self,
        today: Date,
        max_line_len: usize,
        file_enabled: &HashMap<PathBuf, bool>,
        out: &mut Vec<LogEvent>,
        unreadable_files: &mut HashMap<PathBuf, String>,
        status_messages: &mut Vec<String>,
    ) -> Result<()> {
        self.sync_active_files(today, file_enabled, unreadable_files, status_messages)?;

        for state in &mut self.active_files {
            match state.poll(max_line_len, out) {
                Ok(()) => {
                    if unreadable_files.remove(&state.path).is_some() {
                        status_messages
                            .push(format!("File readable again: {}", state.path.display()));
                    }
                }
                Err(err) => {
                    let message = err.to_string();
                    let previous = unreadable_files.insert(state.path.clone(), message.clone());
                    if previous.as_ref() != Some(&message) {
                        status_messages.push(format!(
                            "Ignoring unreadable file {}: {}",
                            state.path.display(),
                            message
                        ));
                    }
                }
            }
        }

        Ok(())
    }

    fn sync_active_files(
        &mut self,
        today: Date,
        file_enabled: &HashMap<PathBuf, bool>,
        unreadable_files: &mut HashMap<PathBuf, String>,
        status_messages: &mut Vec<String>,
    ) -> Result<()> {
        let resolved = self.resolver.resolve_paths_for_date(today);
        self.emit_resolution_warnings(&resolved.warnings, status_messages);
        let resolved_paths = resolved.paths;
        let resolved_set: HashSet<PathBuf> = resolved_paths.iter().cloned().collect();

        for state in &self.active_files {
            if !resolved_set.contains(&state.path) {
                unreadable_files.remove(&state.path);
            }
        }

        let mut existing_files = std::mem::take(&mut self.active_files);
        let mut synced_files = Vec::with_capacity(resolved_paths.len());
        let keep_rollover_files = self.resolver.is_date_sensitive();

        for path in resolved_paths {
            if !is_file_enabled(file_enabled, &path) {
                unreadable_files.remove(&path);
                if let Some(existing_idx) =
                    existing_files.iter().position(|state| state.path == path)
                {
                    existing_files.remove(existing_idx);
                }
                continue;
            }
            if let Some(existing_idx) = existing_files.iter().position(|state| state.path == path) {
                synced_files.push(existing_files.remove(existing_idx));
            } else {
                synced_files.push(TrackedFileState::new(path)?);
            }
        }

        for state in existing_files {
            if keep_rollover_files
                && is_file_enabled(file_enabled, &state.path)
                && should_keep_rollover_file(&state.path)
            {
                synced_files.push(state);
            } else {
                unreadable_files.remove(&state.path);
            }
        }

        self.active_files = synced_files;
        Ok(())
    }

    fn emit_resolution_warnings(&mut self, warnings: &[String], status_messages: &mut Vec<String>) {
        let current_warnings: HashSet<String> = warnings.iter().cloned().collect();
        for warning in warnings {
            if !self.last_resolution_warnings.contains(warning) {
                status_messages.push(warning.clone());
            }
        }
        self.last_resolution_warnings = current_warnings;
    }

    fn descriptor(&self, file_enabled: &HashMap<PathBuf, bool>) -> TrackedPathDescriptor {
        let today = current_local_date();
        let is_dynamic = self.resolver.is_dynamic();
        let mut resolved_paths = self
            .resolver
            .resolve_paths_for_date(today)
            .paths
            .into_iter()
            .filter(|path| !is_dynamic || file_is_present(path))
            .collect::<Vec<_>>();
        for active in &self.active_files {
            if file_is_present(&active.path)
                && !resolved_paths.iter().any(|path| path == &active.path)
            {
                resolved_paths.push(active.path.clone());
            }
        }
        resolved_paths.sort();
        resolved_paths.dedup();

        let resolved_files = resolved_paths
            .into_iter()
            .map(|path| TrackedResolvedFileDescriptor {
                source_label: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
                enabled: is_file_enabled(file_enabled, &path),
                active: self.active_files.iter().any(|state| state.path == path),
                path,
            })
            .collect();

        TrackedPathDescriptor {
            raw_path: self.raw_path.clone(),
            is_dynamic,
            resolved_files,
        }
    }
}

#[derive(Debug, Clone)]
struct PathResolver {
    raw: String,
}

impl PathResolver {
    fn new(path: &Path) -> Self {
        Self {
            raw: path.to_string_lossy().into_owned(),
        }
    }

    fn resolve_paths_for_date(&self, date: Date) -> ResolvedPaths {
        let tokens = DateTokens::for_date(date);
        let base_candidates = if self.raw.contains("{today}") {
            TODAY_FORMAT_NAMES
                .iter()
                .map(|format_name| self.raw.replace("{today}", tokens.value_for(format_name)))
                .collect::<Vec<_>>()
        } else {
            vec![self.raw.clone()]
        };

        let mut seen = HashSet::new();
        let mut resolved = Vec::with_capacity(base_candidates.len());
        let mut warnings = Vec::new();

        for candidate in base_candidates {
            let rendered = tokens.apply(&candidate);
            for path in expand_wildcard_pattern(&rendered, &mut warnings) {
                if seen.insert(path.clone()) {
                    resolved.push(path);
                }
            }
        }

        ResolvedPaths {
            paths: resolved,
            warnings,
        }
    }

    fn is_date_sensitive(&self) -> bool {
        DATE_PLACEHOLDERS
            .iter()
            .any(|placeholder| self.raw.contains(placeholder))
    }

    fn is_dynamic(&self) -> bool {
        self.is_date_sensitive() || contains_wildcards(&self.raw)
    }
}

#[derive(Debug, Clone)]
struct ResolvedPaths {
    paths: Vec<PathBuf>,
    warnings: Vec<String>,
}

fn expand_wildcard_pattern(raw: &str, warnings: &mut Vec<String>) -> Vec<PathBuf> {
    if raw.contains("**") {
        warnings.push(format!(
            "Ignoring tracked pattern {}: recursive wildcards (`**`) are not supported",
            raw
        ));
        return Vec::new();
    }

    if !contains_wildcards(raw) {
        return vec![PathBuf::from(raw)];
    }

    let path = Path::new(raw);
    let Some(file_name_pattern) = path.file_name().and_then(|name| name.to_str()) else {
        warnings.push(format!(
            "Ignoring tracked pattern {}: wildcard patterns must end in a filename segment",
            raw
        ));
        return Vec::new();
    };

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if contains_wildcards_in_path(parent) {
        warnings.push(format!(
            "Ignoring tracked pattern {}: wildcards are only supported in the final path segment",
            raw
        ));
        return Vec::new();
    }

    let read_dir = match std::fs::read_dir(parent) {
        Ok(read_dir) => read_dir,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            warnings.push(format!(
                "Ignoring tracked pattern {}: failed to read {}: {}",
                raw,
                parent.display(),
                err
            ));
            return Vec::new();
        }
    };

    let mut matched = Vec::new();
    let mut capped = false;
    for entry in read_dir {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let entry_path = entry.path();
        if !entry_path.is_file() {
            continue;
        }
        let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !wildcard_match(file_name_pattern, &file_name) {
            continue;
        }
        if matched.len() < MAX_WILDCARD_MATCHES {
            matched.push(entry_path);
        } else {
            capped = true;
        }
    }

    matched.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
    if capped {
        warnings.push(format!(
            "Tracked pattern {} matched more than {} files; using the first {} matches",
            raw, MAX_WILDCARD_MATCHES, MAX_WILDCARD_MATCHES
        ));
    }

    matched
}

fn should_keep_rollover_file(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(_) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

fn file_is_present(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

fn is_file_enabled(file_enabled: &HashMap<PathBuf, bool>, path: &Path) -> bool {
    file_enabled.get(path).copied().unwrap_or(true)
}

fn contains_wildcards(raw: &str) -> bool {
    raw.contains('*') || raw.contains('?')
}

fn contains_wildcards_in_path(path: &Path) -> bool {
    path.components()
        .any(|component| contains_wildcards(&component.as_os_str().to_string_lossy()))
}

fn wildcard_match(pattern: &str, candidate: &str) -> bool {
    let pattern = pattern.as_bytes();
    let candidate = candidate.as_bytes();

    let (mut pattern_idx, mut candidate_idx) = (0usize, 0usize);
    let mut star_idx = None;
    let mut match_idx = 0usize;

    while candidate_idx < candidate.len() {
        if pattern_idx < pattern.len()
            && (pattern[pattern_idx] == b'?' || pattern[pattern_idx] == candidate[candidate_idx])
        {
            pattern_idx += 1;
            candidate_idx += 1;
        } else if pattern_idx < pattern.len() && pattern[pattern_idx] == b'*' {
            star_idx = Some(pattern_idx);
            match_idx = candidate_idx;
            pattern_idx += 1;
        } else if let Some(star) = star_idx {
            pattern_idx = star + 1;
            match_idx += 1;
            candidate_idx = match_idx;
        } else {
            return false;
        }
    }

    while pattern_idx < pattern.len() && pattern[pattern_idx] == b'*' {
        pattern_idx += 1;
    }

    pattern_idx == pattern.len()
}

#[derive(Debug, Clone)]
struct DateTokens {
    yyyy: String,
    yy: String,
    mm: String,
    dd: String,
    yyyymmdd: String,
    yyyy_mm_dd: String,
    yyyy_dash_mm_dash_dd: String,
    yymmdd: String,
    yy_mm_dd: String,
    yy_dash_mm_dash_dd: String,
    mmdd: String,
    mm_dd: String,
    mm_dash_dd: String,
}

impl DateTokens {
    fn for_date(date: Date) -> Self {
        let year = date.year();
        let short_year = (year % 100).abs();
        let month = month_number(date.month());
        let day = date.day();

        let yyyy = format!("{year:04}");
        let yy = format!("{short_year:02}");
        let mm = format!("{month:02}");
        let dd = format!("{day:02}");

        Self {
            yyyy: yyyy.clone(),
            yy: yy.clone(),
            mm: mm.clone(),
            dd: dd.clone(),
            yyyymmdd: format!("{yyyy}{mm}{dd}"),
            yyyy_mm_dd: format!("{yyyy}_{mm}_{dd}"),
            yyyy_dash_mm_dash_dd: format!("{yyyy}-{mm}-{dd}"),
            yymmdd: format!("{yy}{mm}{dd}"),
            yy_mm_dd: format!("{yy}_{mm}_{dd}"),
            yy_dash_mm_dash_dd: format!("{yy}-{mm}-{dd}"),
            mmdd: format!("{mm}{dd}"),
            mm_dd: format!("{mm}_{dd}"),
            mm_dash_dd: format!("{mm}-{dd}"),
        }
    }

    fn apply(&self, raw: &str) -> String {
        [
            ("{yyyymmdd}", self.yyyymmdd.as_str()),
            ("{yyyy-mm-dd}", self.yyyy_dash_mm_dash_dd.as_str()),
            ("{yyyy_mm_dd}", self.yyyy_mm_dd.as_str()),
            ("{yymmdd}", self.yymmdd.as_str()),
            ("{yy-mm-dd}", self.yy_dash_mm_dash_dd.as_str()),
            ("{yy_mm_dd}", self.yy_mm_dd.as_str()),
            ("{mmdd}", self.mmdd.as_str()),
            ("{mm-dd}", self.mm_dash_dd.as_str()),
            ("{mm_dd}", self.mm_dd.as_str()),
            ("{yyyy}", self.yyyy.as_str()),
            ("{yy}", self.yy.as_str()),
            ("{mm}", self.mm.as_str()),
            ("{dd}", self.dd.as_str()),
        ]
        .into_iter()
        .fold(raw.to_string(), |rendered, (token, value)| {
            rendered.replace(token, value)
        })
    }

    fn value_for(&self, format_name: &str) -> &str {
        match format_name {
            "yyyymmdd" => &self.yyyymmdd,
            "yyyy-mm-dd" => &self.yyyy_dash_mm_dash_dd,
            "yyyy_mm_dd" => &self.yyyy_mm_dd,
            "yymmdd" => &self.yymmdd,
            "yy-mm-dd" => &self.yy_dash_mm_dash_dd,
            "yy_mm_dd" => &self.yy_mm_dd,
            "mmdd" => &self.mmdd,
            "mm-dd" => &self.mm_dash_dd,
            "mm_dd" => &self.mm_dd,
            _ => &self.yyyymmdd,
        }
    }
}

fn current_local_date() -> Date {
    OffsetDateTime::now_local()
        .unwrap_or_else(|_| OffsetDateTime::now_utc())
        .date()
}

fn month_number(month: Month) -> u8 {
    month as u8
}

fn suggest_today_pattern_for_date(path: &Path) -> Option<TodayPatternSuggestion> {
    let file_name = path.file_name()?.to_str()?;
    let stem = path.file_stem()?.to_str()?;
    let extension = path.extension().and_then(|ext| ext.to_str());
    if file_name.contains("{today}")
        || DATE_PLACEHOLDERS
            .iter()
            .any(|token| file_name.contains(token))
    {
        return None;
    }

    let suffix_len = recognized_date_suffix_len(stem)?;
    let prefix = stem.get(..stem.len().checked_sub(suffix_len)?)?;
    if prefix.is_empty() && extension.is_none() {
        return None;
    }

    let mut suggested_name = format!("{prefix}{{today}}");
    if let Some(extension) = extension {
        suggested_name.push('.');
        suggested_name.push_str(extension);
    }
    let pattern_path = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(
            || PathBuf::from(&suggested_name),
            |parent| parent.join(&suggested_name),
        );
    if pattern_path != path {
        return Some(TodayPatternSuggestion {
            original_path: path.to_path_buf(),
            pattern_path,
        });
    }

    None
}

fn recognized_date_suffix_len(stem: &str) -> Option<usize> {
    [
        (10usize, DateSuffixFormat::YearMonthDay('-')),
        (10, DateSuffixFormat::YearMonthDay('_')),
        (8, DateSuffixFormat::YearMonthDayCompact),
        (8, DateSuffixFormat::ShortYearMonthDay('-')),
        (8, DateSuffixFormat::ShortYearMonthDay('_')),
        (6, DateSuffixFormat::ShortYearMonthDayCompact),
        (5, DateSuffixFormat::MonthDay('-')),
        (5, DateSuffixFormat::MonthDay('_')),
        (4, DateSuffixFormat::MonthDayCompact),
    ]
    .into_iter()
    .find_map(|(len, format)| {
        stem.get(stem.len().checked_sub(len)?..)
            .filter(|suffix| format.is_valid(suffix))
            .map(|_| len)
    })
}

#[derive(Debug, Clone, Copy)]
enum DateSuffixFormat {
    YearMonthDay(char),
    YearMonthDayCompact,
    ShortYearMonthDay(char),
    ShortYearMonthDayCompact,
    MonthDay(char),
    MonthDayCompact,
}

impl DateSuffixFormat {
    fn is_valid(self, value: &str) -> bool {
        match self {
            Self::YearMonthDay(separator) => parse_ymd(value, separator)
                .is_some_and(|(year, month, day)| is_valid_year_month_day(year, month, day)),
            Self::YearMonthDayCompact => parse_digits(value, &[4, 2, 2]).is_some_and(|parts| {
                is_valid_year_month_day(parts[0], parts[1] as u8, parts[2] as u8)
            }),
            Self::ShortYearMonthDay(separator) => {
                parse_ymd(value, separator).is_some_and(|(year, month, day)| {
                    is_valid_year_month_day(2000 + year.rem_euclid(100), month, day)
                })
            }
            Self::ShortYearMonthDayCompact => {
                parse_digits(value, &[2, 2, 2]).is_some_and(|parts| {
                    is_valid_year_month_day(2000 + parts[0], parts[1] as u8, parts[2] as u8)
                })
            }
            Self::MonthDay(separator) => parse_md(value, separator)
                .is_some_and(|(month, day)| is_valid_month_day(month, day)),
            Self::MonthDayCompact => parse_digits(value, &[2, 2])
                .is_some_and(|parts| is_valid_month_day(parts[0] as u8, parts[1] as u8)),
        }
    }
}

fn parse_ymd(value: &str, separator: char) -> Option<(i32, u8, u8)> {
    let mut parts = value.split(separator);
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((year, month, day))
}

fn parse_md(value: &str, separator: char) -> Option<(u8, u8)> {
    let mut parts = value.split(separator);
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((month, day))
}

fn parse_digits(value: &str, widths: &[usize]) -> Option<Vec<i32>> {
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let expected_len = widths.iter().sum::<usize>();
    if value.len() != expected_len {
        return None;
    }
    let mut start = 0;
    let mut parts = Vec::with_capacity(widths.len());
    for width in widths {
        let end = start + width;
        parts.push(value.get(start..end)?.parse().ok()?);
        start = end;
    }
    Some(parts)
}

fn is_valid_year_month_day(year: i32, month: u8, day: u8) -> bool {
    let Ok(month) = Month::try_from(month) else {
        return false;
    };
    Date::from_calendar_date(year, month, day).is_ok()
}

fn is_valid_month_day(month: u8, day: u8) -> bool {
    matches!(
        (month, day),
        (1 | 3 | 5 | 7 | 8 | 10 | 12, 1..=31) | (4 | 6 | 9 | 11, 1..=30) | (2, 1..=29)
    )
}

#[derive(Debug, Clone)]
struct TrackedFileState {
    path: PathBuf,
    source_label: String,
    file_id: Option<FileIdentity>,
    read_offset: u64,
    last_modified: Option<SystemTime>,
    partial_line: Vec<u8>,
}

impl TrackedFileState {
    fn new(path: PathBuf) -> Result<Self> {
        let source_label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());

        let (file_id, read_offset, last_modified) = match std::fs::metadata(&path) {
            Ok(meta) => (
                FileIdentity::from_metadata(&meta),
                meta.len(),
                meta.modified().ok(),
            ),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (None, 0, None),
            Err(err) => {
                return Err(err).with_context(|| format!("failed to stat {}", path.display()));
            }
        };

        Ok(Self {
            path,
            source_label,
            file_id,
            read_offset,
            last_modified,
            partial_line: Vec::new(),
        })
    }

    fn poll(&mut self, max_line_len: usize, out: &mut Vec<LogEvent>) -> Result<()> {
        let meta = match std::fs::metadata(&self.path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                self.file_id = None;
                self.read_offset = 0;
                self.last_modified = None;
                self.partial_line.clear();
                return Ok(());
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("failed to stat tracked file {}", self.path.display())
                });
            }
        };

        let current_file_id = FileIdentity::from_metadata(&meta);
        let current_modified = meta.modified().ok();
        let file_size = meta.len();
        let file_replaced = self.file_id.is_some() && self.file_id != current_file_id;
        let same_size_but_modified = file_size == self.read_offset
            && matches!(
                (self.last_modified, current_modified),
                (Some(previous), Some(current)) if current > previous
            );
        if file_replaced || file_size < self.read_offset || same_size_but_modified {
            self.read_offset = 0;
            self.partial_line.clear();
        }
        self.file_id = current_file_id;
        self.last_modified = current_modified;

        if file_size <= self.read_offset {
            return Ok(());
        }

        let mut file = File::open(&self.path)
            .with_context(|| format!("failed to open tracked file {}", self.path.display()))?;
        file.seek(SeekFrom::Start(self.read_offset))
            .with_context(|| format!("failed to seek tracked file {}", self.path.display()))?;

        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .with_context(|| format!("failed to read tracked file {}", self.path.display()))?;
        if buf.is_empty() {
            return Ok(());
        }
        self.read_offset += buf.len() as u64;

        self.emit_events(buf, max_line_len, out);
        Ok(())
    }

    fn emit_events(&mut self, bytes: Vec<u8>, max_line_len: usize, out: &mut Vec<LogEvent>) {
        let mut combined = std::mem::take(&mut self.partial_line);
        combined.extend_from_slice(&bytes);

        let mut start = 0usize;
        for (idx, byte) in combined.iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }

            let mut line = combined[start..idx].to_vec();
            if line.last().copied() == Some(b'\r') {
                line.pop();
            }

            let mut text = String::from_utf8_lossy(&line).into_owned();
            if text.len() > max_line_len {
                text.truncate(max_line_len);
            }

            out.push(LogEvent {
                ts: SystemTime::now(),
                source: self.source_label.clone(),
                line: text,
            });

            start = idx + 1;
        }

        self.partial_line = combined[start..].to_vec();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

impl FileIdentity {
    fn from_metadata(meta: &Metadata) -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            return Some(Self {
                dev: meta.dev(),
                ino: meta.ino(),
            });
        }
        #[cfg(not(unix))]
        {
            let _ = meta;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn sample_date() -> Date {
        Date::from_calendar_date(2026, Month::April, 24).expect("valid date")
    }

    fn next_sample_date() -> Date {
        Date::from_calendar_date(2026, Month::April, 25).expect("valid date")
    }

    #[test]
    fn explicit_date_tokens_render_to_todays_filename() {
        let resolver = PathResolver::new(Path::new("/tmp/app_{yyyy}{mm}{dd}.log"));

        let resolved = resolver.resolve_paths_for_date(sample_date());

        assert!(resolved.warnings.is_empty());
        assert_eq!(resolved.paths, vec![PathBuf::from("/tmp/app_20260424.log")]);
    }

    #[test]
    fn today_placeholder_expands_to_common_date_formats() {
        let resolver = PathResolver::new(Path::new("/tmp/app_{today}.log"));

        let resolved = resolver.resolve_paths_for_date(sample_date());

        assert!(resolved.warnings.is_empty());
        assert_eq!(
            resolved.paths,
            vec![
                PathBuf::from("/tmp/app_20260424.log"),
                PathBuf::from("/tmp/app_2026-04-24.log"),
                PathBuf::from("/tmp/app_2026_04_24.log"),
                PathBuf::from("/tmp/app_0424.log"),
                PathBuf::from("/tmp/app_04-24.log"),
                PathBuf::from("/tmp/app_04_24.log"),
                PathBuf::from("/tmp/app_260424.log"),
                PathBuf::from("/tmp/app_26-04-24.log"),
                PathBuf::from("/tmp/app_26_04_24.log"),
            ]
        );
    }

    #[test]
    fn suggest_today_pattern_replaces_recognized_date_suffix() {
        let path = PathBuf::from("/tmp/app_20260424.log");

        let suggestion = suggest_today_pattern_for_date(&path).expect("today suggestion");

        assert_eq!(suggestion.original_path, path);
        assert_eq!(
            suggestion.pattern_path,
            PathBuf::from("/tmp/app_{today}.log")
        );
    }

    #[test]
    fn suggest_today_pattern_supports_short_date_suffixes() {
        let path = PathBuf::from("/tmp/app-0424.log");

        let suggestion = suggest_today_pattern_for_date(&path).expect("today suggestion");

        assert_eq!(
            suggestion.pattern_path,
            PathBuf::from("/tmp/app-{today}.log")
        );
    }

    #[test]
    fn suggest_today_pattern_supports_valid_non_today_suffixes() {
        let path = PathBuf::from("/tmp/app_04_26.log");

        let suggestion = suggest_today_pattern_for_date(&path).expect("today suggestion");

        assert_eq!(
            suggestion.pattern_path,
            PathBuf::from("/tmp/app_{today}.log")
        );
    }

    #[test]
    fn suggest_today_pattern_ignores_invalid_date_suffix() {
        let path = PathBuf::from("/tmp/app_20261340.log");

        assert_eq!(suggest_today_pattern_for_date(&path), None);
    }

    #[test]
    fn wildcard_matches_files_in_final_segment_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("app-1.log"), "one\n").expect("write app-1");
        std::fs::write(dir.path().join("app-2.log"), "two\n").expect("write app-2");
        std::fs::write(dir.path().join("other.log"), "other\n").expect("write other");

        let resolver = PathResolver::new(&dir.path().join("app-?.log"));
        let resolved = resolver.resolve_paths_for_date(sample_date());

        assert!(resolved.warnings.is_empty());
        assert_eq!(
            resolved.paths,
            vec![dir.path().join("app-1.log"), dir.path().join("app-2.log")]
        );
    }

    #[test]
    fn recursive_wildcards_are_rejected_with_warning() {
        let resolver = PathResolver::new(Path::new("/tmp/**/app.log"));
        let resolved = resolver.resolve_paths_for_date(sample_date());

        assert!(resolved.paths.is_empty());
        assert_eq!(resolved.warnings.len(), 1);
        assert!(resolved.warnings[0].contains("recursive wildcards"));
    }

    #[test]
    fn wildcard_match_cap_emits_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        for idx in 0..(MAX_WILDCARD_MATCHES + 5) {
            std::fs::write(dir.path().join(format!("app-{idx:03}.log")), "x\n")
                .expect("write wildcard fixture");
        }

        let resolver = PathResolver::new(&dir.path().join("app-*.log"));
        let resolved = resolver.resolve_paths_for_date(sample_date());

        assert_eq!(resolved.paths.len(), MAX_WILDCARD_MATCHES);
        assert_eq!(resolved.warnings.len(), 1);
        assert!(resolved.warnings[0].contains("matched more than"));
    }

    #[test]
    fn starts_at_end_and_only_reads_new_lines() {
        let mut file = NamedTempFile::new().expect("temp file");
        writeln!(file, "old-line").expect("write");

        let mut watcher =
            PollingWatcher::new(vec![file.path().to_path_buf()], 512).expect("watcher");

        let initial = watcher.poll().expect("poll");
        assert!(initial.is_empty());

        writeln!(file, "new-line").expect("write");
        let events = watcher.poll().expect("poll");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].line, "new-line");
    }

    #[test]
    fn handles_truncate_and_reads_after_reset() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut watcher = PollingWatcher::new(vec![path.clone()], 512).expect("watcher");

        writeln!(file, "line-1").expect("write");
        let events = watcher.poll().expect("poll");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].line, "line-1");

        file.as_file_mut().set_len(0).expect("truncate");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("seek");
        writeln!(file, "line-2").expect("write");
        file.as_file_mut().flush().expect("flush");

        let events = watcher.poll().expect("poll");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].line, "line-2");
    }

    #[test]
    fn waits_for_newline_before_emitting() {
        let mut file = NamedTempFile::new().expect("temp file");
        let mut watcher =
            PollingWatcher::new(vec![file.path().to_path_buf()], 512).expect("watcher");

        write!(file, "partial").expect("write");
        file.as_file_mut().flush().expect("flush");
        let events = watcher.poll().expect("poll");
        assert!(events.is_empty());

        writeln!(file, "-line").expect("write");
        let events = watcher.poll().expect("poll");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].line, "partial-line");
    }

    #[test]
    fn add_file_tracks_new_path_and_rejects_duplicate() {
        let first = NamedTempFile::new().expect("first temp file");
        let second = NamedTempFile::new().expect("second temp file");
        let first_path = first.path().to_path_buf();
        let second_path = second.path().to_path_buf();
        let mut watcher = PollingWatcher::new(vec![first_path.clone()], 512).expect("watcher");

        assert!(watcher.add_file(second_path).expect("add second file"));
        assert!(
            !watcher
                .add_file(first_path)
                .expect("duplicate returns false")
        );
    }

    #[test]
    fn unreadable_file_is_ignored_and_reported_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blocked_path = dir.path().join("blocked");
        let mut watcher = PollingWatcher::new(vec![blocked_path.clone()], 512).expect("watcher");
        fs::create_dir(&blocked_path).expect("create directory at tracked path");

        let events = watcher.poll().expect("poll should not fail");
        assert!(events.is_empty());
        let messages = watcher.take_status_messages();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("Ignoring unreadable file"));

        let events = watcher.poll().expect("poll should not fail");
        assert!(events.is_empty());
        assert!(watcher.take_status_messages().is_empty());
    }

    #[test]
    fn today_placeholder_tracks_existing_matching_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target_path = dir.path().join("app_0424.log");
        let pattern_path = dir.path().join("app_{today}.log");
        let mut file = File::create(&target_path).expect("create target log");
        writeln!(file, "old-line").expect("write old line");
        file.flush().expect("flush");

        let mut watcher = PollingWatcher {
            tracked_files: vec![TrackedPathState::new(pattern_path).expect("pattern state")],
            max_line_len: 512,
            file_enabled: HashMap::new(),
            unreadable_files: HashMap::new(),
            status_messages: Vec::new(),
        };
        let file_enabled = HashMap::new();

        let events = watcher.tracked_files[0]
            .poll(
                sample_date(),
                512,
                &file_enabled,
                &mut Vec::new(),
                &mut watcher.unreadable_files,
                &mut watcher.status_messages,
            )
            .map(|_| Vec::<LogEvent>::new())
            .expect("initial poll");
        assert!(events.is_empty());

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&target_path)
            .expect("open target log");
        writeln!(file, "new-line").expect("append line");
        file.flush().expect("flush");

        let mut out = Vec::new();
        watcher.tracked_files[0]
            .poll(
                sample_date(),
                512,
                &file_enabled,
                &mut out,
                &mut watcher.unreadable_files,
                &mut watcher.status_messages,
            )
            .expect("poll after append");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].line, "new-line");
        assert_eq!(out[0].source, "app_0424.log");
    }

    #[test]
    fn dated_rollover_keeps_previous_file_until_it_disappears() {
        let dir = tempfile::tempdir().expect("tempdir");
        let previous_path = dir.path().join("app_0424.log");
        let next_path = dir.path().join("app_0425.log");
        let pattern_path = dir.path().join("app_{mmdd}.log");

        let mut previous_file = File::create(&previous_path).expect("create previous log");
        writeln!(previous_file, "old-line").expect("write old line");
        previous_file.flush().expect("flush previous");

        let mut tracked = TrackedPathState {
            raw_path: pattern_path.clone(),
            resolver: PathResolver::new(&pattern_path),
            active_files: vec![
                TrackedFileState::new(previous_path.clone()).expect("previous state"),
            ],
            last_resolution_warnings: HashSet::new(),
        };
        let mut unreadable = HashMap::new();
        let mut status_messages = Vec::new();
        let file_enabled = HashMap::new();

        tracked
            .sync_active_files(
                next_sample_date(),
                &file_enabled,
                &mut unreadable,
                &mut status_messages,
            )
            .expect("rollover sync");

        let active_paths = tracked
            .active_files
            .iter()
            .map(|state| state.path.clone())
            .collect::<Vec<_>>();
        assert_eq!(active_paths, vec![next_path.clone(), previous_path.clone()]);

        let mut next_file = File::create(&next_path).expect("create next log");
        writeln!(next_file, "new-day-line").expect("write new-day line");
        next_file.flush().expect("flush next");

        let mut out = Vec::new();
        tracked
            .poll(
                next_sample_date(),
                512,
                &file_enabled,
                &mut out,
                &mut unreadable,
                &mut status_messages,
            )
            .expect("poll after next-day file creation");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, "app_0425.log");
        assert_eq!(out[0].line, "new-day-line");

        std::fs::remove_file(&previous_path).expect("remove previous log");
        tracked
            .sync_active_files(
                next_sample_date(),
                &file_enabled,
                &mut unreadable,
                &mut status_messages,
            )
            .expect("sync after previous removal");

        let active_paths = tracked
            .active_files
            .iter()
            .map(|state| state.path.clone())
            .collect::<Vec<_>>();
        assert_eq!(active_paths, vec![next_path]);
    }

    #[test]
    fn wildcard_warning_is_reported_once_per_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        for idx in 0..(MAX_WILDCARD_MATCHES + 2) {
            std::fs::write(dir.path().join(format!("overflow-{idx:03}.log")), "x\n")
                .expect("write wildcard fixture");
        }

        let mut watcher =
            PollingWatcher::new(vec![dir.path().join("overflow-*.log")], 512).expect("watcher");

        watcher.poll().expect("first poll");
        let first_messages = watcher.take_status_messages();
        assert_eq!(first_messages.len(), 1);
        assert!(first_messages[0].contains("matched more than"));

        watcher.poll().expect("second poll");
        assert!(watcher.take_status_messages().is_empty());
    }

    #[test]
    fn active_sources_return_resolved_source_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("app-a.log"), "a\n").expect("write app-a");
        std::fs::write(dir.path().join("app-b.log"), "b\n").expect("write app-b");

        let watcher =
            PollingWatcher::new(vec![dir.path().join("app-*.log")], 512).expect("watcher");

        assert_eq!(
            watcher.active_sources(),
            vec!["app-a.log".to_string(), "app-b.log".to_string()]
        );
    }

    #[test]
    fn today_descriptors_only_show_existing_date_candidates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tokens = DateTokens::for_date(current_local_date());
        let existing = dir
            .path()
            .join(format!("app_{}.log", tokens.value_for("mm_dd")));
        std::fs::write(&existing, "a\n").expect("write existing today log");

        let watcher =
            PollingWatcher::new(vec![dir.path().join("app_{today}.log")], 512).expect("watcher");
        let descriptors = watcher.tracked_path_descriptors();

        assert_eq!(descriptors.len(), 1);
        assert_eq!(descriptors[0].resolved_files.len(), 1);
        assert_eq!(descriptors[0].resolved_files[0].path, existing);
        assert_eq!(
            watcher.active_sources(),
            vec![descriptors[0].resolved_files[0].source_label.clone()]
        );
    }

    #[test]
    fn literal_descriptor_still_shows_missing_configured_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("missing.log");

        let watcher = PollingWatcher::new(vec![missing.clone()], 512).expect("watcher");
        let descriptors = watcher.tracked_path_descriptors();

        assert_eq!(descriptors.len(), 1);
        assert_eq!(descriptors[0].resolved_files.len(), 1);
        assert_eq!(descriptors[0].resolved_files[0].path, missing);
        assert!(watcher.active_sources().is_empty());
    }

    #[test]
    fn active_file_paths_return_resolved_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_a = dir.path().join("app-a.log");
        let app_b = dir.path().join("app-b.log");
        std::fs::write(&app_a, "a\n").expect("write app-a");
        std::fs::write(&app_b, "b\n").expect("write app-b");

        let watcher =
            PollingWatcher::new(vec![dir.path().join("app-*.log")], 512).expect("watcher");

        assert_eq!(
            watcher.active_file_paths(),
            vec![app_a.display().to_string(), app_b.display().to_string()]
        );
    }

    #[test]
    fn disabled_file_is_removed_from_active_sources() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_a = dir.path().join("app-a.log");
        let app_b = dir.path().join("app-b.log");
        std::fs::write(&app_a, "a\n").expect("write app-a");
        std::fs::write(&app_b, "b\n").expect("write app-b");

        let mut enabled = HashMap::new();
        enabled.insert(app_b.clone(), false);
        let watcher =
            PollingWatcher::with_file_enabled(vec![dir.path().join("app-*.log")], enabled, 512)
                .expect("watcher");

        assert_eq!(watcher.active_sources(), vec!["app-a.log".to_string()]);
        assert_eq!(
            watcher.active_file_paths(),
            vec![app_a.display().to_string()]
        );
    }
}
