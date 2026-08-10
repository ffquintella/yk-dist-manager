//! Binary entry point: initialise logging, then hand over to the GUI.
//!
//! Which database opens is decided by [`YkDistApp::new`]: `$YKDM_DB` if set, then
//! the database last used, then the per-user default. Anything else is the
//! operator's choice on the database screen.

use std::path::PathBuf;

use yk_dist_manager::{YkDistApp, logging};

fn main() -> eframe::Result {
    logging::init();

    // macOS requires this before *anything* touches AVFoundation, and it has to
    // happen on the main thread while the operator is present to answer the
    // permission prompt. A no-op on other platforms and in builds without the
    // `camera` feature.
    yk_dist_manager::scan::preflight::initialise();

    let explicit = std::env::var("YKDM_DB")
        .ok()
        .map(|raw| raw.trim().to_owned())
        .filter(|raw| !raw.is_empty())
        .map(PathBuf::from);

    tracing::info!(
        event = "app.start",
        version = yk_dist_manager::VERSION,
        database = explicit
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(remembered or default)".into())
    );

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("YubiKey Distribution Manager"),
        ..Default::default()
    };

    eframe::run_native(
        "yk-dist-manager",
        options,
        Box::new(move |_cc| Ok(Box::new(YkDistApp::new(explicit)))),
    )
}
