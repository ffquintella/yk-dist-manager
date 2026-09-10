//! Behaviour: a start-up that fails leaves a record the next start can read.
//!
//! `features/logging.md` phase 2. The scenario behind every test here is the
//! support call this feature exists for — *"I click it and nothing happens"* —
//! where the operator has no console, no window, and no idea where to look.
//! What answers it is the pair the application writes on the way up: the log
//! file, which holds every line the dying process managed to emit, and the
//! start-up marker, which holds the stage it never got past even when the
//! process died too abruptly to emit anything at all.
//!
//! The sequence under test is `src/main.rs`'s, reproduced here because the real
//! one ends in `eframe::run_native` and a test binary has no display.

use std::path::Path;

use yk_dist_manager::logbuf::LogBuffer;
use yk_dist_manager::logfile::{self, LogFile, stage};
use yk_dist_manager::logging::{FgvFormat, Sinks};
use yk_dist_manager::renderer::{self, Reason, Renderer};

fn temp() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

/// One launch, in the order `main` does it, stopping at `dies_at`.
///
/// `None` runs the whole sequence through to a window and an application, which
/// is the only path that clears the marker.
fn launch(directory: &Path, dies_at: Option<&str>) {
    let file = LogFile::open(directory).expect("the log file opens");
    let subscriber = tracing_subscriber::fmt()
        .event_format(FgvFormat)
        .with_writer(Sinks::new(Some(file), LogBuffer::new()))
        .with_max_level(tracing::Level::TRACE)
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        // Read before this launch overwrites it — `main`'s one ordering rule.
        let unfinished = logfile::previous_attempt_in(directory);
        logfile::note_stage_in(directory, stage::START).expect("the marker is writable");
        if let Some(marker) = &unfinished {
            let stage = logfile::stage_of(marker).unwrap_or("(unknown)");
            tracing::error!(
                event = "app.start.previous_incomplete",
                stage = stage,
                explanation = logfile::explain(stage),
            );
        }
        tracing::info!(event = "app.start", version = yk_dist_manager::VERSION);

        for reached in [
            stage::CAMERA_PREFLIGHT,
            stage::SETTINGS,
            stage::WINDOW,
            stage::APP,
        ] {
            logfile::note_stage_in(directory, reached).expect("the marker is writable");
            if dies_at == Some(reached) {
                return; // the process is gone: nothing else is written
            }
        }
        logfile::finished_in(directory);
        tracing::info!(event = "app.ready");
    });
}

/// The arm of `main` that runs when `eframe::run_native` returns an error: the
/// windowing system refused a window rather than the process dying inside it.
///
/// The marker is deliberately left where it is, which is what lets the next
/// start step down the renderer ladder (`features/renderer-fallback.md`).
fn launch_refused(directory: &Path, renderer: Renderer) {
    let file = LogFile::open(directory).expect("the log file opens");
    let subscriber = tracing_subscriber::fmt()
        .event_format(FgvFormat)
        .with_writer(Sinks::new(Some(file), LogBuffer::new()))
        .with_max_level(tracing::Level::TRACE)
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        logfile::note_stage_with_in(directory, stage::WINDOW, &[("renderer", renderer.slug())])
            .expect("the marker is writable");
        tracing::error!(
            event = "app.window.failed",
            detail = "no available graphics adapter"
        );
    });
}

fn log_body(directory: &Path) -> String {
    std::fs::read_to_string(directory.join(logfile::FILE_NAME)).expect("the log file is readable")
}

#[test]
fn a_start_that_never_reached_a_window_is_reported_at_the_next_one() {
    // Given a launch that died where a graphics driver dies
    let home = temp();
    launch(home.path(), Some(stage::WINDOW));

    // When the operator tries again
    launch(home.path(), None);

    // Then the second launch's log says so, names the stage, and explains it
    let body = log_body(home.path());
    assert!(
        body.contains("app.start.previous_incomplete"),
        "the failure of the launch before is an event of its own: {body}"
    );
    assert!(body.contains(&format!("stage={}", stage::WINDOW)), "{body}");
    assert!(
        body.contains("graphics driver"),
        "and it is explained in words an operator can act on: {body}"
    );
    assert!(
        body.contains("nivel=Erro"),
        "a launch that failed is an error, not a note: {body}"
    );
}

#[test]
fn a_start_that_succeeded_does_not_accuse_the_next_one() {
    // Given two clean launches
    let home = temp();
    launch(home.path(), None);
    launch(home.path(), None);

    // Then neither ever reported the other
    let body = log_body(home.path());
    assert!(!body.contains("previous_incomplete"), "{body}");
    assert_eq!(
        body.matches("app.ready").count(),
        2,
        "both reached a window: {body}"
    );
    assert!(
        logfile::previous_attempt_in(home.path()).is_none(),
        "and nothing is left on disk to mislead the third"
    );
}

#[test]
fn the_marker_names_the_last_stage_the_launch_reached() {
    // The distinction the operator needs: a launch that dies opening the
    // register is a different problem from one that dies asking for a window,
    // and both look identical from the outside — no window appears.
    for dies_at in [
        stage::CAMERA_PREFLIGHT,
        stage::SETTINGS,
        stage::WINDOW,
        stage::APP,
    ] {
        let home = temp();
        launch(home.path(), Some(dies_at));

        let marker = logfile::previous_attempt_in(home.path()).expect("a marker was left");
        assert_eq!(logfile::stage_of(&marker), Some(dies_at), "{marker}");
        assert!(
            !logfile::explain(dies_at).contains("does not recognise"),
            "every stage a launch can die at has an explanation: {dies_at}"
        );
    }
}

#[test]
fn what_the_dying_launch_managed_to_log_is_on_disk_afterwards() {
    // The other half: the marker says *where*, the file says *what*. Unbuffered
    // writes are what makes this hold — the last line before the process goes is
    // the one that matters most.
    let home = temp();
    let file = LogFile::open(home.path()).expect("opens");
    let subscriber = tracing_subscriber::fmt()
        .event_format(FgvFormat)
        .with_writer(Sinks::new(Some(file), LogBuffer::new()))
        .with_max_level(tracing::Level::TRACE)
        .finish();

    // Given a launch that failed for a reason it could name
    tracing::subscriber::with_default(subscriber, || {
        logfile::note_stage_in(home.path(), stage::WINDOW).unwrap();
        tracing::error!(
            event = "app.window.failed",
            detail = "no available graphics adapter"
        );
    });

    // Then the reason outlived the process that wrote it
    let body = log_body(home.path());
    assert!(body.contains("app.window.failed"), "{body}");
    assert!(body.contains("no available graphics adapter"), "{body}");
}

#[test]
fn the_diagnostic_report_names_a_start_that_never_finished() {
    // Where an operator is actually sent: `--diagnose`, pasted into a ticket.
    // Every other line in that report describes a process that *did* start.
    let home = temp();
    logfile::note_stage_in(home.path(), stage::APP).unwrap();
    let marker = logfile::previous_attempt_in(home.path()).expect("a marker was left");

    let stage = logfile::stage_of(&marker).expect("the marker names its stage");
    assert!(
        logfile::explain(stage).contains("register"),
        "a launch that died building the application points at the register: {}",
        logfile::explain(stage)
    );

    let described = logfile::describe_in(home.path());
    assert!(
        described.contains("nothing written yet"),
        "a marker with no log beside it says so rather than inventing a size: {described}"
    );
}

#[test]
fn a_window_the_platform_refused_leaves_the_marker_so_the_next_start_steps_down() {
    // Given a Windows workstation asked for Direct3D 12 — the first rung there —
    // on which the platform refused a window outright rather than dying: no
    // driver for that backend, so wgpu found no adapter and `run_native`
    // returned an error it could name.
    let home = temp();
    launch_refused(home.path(), Renderer::Dx12);

    // Then the launch said why, in the log, as it always did
    let body = log_body(home.path());
    assert!(body.contains("app.window.failed"), "{body}");

    // And the marker is still on disk, naming the backend that was refused —
    // which it was not, before Direct3D 12 became the rung a Windows start asks
    // for first. A refused window and a window that killed the process are the
    // same fact when the question is which backend to try next.
    let marker = logfile::previous_attempt_in(home.path()).expect("the marker was left");
    assert_eq!(logfile::stage_of(&marker), Some(stage::WINDOW), "{marker}");
    assert_eq!(
        logfile::field_of(&marker, "renderer"),
        Some("dx12"),
        "{marker}"
    );

    // When the operator tries again on that workstation's ladder
    let decision = renderer::decide_on(
        &[Renderer::Dx12, Renderer::Gl],
        None,
        None,
        Some(&marker),
        Renderer::Automatic,
    );

    // Then this start asks for something else, rather than repeating a request
    // the platform has already refused once
    assert_eq!(decision.renderer, Renderer::Gl);
    assert_eq!(decision.reason, Reason::SteppedDown);
    assert!(decision.worth_a_warning());
}

#[test]
fn a_window_that_was_created_and_then_failed_does_not_accuse_the_next_start() {
    // The other half of leaving the marker: a session that got its window and
    // fell over an hour later must not read as a start-up failure. Nothing has
    // to be undone for that, because the marker was removed the moment the
    // window existed — this is the assertion that it stays that way.
    let home = temp();
    launch(home.path(), None);
    launch_refused(home.path(), Renderer::Gl); // a late failure writes no marker of its own
    logfile::finished_in(home.path());

    launch(home.path(), None);
    let body = log_body(home.path());
    assert!(!body.contains("previous_incomplete"), "{body}");
}
