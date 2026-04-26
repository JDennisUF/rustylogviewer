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
const MAX_WILDCARD_MATCHES: usize = 200;

#[derive(Debug, Clone)]
pub struct LogEvent {
    pub ts: SystemTime,
    pub source: String,
    pub line: String,
}

#[derive(Debug, Clone)]
pub struct PollingWatcher {
    tracked_files: Vec<TrackedPathState>,
    max_line_len: usize,
    unreadable_files: HashMap<PathBuf, String>,
    status_messages: Vec<String>,
}

impl PollingWatcher {
    pub fn new(paths: Vec<PathBuf>, max_line_len: usize) -> Result<Self> {
        let mut tracked_files = Vec::with_capacity(paths.len());
        for path in paths {
            tracked_files.push(TrackedPathState::new(path)?);
        }
        Ok(Self {
            tracked_files,
            max_line_len,
            unreadable_files: HashMap::new(),
            status_messages: Vec::new(),
        })
    }

    pub fn poll(&mut self) -> Result<Vec<LogEvent>> {
        self.status_messages.clear();
        let mut out = Vec::new();
        let today = current_local_date();
        for state in &mut self.tracked_files {
            state.poll(
                today,
                self.max_line_len,
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
                    .map(|file| file.path.display().to_string())
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
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
        out: &mut Vec<LogEvent>,
        unreadable_files: &mut HashMap<PathBuf, String>,
        status_messages: &mut Vec<String>,
    ) -> Result<()> {
        self.sync_active_files(today, unreadable_files, status_messages)?;

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

        for path in resolved_paths {
            if let Some(existing_idx) = existing_files.iter().position(|state| state.path == path) {
                synced_files.push(existing_files.remove(existing_idx));
            } else {
                synced_files.push(TrackedFileState::new(path)?);
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
            unreadable_files: HashMap::new(),
            status_messages: Vec::new(),
        };

        let events = watcher.tracked_files[0]
            .poll(
                sample_date(),
                512,
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
}
