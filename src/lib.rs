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
//! | [`domain`] | Records: keys, holders, distribution events, bootstrap runs |
//! | [`device`] | YubiKey discovery / inspection behind a mockable trait |
//! | [`template`] | Bootstrap templates, variable rendering, command planning |
//! | [`store`] | Persistence: one SQLite file, optionally password-protected |
//! | [`settings`] | Which database to open, and the recent ones |
//! | [`scan`] | Reading a serial from a barcode (label or camera) |
//! | [`term`] | Consignment terms: multilingual templates and rendering |
//! | [`audit`] | Append-only, hash-chained audit trail |
//! | [`logging`] | The single logging entry point for the whole app |
//! | [`app`] / [`ui`] | egui shell and screens |
//!
//! See [`roadmap.md`](../../../roadmap.md) for the implementation plan and
//! `features/` for the per-feature specifications.

pub mod app;
pub mod audit;
pub mod device;
pub mod domain;
pub mod logging;
pub mod paths;
pub mod scan;
pub mod settings;
pub mod store;
pub mod template;
pub mod term;
pub mod ui;

pub use app::YkDistApp;

/// Crate version, surfaced in the GUI status bar and in audit entries.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
