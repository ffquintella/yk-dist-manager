//! The **single** logging entry point for the application.
//!
//! Guide G-002 fixes the operational log format:
//!
//! ```text
//! [dd/mm/aaaa] hh:mm:ss ; evento ; detalhes
//! ```
//!
//! and requires at least three levels (Informação / Aviso / Erro). This module
//! configures `tracing` to emit exactly that, so no call site ever formats a
//! log line by hand. Every log call must therefore look like:
//!
//! ```ignore
//! tracing::info!(event = "key.detected", serial = 20423633);
//! ```
//!
//! Secrets (PIN, PUK, management key, OTP access code) must never be passed as
//! a field — see `docs/security-and-compliance.md`.

use std::fmt;
use std::io::{self, Write as _};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, MakeWriter};
use tracing_subscriber::registry::LookupSpan;

use crate::logbuf::LogBuffer;
use crate::logfile::LogFile;

/// Field names accepted as the `evento` slot of the log line.
const EVENT_FIELDS: [&str; 3] = ["message", "event", "evento"];

/// The filter a start uses when `$YKDM_LOG` says nothing.
///
/// `info` for this application, and `warn` for the [`yubikey`] crate, whose
/// `connected to reader` line is emitted at `info` **on every poll of the card**
/// — once or twice a second, for as long as a key is in the slot. A log file
/// recovered from a workstation that had been open for an afternoon was 976 KiB
/// of which 3,500 lines were that one sentence: it had rotated away every
/// generation that held the start-up lines the file was collected for. Warnings
/// and errors from the crate are kept, because those are about the key rather
/// than about the polling.
///
/// `wgpu` and `eframe` are deliberately left at `info`: the adapter list they
/// print is what named the faulting backend on the workstation
/// `features/renderer-fallback.md` exists for.
pub const DEFAULT_FILTER: &str = "info,yubikey=warn";

/// Whether this is a release build, in the one word a ticket needs.
fn build_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

/// What identifies this build and this process, for the head of every log file.
///
/// The whole point is that a log file arriving in a ticket answers "which build,
/// on what, and which of the processes writing here" without anybody having to
/// ask. Every field is one that has already cost a support conversation:
///
/// * `version`, `commit`, `build` — the file that prompted this had three starts
///   in it, all from 0.19.2, on a workstation everyone believed had been
///   upgraded to the release that fixes exactly what it was failing at.
/// * `os`, `arch` — the same binary is installed on Windows, macOS and Linux.
/// * `pid` — that file was **two** processes: an instance from the morning still
///   polling its key, interleaved line by line with three launches that never
///   got a window. Nothing in the file said so.
/// * `features` — whether this build has `encrypted-db`, a camera or a native
///   transport changes which failures are even possible.
///
/// Nothing here is personal data and nothing here is a secret (AGENTS.md §2):
/// it is the build, the platform and a process id.
pub fn metadata() -> Vec<(&'static str, String)> {
    vec![
        ("version", crate::VERSION.to_owned()),
        ("commit", crate::COMMIT.to_owned()),
        ("build", build_profile().to_owned()),
        ("os", std::env::consts::OS.to_owned()),
        ("arch", std::env::consts::ARCH.to_owned()),
        ("pid", std::process::id().to_string()),
        (
            "features",
            crate::diagnostics::compiled_features().join("+"),
        ),
    ]
}

/// [`metadata`] as one G-002 line, ready to write.
///
/// Built by hand rather than emitted through `tracing`, because the two places
/// that need it are both outside a subscriber: the first line of a session,
/// written before the subscriber is installed, and the first line of every
/// rotated generation, written by [`crate::logfile::LogFile`] as it opens one.
/// Same [`render_line`] as every other line, so the shape cannot drift.
pub fn metadata_line() -> String {
    let details: Vec<String> = metadata()
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    render_line(
        chrono::Local::now(),
        "app.build",
        level_label(&Level::INFO),
        &details.join(" "),
    )
}

/// The G-002 layout itself, in one place: `[dd/mm/aaaa] hh:mm:ss ; evento ;
/// detalhes`, newline included.
///
/// Every line this application writes goes through here — the subscriber's
/// [`FgvFormat`] and the hand-written [`metadata_line`] alike — because a file
/// whose first line has a different shape from the rest is a file no parser and
/// no reader can trust.
pub fn render_line(
    now: chrono::DateTime<chrono::Local>,
    event: &str,
    nivel: &str,
    details: &str,
) -> String {
    format!(
        "[{}] {} ; {} ; nivel={} {}\n",
        now.format("%d/%m/%Y"),
        now.format("%H:%M:%S"),
        event,
        nivel,
        details,
    )
}

/// Install the global subscriber. Safe to call once; later calls are ignored.
///
/// Three sinks, all fed the same G-002 line (see [`Sinks`]): the rotating file
/// under [`crate::logfile::directory`], the in-memory ring the *Show log* panel
/// reads, and stderr for whoever launched this from a terminal.
///
/// A log directory that cannot be created costs the operator the file sink and
/// nothing else — the application still starts, and the panel still fills. The
/// alternative, refusing to launch because the diagnostics are unavailable, gets
/// the priority exactly backwards.
pub fn init() {
    let file = match LogFile::open(&crate::logfile::directory()) {
        Ok(file) => Some(file),
        Err(problem) => {
            // No `tracing` yet, so this one line is written by hand, and the
            // buffer is told as well because stderr may go nowhere.
            let text = format!(
                "logging.file.unavailable ; {} — {problem}",
                crate::logfile::path().display()
            );
            let _ = writeln!(io::stderr(), "{text}");
            crate::logbuf::shared().push(crate::logbuf::Level::Error, text);
            None
        }
    };
    let file = file.map(|file| file.with_header(metadata_line));
    let sinks = Sinks::new(file, crate::logbuf::shared());
    // Before the subscriber, and to all three sinks: whoever reads this file,
    // this panel or this terminal should not have to ask which build wrote it.
    sinks.note(crate::logbuf::Level::Info, &metadata_line());
    install(sinks);
}

/// Install a subscriber writing to `sinks`. Later calls are ignored, which is
/// what makes a second `init()` in a test binary harmless.
fn install(sinks: Sinks) {
    let filter =
        EnvFilter::try_from_env("YKDM_LOG").unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let _ = tracing_subscriber::fmt()
        .event_format(FgvFormat)
        .with_writer(sinks)
        .with_env_filter(filter)
        .try_init();
}

/// Everywhere one formatted log line goes.
///
/// One formatter, three destinations, rather than three subscribers: the norm
/// fixes *the* line format, and a panel or a file showing a different shape of
/// line from the one in the ticket is how a support conversation goes wrong.
#[derive(Clone, Default)]
pub struct Sinks {
    /// `None` when the log directory could not be opened.
    ///
    /// Poisoning is recovered from rather than propagated: the lock is held only
    /// across one `write_all`, and a panic elsewhere must not silently switch
    /// the log off for the rest of the session — least of all when a panic is
    /// exactly what wants recording.
    file: Option<Arc<Mutex<LogFile>>>,
    buffer: LogBuffer,
    /// Whether the last write to the file failed, so the panel is told once per
    /// spell of trouble rather than once per line.
    ///
    /// A full disk fails *every* write, and a ring of 500 identical complaints
    /// would evict the history the operator opened the panel to read — which is
    /// the same failure as saying nothing, reached from the other direction.
    file_failing: Arc<std::sync::atomic::AtomicBool>,
}

impl Sinks {
    pub fn new(file: Option<LogFile>, buffer: LogBuffer) -> Self {
        Self {
            file: file.map(|f| Arc::new(Mutex::new(f))),
            buffer,
            file_failing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Write one already-formatted line to every sink, outside the subscriber.
    ///
    /// For the line that has to be written *before* `tracing` exists — the
    /// build metadata at the head of a session ([`metadata_line`]) — and for
    /// nothing else. Everything that happens while the application is running
    /// goes through `tracing::info!` and friends, so it is filtered, levelled
    /// and timestamped like the rest.
    pub fn note(&self, level: crate::logbuf::Level, line: &str) {
        self.emit(level, line.as_bytes());
    }

    fn emit(&self, level: crate::logbuf::Level, line: &[u8]) {
        // stderr first and unconditionally: it is free, and on a workstation
        // where the log directory is unwritable it is all there is. Its own
        // failure is ignored — a GUI process on Windows has no console at all.
        let _ = io::stderr().write_all(line);

        if let Some(file) = &self.file {
            let mut file = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            match file.write_line(line) {
                // Not through `tracing`: that would re-enter this writer. The
                // panel is where an operator will see it, and §3 forbids letting
                // a failed write disappear.
                Err(problem) => {
                    if !self.file_failing.swap(true, Ordering::Relaxed) {
                        self.buffer.push(
                            crate::logbuf::Level::Error,
                            format!(
                                "logging.file.write_failed ; {} — {problem}",
                                file.path().display()
                            ),
                        );
                    }
                }
                Ok(()) => {
                    if self.file_failing.swap(false, Ordering::Relaxed) {
                        self.buffer.push(
                            crate::logbuf::Level::Warn,
                            "logging.file.write_recovered".to_owned(),
                        );
                    }
                }
            }
        }

        self.buffer
            .push(level, String::from_utf8_lossy(line).trim_end().to_owned());
    }
}

/// One event's worth of bytes, dispatched to every sink when it is complete.
///
/// The layer formats an event into a buffer and writes it in one go, but
/// `io::Write` does not promise that, so the line is accumulated and flushed on
/// drop — a half-line reaching the file would break the one-event-one-line
/// property every reader of it depends on.
pub struct SinkWriter<'a> {
    sinks: &'a Sinks,
    level: crate::logbuf::Level,
    line: Vec<u8>,
}

impl io::Write for SinkWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.line.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.line.is_empty() {
            let line = std::mem::take(&mut self.line);
            self.sinks.emit(self.level, &line);
        }
        Ok(())
    }
}

impl Drop for SinkWriter<'_> {
    fn drop(&mut self) {
        let _ = io::Write::flush(self);
    }
}

impl<'a> MakeWriter<'a> for Sinks {
    type Writer = SinkWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        SinkWriter {
            sinks: self,
            level: crate::logbuf::Level::Info,
            line: Vec::new(),
        }
    }

    fn make_writer_for(&'a self, meta: &Metadata<'_>) -> Self::Writer {
        SinkWriter {
            sinks: self,
            level: panel_level(meta.level()),
            line: Vec::new(),
        }
    }
}

/// The severity the panel filters on, from the one the event carries.
pub fn panel_level(level: &Level) -> crate::logbuf::Level {
    match *level {
        Level::ERROR => crate::logbuf::Level::Error,
        Level::WARN => crate::logbuf::Level::Warn,
        Level::INFO => crate::logbuf::Level::Info,
        _ => crate::logbuf::Level::Debug,
    }
}

/// Record every panic as an `Erro` line before the process goes.
///
/// Without this, a panic anywhere — including in `YkDistApp::new`, where the
/// register is opened — prints to a stderr that a launched-from-the-desktop
/// application does not have, and the window simply never appears. The previous
/// hook still runs afterwards, so a debug build keeps its backtrace.
///
/// Secrets: a panic message is arbitrary text, which is why
/// [`crate::secret::Secret`] has a redacting `Debug` and no `Display`
/// (`AGENTS.md` §2). Nothing is redacted here — a secret that reached a panic
/// message reached stderr and the terminal first.
pub fn install_panic_hook() {
    static INSTALLED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if INSTALLED.set(()).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let (message, location) = panic_fields(
            info.payload_as_str(),
            info.location()
                .map(|at| format!("{}:{}", at.file(), at.line())),
        );
        tracing::error!(event = "app.panic", location = %location, message = %message);
        // The start-up marker is deliberately left alone: if the panic happened
        // during start-up it already names the stage, and if it happened hours
        // later there is no marker to write without accusing the next launch of
        // a start-up failure that never happened.
        previous(info);
    }));
}

/// The two fields a panic contributes, defaulted and flattened.
///
/// Pure so it can be tested: the hook itself cannot be, because installing one
/// is process-global and un-installing it is not possible.
pub fn panic_fields(payload: Option<&str>, location: Option<String>) -> (String, String) {
    (
        payload
            .map(one_line)
            .unwrap_or_else(|| "(panicked with no message)".to_owned()),
        location.unwrap_or_else(|| "(unknown)".to_owned()),
    )
}

/// Flatten and cap arbitrary text so it cannot break the line format.
fn one_line(text: &str) -> String {
    const MAX: usize = 400;
    let flattened: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flattened = flattened.trim().to_owned();
    match flattened.char_indices().nth(MAX) {
        Some((cut, _)) => format!("{}…", &flattened[..cut]),
        None => flattened,
    }
}

/// Formatter for the G-002 log layout.
pub struct FgvFormat;

#[derive(Default)]
struct Captured {
    event: Option<String>,
    details: Vec<String>,
}

impl Captured {
    fn push(&mut self, name: &str, value: String) {
        if EVENT_FIELDS.contains(&name) && self.event.is_none() {
            self.event = Some(value);
        } else {
            self.details.push(format!("{name}={value}"));
        }
    }
}

impl Visit for Captured {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field.name(), value.to_owned());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field.name(), value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field.name(), value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field.name(), value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push(field.name(), format!("{value:?}"));
    }
}

/// Maps a `tracing` level onto the three G-002 categories.
pub fn level_label(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "Erro",
        Level::WARN => "Aviso",
        _ => "Informacao",
    }
}

impl<S, N> FormatEvent<S, N> for FgvFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut captured = Captured::default();
        event.record(&mut captured);

        write!(
            writer,
            "{}",
            render_line(
                chrono::Local::now(),
                captured.event.as_deref().unwrap_or("(sem evento)"),
                level_label(event.metadata().level()),
                &captured.details.join(" "),
            )
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_map_to_three_categories() {
        assert_eq!(level_label(&Level::ERROR), "Erro");
        assert_eq!(level_label(&Level::WARN), "Aviso");
        assert_eq!(level_label(&Level::INFO), "Informacao");
        assert_eq!(level_label(&Level::DEBUG), "Informacao");
    }

    #[test]
    fn the_metadata_line_names_the_build_the_platform_and_the_process() {
        // The log that prompted this: three starts in one file, all of them from
        // a version everybody believed had been replaced, interleaved with the
        // lines of a fourth process nobody knew was still running.
        let line = metadata_line();
        for expected in [
            "app.build",
            "nivel=Informacao",
            &format!("version={}", crate::VERSION),
            &format!("commit={}", crate::COMMIT),
            &format!("os={}", std::env::consts::OS),
            &format!("arch={}", std::env::consts::ARCH),
            &format!("pid={}", std::process::id()),
            "build=",
            "features=",
        ] {
            assert!(
                line.contains(expected),
                "{expected} is missing from: {line}"
            );
        }
    }

    #[test]
    fn the_metadata_line_has_the_same_shape_as_every_other_line() {
        // It is written by hand, before the subscriber exists, so this is the
        // only thing keeping it from being the one line in the file that no
        // reader of the file can parse.
        let line = metadata_line();
        assert!(line.ends_with('\n'), "{line:?}");
        let body = line.trim_end();
        let parts: Vec<&str> = body.split(" ; ").collect();
        assert_eq!(parts.len(), 3, "expected the G-002 three fields: {body}");
        assert!(parts[0].starts_with('['), "{body}");
        assert_eq!(parts[1], "app.build");
        // No field may carry a `;` or a newline into the middle of a line.
        for (key, value) in metadata() {
            assert!(
                !value.contains(';') && !value.contains('\n'),
                "{key}={value} would break the line format"
            );
        }
    }

    #[test]
    fn the_default_filter_keeps_the_card_polling_out_of_the_log() {
        // 3,500 lines of `connected to reader` in one collected log file, at one
        // to two a second, which had rotated away every generation that held the
        // start-up lines it was collected for.
        assert!(DEFAULT_FILTER.starts_with("info"));
        assert!(DEFAULT_FILTER.contains("yubikey=warn"));
        assert!(
            EnvFilter::try_new(DEFAULT_FILTER).is_ok(),
            "an unparseable default would silently become `info` for everything"
        );
        // wgpu's adapter list is what named the faulting backend on the
        // workstation `features/renderer-fallback.md` exists for: it stays.
        assert!(!DEFAULT_FILTER.contains("wgpu"));
    }

    #[test]
    fn first_event_field_wins_and_rest_become_details() {
        let mut c = Captured::default();
        c.push("event", "key.detected".into());
        c.push("serial", "20423633".into());
        assert_eq!(c.event.as_deref(), Some("key.detected"));
        assert_eq!(c.details, vec!["serial=20423633"]);
    }
}
