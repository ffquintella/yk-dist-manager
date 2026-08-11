//! Binary entry point: initialise logging, then hand over to the GUI.
//!
//! Which database opens is decided by [`YkDistApp::new`]: `$YKDM_DB` if set, then
//! the database last used, then the per-user default. Anything else is the
//! operator's choice on the database screen.

use std::path::PathBuf;

use yk_dist_manager::diagnostics::{self, Invocation};
use yk_dist_manager::{YkDistApp, logging};

fn main() -> eframe::Result {
    // Answer the informational switches before doing anything else: `--diagnose` in
    // particular must not need a database, a key or a window. It is also how
    // `make verify-bundle` interrogates the packaged application.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match diagnostics::parse_args(args.iter().map(String::as_str)) {
        Invocation::Gui => {}
        Invocation::Version => {
            println!("yk-dist-manager {}", yk_dist_manager::VERSION);
            return Ok(());
        }
        Invocation::Help => {
            print!("{}", diagnostics::USAGE);
            return Ok(());
        }
        Invocation::Diagnose => {
            print!("{}", diagnostics::Report::gather().render());
            return Ok(());
        }
        Invocation::Unknown(arg) => {
            eprintln!("yk-dist-manager: unrecognised option `{arg}`");
            eprint!("{}", diagnostics::USAGE);
            std::process::exit(2);
        }
    }

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

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([1180.0, 760.0])
        .with_min_inner_size([900.0, 600.0])
        .with_title("YubiKey Distribution Manager");

    // A missing icon is cosmetic, so `window_icon` reporting a malformed blob
    // costs the operator a generic icon, not a launch. macOS bundles take theirs
    // from Info.plist instead; this is what Windows, Linux and `cargo run` show.
    if let Some(icon) = yk_dist_manager::branding::window_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "yk-dist-manager",
        options,
        Box::new(move |_cc| Ok(Box::new(YkDistApp::new(explicit)))),
    )
}
