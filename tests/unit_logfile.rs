//! Unit tests for the log on disk: where it goes, that it stays bounded, and
//! that a G-002 line survives the trip through the file sink unchanged.
//!
//! `features/logging.md` phase 2. The rotation and marker mechanics are unit
//! tested in-source, next to the code; what is here is the part that needs the
//! real subscriber, the real environment variables, or both.
//!
//! Every test names its own temporary directory. `$YKDM_LOG_DIR` is
//! process-global, so only [`the_environment_decides_where_the_log_goes`] sets
//! it, and it is the only test in this binary that reads it.

use std::path::Path;

use yk_dist_manager::logbuf::{Level, LogBuffer};
use yk_dist_manager::logfile::{self, LogFile};
use yk_dist_manager::logging::{FgvFormat, Sinks};

fn temp() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

/// Run `body` against a subscriber wired exactly as the application wires it,
/// writing into `directory`, and hand back the buffer the panel would read.
fn with_sinks(directory: &Path, body: impl FnOnce()) -> LogBuffer {
    let buffer = LogBuffer::new();
    let file = LogFile::open(directory).expect("the log file opens");
    let subscriber = tracing_subscriber::fmt()
        .event_format(FgvFormat)
        .with_writer(Sinks::new(Some(file), buffer.clone()))
        .with_max_level(tracing::Level::TRACE)
        .finish();
    tracing::subscriber::with_default(subscriber, body);
    buffer
}

fn contents(directory: &Path) -> String {
    std::fs::read_to_string(directory.join(logfile::FILE_NAME)).expect("the log file is readable")
}

#[test]
fn the_file_sink_writes_the_g002_line_and_nothing_else() {
    // The normative assertion for phase 2: what reaches the file is the same
    // line the norm fixes, not a debug rendering of an event.
    let home = temp();
    with_sinks(home.path(), || {
        tracing::info!(event = "key.detected", serial = 20_423_633_u64);
    });

    let body = contents(home.path());
    let line = body.lines().next().expect("one line was written");
    let parts: Vec<&str> = line.split(" ; ").collect();
    assert_eq!(
        parts.len(),
        3,
        "expected `timestamp ; evento ; detalhes`, got: {line}"
    );
    assert!(parts[0].starts_with('['), "{line}");
    assert_eq!(parts[1], "key.detected");
    assert!(parts[2].contains("nivel=Informacao"), "{line}");
    assert!(parts[2].contains("serial=20423633"), "{line}");
    assert_eq!(body.lines().count(), 1, "one event, one line");
}

#[test]
fn one_event_reaches_the_file_and_the_panel_alike() {
    // The two ends of a support conversation — the operator reading the panel
    // and whoever opens the file afterwards — must not be shown different text.
    let home = temp();
    let buffer = with_sinks(home.path(), || {
        tracing::error!(event = "db.open.failed", detail = "share not reachable");
    });

    let file_line = contents(home.path()).trim_end().to_owned();
    let panel = buffer.lines(Level::Debug);
    assert_eq!(panel.len(), 1);
    assert_eq!(panel[0].text, file_line);
    assert_eq!(
        panel[0].level,
        Level::Error,
        "the panel filters on severity, so it has to carry it"
    );
}

#[test]
fn severity_survives_the_trip_into_the_panel() {
    let home = temp();
    let buffer = with_sinks(home.path(), || {
        tracing::debug!(event = "a");
        tracing::info!(event = "b");
        tracing::warn!(event = "c");
        tracing::error!(event = "d");
    });

    let levels: Vec<Level> = buffer.lines(Level::Debug).iter().map(|l| l.level).collect();
    assert_eq!(
        levels,
        vec![Level::Debug, Level::Info, Level::Warn, Level::Error]
    );
}

#[test]
fn the_log_survives_a_directory_that_cannot_be_opened() {
    // A read-only or missing profile directory must cost the diagnostics, never
    // the launch: `Sinks` with no file still feeds the panel.
    let buffer = LogBuffer::new();
    let subscriber = tracing_subscriber::fmt()
        .event_format(FgvFormat)
        .with_writer(Sinks::new(None, buffer.clone()))
        .with_max_level(tracing::Level::TRACE)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        tracing::warn!(event = "reader.unavailable");
    });

    assert_eq!(buffer.lines(Level::Warn).len(), 1);
}

#[test]
fn a_log_file_that_stops_accepting_writes_is_reported_once_not_once_per_line() {
    // The scenario: the log directory goes away under a running application —
    // somebody "cleaning up", a profile on a share that dropped. Every write
    // then fails, and 500 identical complaints in a 500-line ring would evict
    // the history the operator opened the panel to read.
    let home = temp();
    let inner = home.path().join("logs");
    let file = LogFile::with_limits(&inner, 8, 2).expect("opens");
    let buffer = LogBuffer::new();
    let subscriber = tracing_subscriber::fmt()
        .event_format(FgvFormat)
        .with_writer(Sinks::new(Some(file), buffer.clone()))
        .with_max_level(tracing::Level::TRACE)
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(event = "before");
        std::fs::remove_dir_all(&inner).expect("the directory goes away");
        for n in 0..5 {
            tracing::info!(event = "after", n = n as u64);
        }
    });

    let complaints = buffer
        .lines(Level::Debug)
        .iter()
        .filter(|line| line.text.contains("logging.file.write_failed"))
        .count();
    assert_eq!(
        complaints,
        1,
        "five failed writes, one complaint: {:?}",
        buffer.lines(Level::Debug)
    );
    assert_eq!(
        buffer
            .lines(Level::Debug)
            .iter()
            .filter(|line| line.text.contains("; after ;"))
            .count(),
        5,
        "and the events themselves still reach the panel, which is now the only \
         place they exist"
    );
}

#[test]
fn a_long_session_cannot_fill_the_disk() {
    // The ceiling, asserted end to end rather than on `write_line` alone.
    let home = temp();
    let mut file = LogFile::with_limits(home.path(), 256, 2).expect("opens");
    for n in 0..500 {
        file.write_line(
            format!("[01/01/2026] 00:00:00 ; line.{n} ; nivel=Informacao\n").as_bytes(),
        )
        .expect("writes");
    }

    let total: u64 = std::fs::read_dir(home.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum();
    assert!(
        total <= 256 * 3 + 128,
        "three generations of 256 bytes at most, found {total}"
    );
    assert!(
        contents(home.path()).contains("line.499"),
        "and the newest line is the one that is kept"
    );
}

#[test]
fn the_environment_decides_where_the_log_goes() {
    // Both overrides, because deployments use one and the test suite uses the
    // other. The only test in this binary that touches process-global state.
    let home = temp();
    unsafe { std::env::set_var("YKDM_DATA_DIR", home.path()) };
    unsafe { std::env::remove_var("YKDM_LOG_DIR") };
    assert_eq!(
        logfile::directory(),
        home.path().join("logs"),
        "by default the log sits beside the settings and the default register"
    );
    assert_eq!(
        logfile::path(),
        home.path().join("logs").join(logfile::FILE_NAME)
    );

    let elsewhere = temp();
    unsafe { std::env::set_var("YKDM_LOG_DIR", elsewhere.path()) };
    assert_eq!(logfile::directory(), elsewhere.path());

    unsafe { std::env::remove_var("YKDM_LOG_DIR") };
    unsafe { std::env::remove_var("YKDM_DATA_DIR") };
}

#[test]
fn a_panic_message_cannot_break_the_line_format() {
    // A panic payload is arbitrary text, and the log's one-event-one-line
    // property is what every reader of the file depends on.
    let (message, location) = yk_dist_manager::logging::panic_fields(
        Some("assertion failed:\n  left: 1\n right: 2"),
        Some("src/store/mod.rs:120".into()),
    );
    assert!(!message.contains('\n'), "{message}");
    assert!(message.contains("left: 1"), "and it is still readable");
    assert_eq!(location, "src/store/mod.rs:120");

    let (message, location) = yk_dist_manager::logging::panic_fields(None, None);
    assert!(!message.is_empty(), "a nameless panic still says something");
    assert_eq!(location, "(unknown)");

    let (message, _) = yk_dist_manager::logging::panic_fields(Some(&"x".repeat(5_000)), None);
    assert!(
        message.chars().count() < 500,
        "an unbounded payload is capped, not written whole: {} chars",
        message.chars().count()
    );
}
