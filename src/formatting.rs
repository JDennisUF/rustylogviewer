use crate::watcher::LogEvent;
use std::time::SystemTime;
use time::{OffsetDateTime, UtcOffset, format_description};

pub fn format_event_line(event: &LogEvent, show_timestamps: bool) -> String {
    if !show_timestamps {
        return format!("[{}] {}", event.source, event.line);
    }

    let time_fragment = local_hms(event.ts).unwrap_or_else(|| "??:??:??".to_string());
    format!("[{}] [{}] {}", time_fragment, event.source, event.line)
}

fn local_hms(ts: SystemTime) -> Option<String> {
    let fmt = format_description::parse("[hour]:[minute]:[second]").ok()?;
    let offset = UtcOffset::current_local_offset().ok()?;
    let datetime = OffsetDateTime::from(ts).to_offset(offset);
    datetime.format(&fmt).ok()
}
