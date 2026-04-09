use crate::watcher::LogEvent;
use anyhow::{Context, Result};
use regex::Regex;

#[derive(Debug, Clone)]
pub struct LineRules {
    blacklist: Vec<Regex>,
    whitelist: Vec<Regex>,
}

impl LineRules {
    pub fn new(blacklist_patterns: &[String], whitelist_patterns: &[String]) -> Result<Self> {
        let blacklist = compile_patterns(blacklist_patterns, "blacklist")?;
        let whitelist = compile_patterns(whitelist_patterns, "whitelist")?;
        Ok(Self {
            blacklist,
            whitelist,
        })
    }

    pub fn partition_events(&self, events: Vec<LogEvent>) -> (Vec<LogEvent>, usize) {
        let mut kept = Vec::with_capacity(events.len());
        let mut suppressed = 0usize;
        for event in events {
            if self.should_display(&event.line) {
                kept.push(event);
            } else {
                suppressed += 1;
            }
        }
        (kept, suppressed)
    }

    pub fn should_display(&self, line: &str) -> bool {
        if self.whitelist.iter().any(|pattern| pattern.is_match(line)) {
            return true;
        }
        if self.blacklist.iter().any(|pattern| pattern.is_match(line)) {
            return false;
        }
        true
    }
}

fn compile_patterns(patterns: &[String], kind: &str) -> Result<Vec<Regex>> {
    patterns
        .iter()
        .map(|pattern| {
            Regex::new(pattern).with_context(|| format!("invalid {} regex `{}`", kind, pattern))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_overrides_blacklist() {
        let rules = LineRules::new(&["ERROR".to_string()], &["ERROR allow-this".to_string()])
            .expect("valid rules");
        assert!(!rules.should_display("ERROR random issue"));
        assert!(rules.should_display("ERROR allow-this"));
    }

    #[test]
    fn non_matching_lines_are_kept() {
        let rules = LineRules::new(&["DEBUG".to_string()], &[]).expect("valid rules");
        assert!(rules.should_display("INFO started"));
    }
}
