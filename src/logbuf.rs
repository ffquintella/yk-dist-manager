//! The last N log lines, kept in memory so the operator can read them without
//! finding a file.
//!
//! `features/gui-shell.md` phase 8. The reason it earns its place at a desk: a
//! hand-over goes wrong, the status bar shows one line, and the useful detail is
//! in a log file on a path the operator does not know and may not have a terminal
//! to read. A panel they can open and copy from turns "it didn't work" into a
//! paste into a ticket.
//!
//! ## Bounded, always
//!
//! A ring of [`CAPACITY`] lines. An unbounded buffer in a process that runs all
//! day at a reception desk is a slow leak, and the lines that matter are the
//! recent ones — nobody scrolls back four hours in a GUI panel.
//!
//! ## This is not the audit trail
//!
//! `features/audit-trail.md` is emphatic about the split and it applies here.
//! Logs are for diagnosis and may be rotated away or lost on a crash; audit is
//! for accountability and is never rewritten. This buffer is the former. It is
//! *not* persisted, it is *not* a record of anything, and nothing in it should
//! ever be quoted as evidence.
//!
//! ## Secrets
//!
//! Nothing here redacts, because nothing here should need to: a secret never
//! reaches a log line in the first place (`AGENTS.md` §2, and
//! [`crate::secret::Secret`] has no `Display` and a redacting `Debug` precisely
//! so an accident prints `<redacted>`). This buffer holds whatever the logging
//! layer emitted, which is the same thing the log file holds — so if a secret
//! ever reached it, the file is already the bigger problem.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// How many lines the panel keeps.
pub const CAPACITY: usize = 500;

/// Severity, as the panel filters on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn label(&self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw.trim().to_uppercase().as_str() {
            "DEBUG" | "TRACE" => Level::Debug,
            "INFO" => Level::Info,
            "WARN" | "WARNING" => Level::Warn,
            "ERROR" => Level::Error,
            _ => return None,
        })
    }
}

/// One line as the panel shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub level: Level,
    pub text: String,
}

/// A bounded, shareable ring of recent log lines.
///
/// `Arc<Mutex<…>>` inside because the writer is the logging layer — potentially
/// on another thread, once hot-plug polling lands — and the reader is the paint
/// pass. The lock is held only to push or to clone out, never across a render.
#[derive(Debug, Clone, Default)]
pub struct LogBuffer {
    lines: Arc<Mutex<VecDeque<Line>>>,
}

impl LogBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a line, dropping the oldest once the ring is full.
    pub fn push(&self, level: Level, text: impl Into<String>) {
        let Ok(mut lines) = self.lines.lock() else {
            // A poisoned lock means a panic while holding it. Losing log lines
            // is the right cost here: this is a diagnostic aid, and taking the
            // application down to protect it would be the wrong trade.
            return;
        };
        if lines.len() == CAPACITY {
            lines.pop_front();
        }
        lines.push_back(Line {
            level,
            text: text.into(),
        });
    }

    /// Lines at or above `minimum`, oldest first.
    pub fn lines(&self, minimum: Level) -> Vec<Line> {
        let Ok(lines) = self.lines.lock() else {
            return Vec::new();
        };
        lines
            .iter()
            .filter(|line| line.level >= minimum)
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.lines.lock().map(|l| l.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        if let Ok(mut lines) = self.lines.lock() {
            lines.clear();
        }
    }

    /// Everything at or above `minimum`, as one block for the clipboard.
    ///
    /// The point of the panel: an operator pastes this into a ticket rather than
    /// describing what they saw.
    pub fn to_clipboard(&self, minimum: Level) -> String {
        self.lines(minimum)
            .iter()
            .map(|l| format!("{} {}", l.level.label(), l.text))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// How many lines of each level are held, for the panel's header.
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let lines = self.lines(Level::Debug);
        let count = |want: Level| lines.iter().filter(|l| l.level == want).count();
        (
            count(Level::Debug),
            count(Level::Info),
            count(Level::Warn),
            count(Level::Error),
        )
    }

    /// One line for the panel header, so an error is visible without opening it.
    pub fn describe(&self) -> String {
        let (_, _, warnings, errors) = self.counts();
        match (errors, warnings) {
            (0, 0) => format!("{} log lines", self.len()),
            (0, w) => format!("{} log lines, {w} warning(s)", self.len()),
            (e, 0) => format!("{} log lines, {e} ERROR(s)", self.len()),
            (e, w) => format!("{} log lines, {e} ERROR(s), {w} warning(s)", self.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_is_bounded_and_keeps_the_newest() {
        // The property that matters for a process left open all day.
        let buffer = LogBuffer::new();
        for n in 0..CAPACITY + 50 {
            buffer.push(Level::Info, format!("line {n}"));
        }
        assert_eq!(buffer.len(), CAPACITY);
        let lines = buffer.lines(Level::Debug);
        assert_eq!(lines.first().unwrap().text, "line 50", "the oldest went");
        assert_eq!(
            lines.last().unwrap().text,
            format!("line {}", CAPACITY + 49)
        );
    }

    #[test]
    fn filtering_by_level_keeps_everything_at_or_above_it() {
        let buffer = LogBuffer::new();
        buffer.push(Level::Debug, "d");
        buffer.push(Level::Info, "i");
        buffer.push(Level::Warn, "w");
        buffer.push(Level::Error, "e");

        assert_eq!(buffer.lines(Level::Debug).len(), 4);
        assert_eq!(buffer.lines(Level::Info).len(), 3);
        assert_eq!(buffer.lines(Level::Warn).len(), 2);
        assert_eq!(buffer.lines(Level::Error).len(), 1);
    }

    #[test]
    fn the_clipboard_block_carries_the_level_with_each_line() {
        // Pasted into a ticket, "the third line" means nothing without it.
        let buffer = LogBuffer::new();
        buffer.push(Level::Info, "db.opened path=/x");
        buffer.push(Level::Error, "db.backup.failed reason=disk full");

        let block = buffer.to_clipboard(Level::Info);
        assert!(block.contains("INFO db.opened"), "{block}");
        assert!(block.contains("ERROR db.backup.failed"), "{block}");
        assert_eq!(block.lines().count(), 2);
    }

    #[test]
    fn the_header_puts_an_error_in_front_of_the_operator_without_opening_the_panel() {
        let buffer = LogBuffer::new();
        buffer.push(Level::Info, "fine");
        assert_eq!(buffer.describe(), "1 log lines");

        buffer.push(Level::Warn, "hmm");
        assert!(
            buffer.describe().contains("1 warning(s)"),
            "{}",
            buffer.describe()
        );

        buffer.push(Level::Error, "bad");
        let described = buffer.describe();
        assert!(described.contains("1 ERROR(s)"), "{described}");
        assert!(
            described.contains("ERROR"),
            "upper case, because an error must not read like the rest: {described}"
        );
    }

    #[test]
    fn levels_parse_from_what_the_logging_layer_writes() {
        assert_eq!(Level::parse("info"), Some(Level::Info));
        assert_eq!(Level::parse(" WARN "), Some(Level::Warn));
        assert_eq!(Level::parse("warning"), Some(Level::Warn));
        assert_eq!(Level::parse("trace"), Some(Level::Debug));
        assert_eq!(Level::parse("shout"), None);
    }

    #[test]
    fn levels_order_so_a_filter_can_be_a_comparison() {
        assert!(Level::Error > Level::Warn);
        assert!(Level::Warn > Level::Info);
        assert!(Level::Info > Level::Debug);
    }

    #[test]
    fn a_clone_shares_the_same_ring() {
        // The logging layer holds one handle and the paint pass another; they
        // have to be looking at the same lines.
        let buffer = LogBuffer::new();
        let other = buffer.clone();
        buffer.push(Level::Info, "written through one handle");
        assert_eq!(other.len(), 1);

        other.clear();
        assert!(buffer.is_empty(), "and clearing through either clears both");
    }
}
