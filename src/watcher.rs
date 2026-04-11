use anyhow::{Context, Result};
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct LogEvent {
    pub ts: SystemTime,
    pub source: String,
    pub line: String,
}

#[derive(Debug, Clone)]
pub struct PollingWatcher {
    tracked_files: Vec<TrackedFileState>,
    max_line_len: usize,
}

impl PollingWatcher {
    pub fn new(paths: Vec<PathBuf>, max_line_len: usize) -> Result<Self> {
        let mut tracked_files = Vec::with_capacity(paths.len());
        for path in paths {
            tracked_files.push(TrackedFileState::new(path)?);
        }
        Ok(Self {
            tracked_files,
            max_line_len,
        })
    }

    pub fn poll(&mut self) -> Result<Vec<LogEvent>> {
        let mut out = Vec::new();
        for state in &mut self.tracked_files {
            state.poll(self.max_line_len, &mut out)?;
        }
        Ok(out)
    }

    pub fn add_file(&mut self, path: PathBuf) -> Result<bool> {
        if self.tracked_files.iter().any(|state| state.path == path) {
            return Ok(false);
        }
        self.tracked_files.push(TrackedFileState::new(path)?);
        Ok(true)
    }
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
    use std::io::Write;
    use tempfile::NamedTempFile;

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
}
