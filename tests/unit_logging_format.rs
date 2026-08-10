//! Unit tests for the log line format.
//!
//! G-002 specifies `[dd/mm/aaaa] hh:mm:ss ; evento ; detalhes` with at least
//! three levels. That is a normative requirement, so it is asserted rather than
//! assumed: these tests capture real subscriber output and parse it back.

use std::io::Write;
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;
use yk_dist_manager::logging::FgvFormat;

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Buffer {
    fn contents(&self) -> String {
        String::from_utf8(self.0.lock().expect("buffer lock").clone()).expect("utf-8")
    }
}

impl Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("buffer lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Buffer {
    type Writer = Buffer;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Run `body` against a subscriber using the production formatter, and return
/// what it wrote.
fn capture(body: impl FnOnce()) -> String {
    let buffer = Buffer::default();
    let subscriber = tracing_subscriber::fmt()
        .event_format(FgvFormat)
        .with_writer(buffer.clone())
        .with_max_level(tracing::Level::TRACE)
        .finish();
    tracing::subscriber::with_default(subscriber, body);
    buffer.contents()
}

/// `[dd/mm/aaaa] hh:mm:ss` — checked positionally, because the norm fixes the
/// layout and not merely the information.
fn assert_timestamp(prefix: &str) {
    let bytes: Vec<char> = prefix.chars().collect();
    assert_eq!(bytes[0], '[', "line must open with `[`: {prefix}");
    for index in [1, 2, 4, 5, 7, 8, 9, 10] {
        assert!(
            bytes[index].is_ascii_digit(),
            "expected a digit at position {index} of `{prefix}`"
        );
    }
    assert_eq!(bytes[3], '/', "day/month separator: {prefix}");
    assert_eq!(bytes[6], '/', "month/year separator: {prefix}");
    assert_eq!(bytes[11], ']', "date must be bracketed: {prefix}");

    let time = &prefix[13..21];
    let time: Vec<char> = time.chars().collect();
    assert_eq!(time[2], ':', "hh:mm separator: {prefix}");
    assert_eq!(time[5], ':', "mm:ss separator: {prefix}");
    for index in [0, 1, 3, 4, 6, 7] {
        assert!(
            time[index].is_ascii_digit(),
            "expected a digit in the time part of `{prefix}`"
        );
    }
}

#[test]
fn emits_the_g002_line_layout() {
    let out = capture(|| {
        tracing::info!(event = "key.detected", serial = 20_423_633_u64);
    });

    let line = out.lines().next().expect("one line was written");
    let parts: Vec<&str> = line.split(" ; ").collect();
    assert_eq!(
        parts.len(),
        3,
        "expected `timestamp ; evento ; detalhes`, got: {line}"
    );

    assert_timestamp(parts[0]);
    assert_eq!(parts[1], "key.detected", "the event slot carries the event");
    assert!(
        parts[2].contains("serial=20423633"),
        "other fields become key=value details: {line}"
    );
}

#[test]
fn the_three_levels_are_labelled_as_the_norm_names_them() {
    let out = capture(|| {
        tracing::info!(event = "a");
        tracing::warn!(event = "b");
        tracing::error!(event = "c");
        tracing::debug!(event = "d");
    });

    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4);
    assert!(lines[0].contains("nivel=Informacao"), "{}", lines[0]);
    assert!(lines[1].contains("nivel=Aviso"), "{}", lines[1]);
    assert!(lines[2].contains("nivel=Erro"), "{}", lines[2]);
    assert!(
        lines[3].contains("nivel=Informacao"),
        "debug maps onto the lowest category: {}",
        lines[3]
    );
}

#[test]
fn a_plain_message_lands_in_the_event_slot() {
    let out = capture(|| {
        tracing::info!("bare message");
    });
    let line = out.lines().next().unwrap();
    let parts: Vec<&str> = line.split(" ; ").collect();
    assert_eq!(parts[1], "bare message");
}

#[test]
fn an_event_without_a_name_is_marked_rather_than_blank() {
    let out = capture(|| {
        tracing::info!(serial = 1_u64);
    });
    let line = out.lines().next().unwrap();
    assert!(
        line.contains("(sem evento)"),
        "a nameless event must be visible, not an empty slot: {line}"
    );
}

#[test]
fn field_types_all_render() {
    let out = capture(|| {
        tracing::info!(
            event = "mixed",
            count = 3_i64,
            total = 7_u64,
            ok = true,
            name = "ana"
        );
    });
    let line = out.lines().next().unwrap();
    for expected in ["count=3", "total=7", "ok=true", "name=ana"] {
        assert!(line.contains(expected), "missing {expected} in: {line}");
    }
}

#[test]
fn every_event_is_one_line() {
    // A multi-line log entry breaks any grep-based reading of the trail.
    let out = capture(|| {
        tracing::info!(event = "one");
        tracing::info!(event = "two");
    });
    assert_eq!(out.lines().count(), 2);
    assert!(out.ends_with('\n'));
}
