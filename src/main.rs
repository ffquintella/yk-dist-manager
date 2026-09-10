//! Binary entry point: initialise logging, then hand over to the GUI.
//!
//! Which database opens is decided by [`YkDistApp::new`]: `$YKDM_DB` if set, then
//! the database last used, then the per-user default. Anything else is the
//! operator's choice on the database screen.

// Without this, Windows links the binary as the default console subsystem, which
// allocates and briefly shows a console window before the egui window appears.
// Kept for debug builds so `--diagnose`/`--version`/`--help` stay visible in a
// terminal during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use yk_dist_manager::diagnostics::{self, Invocation};
use yk_dist_manager::logfile::stage;
use yk_dist_manager::{YkDistApp, logfile, logging};

fn main() -> eframe::Result {
    // Answer the informational switches before doing anything else: `--diagnose` in
    // particular must not need a database, a key or a window. It is also how
    // `make verify-bundle` interrogates the packaged application.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match diagnostics::parse_args(args.iter().map(String::as_str)) {
        Invocation::Gui => {}
        Invocation::Version => {
            // The build id rather than the version: a release is identified by
            // the commit it came from (`features/packaging-and-release.md` phase 2).
            println!("yk-dist-manager {}", yk_dist_manager::build_id());
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
        Invocation::WindowsService => {
            // The elevated FIDO2 helper
            // (`features/windows-elevated-helper.md` phase 3). Logging first,
            // because a service has no console to print to and the log is the
            // only place its life is visible.
            #[cfg(windows)]
            {
                logging::init();
                // A service has even less to print to than a windows-subsystem
                // GUI: no console, no window, and a service-control manager that
                // reports only an exit code. The hook is the only way a panic in
                // here says anything at all.
                logging::install_panic_hook();
                let code = yk_dist_manager::device::helper::service::run();
                std::process::exit(code);
            }
            #[cfg(not(windows))]
            {
                eprintln!(
                    "yk-dist-manager: the FIDO2 helper service exists only on Windows, where the \
                     operating system refuses an unelevated process a handle to a security key. \
                     On this platform the application talks to the key directly."
                );
                std::process::exit(2);
            }
        }
        Invocation::Unknown(arg) => {
            eprintln!("yk-dist-manager: unrecognised option `{arg}`");
            eprint!("{}", diagnostics::USAGE);
            std::process::exit(2);
        }
    }

    // Read before `logging::init()` writes the new one: this is the only
    // evidence of a previous start that died outright — a driver fault, an
    // `abort`, the operating system killing the process — none of which reaches
    // the panic hook installed below.
    let unfinished = logfile::previous_attempt();

    logging::init();
    logging::install_panic_hook();
    logfile::note_stage(stage::START);

    if let Some(marker) = &unfinished {
        let stage = logfile::stage_of(marker).unwrap_or("(unknown)");
        tracing::error!(
            event = "app.start.previous_incomplete",
            stage = stage,
            marker = marker.as_str(),
            explanation = logfile::explain(stage),
        );
    }

    let explicit = std::env::var("YKDM_DB")
        .ok()
        .map(|raw| raw.trim().to_owned())
        .filter(|raw| !raw.is_empty())
        .map(PathBuf::from);

    tracing::info!(
        event = "app.start",
        version = yk_dist_manager::VERSION,
        commit = yk_dist_manager::COMMIT,
        // Where this line was written, in the line itself: an operator reading
        // it in the panel can then be told a path they can open.
        log = logfile::path().display().to_string(),
        database = explicit
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(remembered or default)".into())
    );

    // macOS requires this before *anything* touches AVFoundation, and it has to
    // happen on the main thread while the operator is present to answer the
    // permission prompt. A no-op on other platforms and in builds without the
    // `camera` feature.
    logfile::note_stage(stage::CAMERA_PREFLIGHT);
    yk_dist_manager::scan::preflight::initialise();

    // Reopen where the operator left it. `size()` clamps, so a value from a
    // monitor that is no longer attached — or a NaN from a half-written settings
    // file — produces a usable window rather than one with no dimensions or one
    // whose close button is off-screen.
    logfile::note_stage(stage::SETTINGS);
    let settings = yk_dist_manager::settings::AppSettings::load();
    let remembered = settings.window;
    let (width, height) = remembered.size();

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([width, height])
        .with_min_inner_size([
            yk_dist_manager::settings::WindowState::MIN_WIDTH,
            yk_dist_manager::settings::WindowState::MIN_HEIGHT,
        ])
        .with_maximized(remembered.maximised)
        .with_title("YubiKey Distribution Manager");

    // A missing icon is cosmetic, so `window_icon` reporting a malformed blob
    // costs the operator a generic icon, not a launch. macOS bundles take theirs
    // from Info.plist instead; this is what Windows, Linux and `cargo run` show.
    if let Some(icon) = yk_dist_manager::branding::window_icon() {
        viewport = viewport.with_icon(icon);
    }

    let mut options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    // Which graphics backend to ask for, and whether the last start died asking
    // for one (`features/renderer-fallback.md`). The decision belongs to
    // `renderer::decide`, which is pure and tested; this reports it and hands it
    // to eframe. A driver that takes the process down inside `request_device`
    // reaches neither the error path below nor the panic hook, so the only
    // evidence is the marker the previous start left — and the only place to act
    // on it is here, before the attempt.
    let renderer = yk_dist_manager::renderer::decide(unfinished.as_deref(), settings.renderer);
    // Two macros rather than a level variable: `tracing` takes the level at
    // compile time. Which reasons are bad news is `worth_a_warning`'s to say.
    if renderer.worth_a_warning() {
        tracing::warn!(
            event = "app.renderer",
            renderer = renderer.renderer.slug(),
            detail = renderer.describe()
        );
    } else {
        tracing::info!(
            event = "app.renderer",
            renderer = renderer.renderer.slug(),
            detail = renderer.describe()
        );
    }
    yk_dist_manager::renderer::apply(&mut options.wgpu_options, renderer.renderer);

    // Everything from here is out of this process's hands — the windowing
    // system, the graphics driver, then the register — and it is where a launch
    // that produces no window dies. Each stage is on disk before it is entered,
    // so the next start can say which one it was.
    logfile::note_stage_with(stage::WINDOW, &[("renderer", renderer.renderer.slug())]);
    let started = eframe::run_native(
        "yk-dist-manager",
        options,
        Box::new(move |_cc| {
            logfile::note_stage(stage::APP);
            // There is a window, so this renderer works on this workstation.
            // Written down before the application loads the settings, because
            // the marker that got us here is about to be removed and would
            // otherwise be the only record — leaving the next start to
            // rediscover the fault from the top of the ladder.
            if renderer.worth_remembering() {
                let mut settings = yk_dist_manager::settings::AppSettings::load();
                if yk_dist_manager::renderer::remember_in(&mut settings, renderer.renderer) {
                    tracing::info!(
                        event = "app.renderer.remembered",
                        renderer = renderer.renderer.slug()
                    );
                    settings.save_quietly();
                }
            }
            let mut app = YkDistApp::new(explicit);
            // The constructor could only read what the settings file remembers.
            // This is what *this* start actually did, and the only place that
            // knows it: the marker it was decided from is removed two lines
            // below (`features/renderer-fallback.md` phase 6).
            app.renderer = renderer;
            // A window exists and the application behind it is built: this
            // start finished, whatever happens to the session now.
            logfile::finished();
            tracing::info!(event = "app.ready");
            Ok(Box::new(app))
        }),
    );

    match &started {
        Ok(()) => tracing::info!(event = "app.stopped"),
        Err(problem) => tracing::error!(
            event = "app.window.failed",
            // The one failure `main` can still report: `run_native` returns
            // rather than panicking when there is no display, no usable
            // graphics backend, or no permission to open a window.
            detail = problem.to_string()
        ),
    }
    // A start that got no window is over too; leaving the marker would make the
    // *next* launch report a failure that this line already recorded properly.
    logfile::finished();

    started
}
