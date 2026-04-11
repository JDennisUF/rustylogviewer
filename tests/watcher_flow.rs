use logtrak::watcher::PollingWatcher;
use std::io::Write;
use tempfile::NamedTempFile;

#[test]
fn merges_events_from_multiple_files() {
    let mut a = NamedTempFile::new().expect("temp file a");
    let mut b = NamedTempFile::new().expect("temp file b");

    let mut watcher =
        PollingWatcher::new(vec![a.path().to_path_buf(), b.path().to_path_buf()], 512)
            .expect("watcher");

    writeln!(a, "alpha").expect("write");
    writeln!(b, "beta").expect("write");
    let events = watcher.poll().expect("poll");

    assert_eq!(events.len(), 2);
    assert!(events.iter().any(|event| event.line == "alpha"));
    assert!(events.iter().any(|event| event.line == "beta"));
}

#[test]
fn truncates_lines_to_max_len() {
    let mut file = NamedTempFile::new().expect("temp file");
    let mut watcher = PollingWatcher::new(vec![file.path().to_path_buf()], 5).expect("watcher");

    writeln!(file, "123456789").expect("write");
    let events = watcher.poll().expect("poll");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].line, "12345");
}
