//! The **single** logging entry point for the application.
//!
//! FGV G-002 fixes the operational log format:
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

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

/// Field names accepted as the `evento` slot of the log line.
const EVENT_FIELDS: [&str; 3] = ["message", "event", "evento"];

/// Install the global subscriber. Safe to call once; later calls are ignored.
pub fn init() {
    let filter = EnvFilter::try_from_env("YKDM_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .event_format(FgvFormat)
        .with_env_filter(filter)
        .try_init();
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

        let now = chrono::Local::now();
        writeln!(
            writer,
            "[{}] {} ; {} ; nivel={} {}",
            now.format("%d/%m/%Y"),
            now.format("%H:%M:%S"),
            captured.event.as_deref().unwrap_or("(sem evento)"),
            level_label(event.metadata().level()),
            captured.details.join(" "),
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
    fn first_event_field_wins_and_rest_become_details() {
        let mut c = Captured::default();
        c.push("event", "key.detected".into());
        c.push("serial", "20423633".into());
        assert_eq!(c.event.as_deref(), Some("key.detected"));
        assert_eq!(c.details, vec!["serial=20423633"]);
    }
}
