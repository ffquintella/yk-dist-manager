//! Behaviour test for closing an unfinished run
//! (`features/gui-bootstrap-wizard.md` phase 5).
//!
//! *Unfinished runs on this register* only ever lost a run when somebody finished
//! it. A run that will never be finished — the certificate request was cancelled,
//! the key went back in the box, the procedure was started twice by mistake — sat
//! there for the life of the register, beside the ones that really are outstanding,
//! until the list stopped being worth reading.
//!
//! Closing one is **not** a delete, and this is the test that says so: the run, its
//! steps and their outcomes stay exactly where they were, and what changes is the
//! status, the trail entry naming who closed it, and the offer to resume.
//!
//! **One test in this file, deliberately** — it drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared by
//! every test in a binary.

use yk_dist_manager::YkDistApp;
use yk_dist_manager::domain::{
    BootstrapRun, Holder, RunStatus, SerialSource, StepKind, StepOutcome, StepStatus, YubiKeyRecord,
};
use yk_dist_manager::store::{Store, StoreConfig};

const SERIAL: u32 = 20_423_633;

/// A run that set the PIN and then stopped: the certificate never came back, so
/// the import is still pending and the run is still open.
fn open_run(holder: &Holder) -> BootstrapRun {
    let mut run = BootstrapRun::new(
        SERIAL,
        Some(holder.id),
        "org-standard",
        "2",
        "felipe",
        vec![
            StepOutcome {
                step_id: "fido2-pin".into(),
                kind: StepKind::Fido2Pin,
                status: StepStatus::Done,
                started_at: Some(chrono::Utc::now()),
                finished_at: Some(chrono::Utc::now()),
                detail: "PIN set".into(),
            },
            StepOutcome::planned(
                "piv-cert-import",
                StepKind::PivCertImport,
                "awaiting the issued certificate",
            ),
        ],
    );
    run.settle();
    assert_eq!(run.status, RunStatus::Running, "one step is still pending");
    run
}

fn events(app: &YkDistApp, event: &str) -> Vec<String> {
    app.store
        .as_ref()
        .expect("a register is open")
        .audit_entries(200)
        .expect("the trail reads back")
        .into_iter()
        .filter(|entry| entry.event == event)
        .map(|entry| entry.details)
        .collect()
}

#[test]
fn scenario_an_unfinished_run_is_closed_and_stays_on_the_register() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
    }
    let database = home.path().join("keys.sqlite3");

    // Given a register with a run that stopped waiting for a certificate
    let run_id = {
        let store = Store::create_new(&StoreConfig::new(&database)).unwrap();
        store
            .upsert_key(&YubiKeyRecord::from_serial(
                SERIAL,
                SerialSource::ManualEntry,
            ))
            .unwrap();
        let holder = Holder::new("Ana Silva", "ana.silva@example.org", "ESI", "1").unwrap();
        store.insert_holder(&holder).unwrap();
        let run = open_run(&holder);
        store.insert_run(&run).unwrap();
        let _ = store.close();
        run.id
    };

    let mut app = YkDistApp::new(Some(database.clone()));
    assert!(app.store.is_some(), "{:?}", app.db_form.error);
    assert_eq!(
        yk_dist_manager::bootstrap::resumable(&app.runs).len(),
        1,
        "the wizard offers it while it is open"
    );

    // When the operator asks to close it, nothing is written by the asking: the
    // confirmation is a card on screen, not the act
    app.ask_abandon_run(run_id);
    assert_eq!(app.wizard.pending_abandon, Some(run_id));
    assert_eq!(
        app.runs
            .iter()
            .find(|run| run.id == run_id)
            .expect("still there")
            .status,
        RunStatus::Running,
        "asking changed nothing"
    );
    assert!(events(&app, "bootstrap.abandoned").is_empty());

    // And they change their mind
    app.cancel_abandon_run();
    assert!(app.wizard.pending_abandon.is_none());
    assert_eq!(
        yk_dist_manager::bootstrap::resumable(&app.runs).len(),
        1,
        "a cancelled confirmation leaves the run where it was"
    );

    // When they go through with it
    app.ask_abandon_run(run_id);
    app.abandon_run(run_id);

    // Then the run is off the list, and still on the register
    assert!(app.wizard.pending_abandon.is_none());
    assert!(app.wizard.error.is_none(), "{:?}", app.wizard.error);
    assert!(
        yk_dist_manager::bootstrap::resumable(&app.runs).is_empty(),
        "it is no longer offered as unfinished business"
    );
    let closed = app
        .runs
        .iter()
        .find(|run| run.id == run_id)
        .expect("the run is kept — closing is not deleting");
    assert_eq!(closed.status, RunStatus::Aborted);
    assert!(closed.finished_at.is_some());

    // And every step keeps the outcome it actually reached: the one that ran is
    // still Done, and the one that never ran is still pending rather than being
    // rewritten as skipped by a button nobody pointed at the key
    assert_eq!(closed.steps.len(), 2);
    assert_eq!(closed.steps[0].status, StepStatus::Done);
    assert_eq!(closed.steps[1].status, StepStatus::Pending);

    // And the trail says who closed it and what state it was in
    let recorded = events(&app, "bootstrap.abandoned");
    assert_eq!(recorded.len(), 1, "one entry, once");
    let detail = &recorded[0];
    assert!(
        detail.contains("org-standard")
            && detail.contains("done=1")
            && detail.contains("pending=1"),
        "the entry says what was left behind: {detail}"
    );

    // And it is the *database* that says so, not a cached list the screen dropped
    // it from: `refresh` re-read the register, and reading it again agrees
    let stored = app
        .store
        .as_ref()
        .expect("a register is open")
        .runs()
        .expect("the runs read back");
    assert_eq!(stored.len(), 1, "nothing was removed");
    assert_eq!(stored[0].status, RunStatus::Aborted);
    assert_eq!(stored[0].steps.len(), 2);

    // And a run that is already closed cannot be closed again
    app.abandon_run(run_id);
    assert!(
        app.wizard
            .error
            .as_deref()
            .is_some_and(|e| e.contains("already abandoned")),
        "{:?}",
        app.wizard.error
    );
    assert_eq!(
        events(&app, "bootstrap.abandoned").len(),
        1,
        "and writes no second entry"
    );
}
