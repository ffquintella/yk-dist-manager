//! # yk-dist-manager
//!
//! Desktop tool (egui) that tracks **YubiKey distribution** and drives a
//! **templated bootstrap procedure** for each key handed to a person.
//!
//! The crate is split so that everything except [`app`] and [`ui`] is
//! headless and unit-testable without a display or a physical key:
//!
//! | Module | Role |
//! |---|---|
//! | [`branding`] | The application icon, embedded for every platform |
//! | [`browse`] | Searching, sorting and paging the tables an operator reads |
//! | [`domain`] | Records: keys, holders, distribution events, bootstrap runs |
//! | [`device`] | YubiKey discovery / inspection behind a mockable trait |
//! | [`template`] | Bootstrap templates, variable rendering, command planning |
//! | [`store`] | Persistence: one SQLite file, optionally password-protected |
//! | [`settings`] | Which database to open, and the recent ones |
//! | [`scan`] | Reading a serial from a barcode (label or camera) |
//! | [`san`] | The certificate's rfc822Name: how it is produced, and how to check it |
//! | [`password`] | The database password: how strong, and how slowly retried |
//! | [`secret`] | The secrets a bootstrap sets: generated, shown once, wiped |
//! | [`term`] | Consignment terms: multilingual templates and rendering |
//! | [`pdf`] | The PDF a term is printed and signed on, written without a dependency |
//! | [`versioning`] | "What number does the next edit get?", shared by both |
//! | [`diagnostics`] | `--diagnose`: what this build is and what it can reach |
//! | [`audit`] | Append-only, hash-chained audit trail |
//! | [`status`] | How loudly the status bar reports the last outcome |
//! | [`logbuf`] | The last N log lines, for the panel an operator can copy from |
//! | [`logging`] | The single logging entry point for the whole app |
//! | [`app`] / [`ui`] | egui shell and screens |
//!
//! See [`roadmap.md`](../../../roadmap.md) for the implementation plan and
//! `features/` for the per-feature specifications.

pub mod app;
pub mod audit;
pub mod bootstrap;
pub mod branding;
pub mod browse;
pub mod device;
pub mod diagnostics;
pub mod domain;
pub mod envelope;
pub mod logbuf;
pub mod logging;
pub mod password;
pub mod paths;
pub mod pdf;
pub mod san;
pub mod scan;
pub mod secret;
pub mod settings;
pub mod status;
pub mod store;
pub mod template;
pub mod term;
pub mod ui;
pub mod versioning;

pub use app::YkDistApp;

/// Crate version, surfaced in the GUI status bar and in audit entries.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
