//! Behaviour test for the step between the bootstrap and the hand-over
//! (`features/key-inventory.md`, "Lifecycle"; `features/distribution-records.md`).
//!
//! The lifecycle says a key is handed over *after* it is bootstrapped, and
//! `InStock → Distributed` is refused on purpose. Two things made that refusal
//! land in the wrong place: a run that completed left the key `In stock` — the
//! only thing that moved it was a button in Inventory nobody remembers — and the
//! Distribution screen wrote the hand-over first and asked the lifecycle second,
//! so the refusal arrived with the record already on the register.
//!
//! This is that story end to end: the refusal comes before anything is written,
//! and a completed run is what makes the key ready.
//!
//! **One test in this file, deliberately** — it drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared by
//! every test in a binary.

use yk_dist_manager::YkDistApp;
use yk_dist_manager::domain::{
    BootstrapRun, Holder, KeyStatus, RunStatus, SerialSource, StepKind, StepOutcome, StepStatus,
    YubiKeyRecord,
};
use yk_dist_manager::store::{Store, StoreConfig};

const SERIAL: u32 = 20_423_633;

/// A run that applied its one step and settled `Completed`.
fn completed_run(serial: u32, holder: &Holder, operator: &str) -> BootstrapRun {
    let mut run = BootstrapRun::new(
        serial,
        Some(holder.id),
        "org-standard",
        "2",
        operator,
        vec![StepOutcome {
            step_id: "fido2-pin".into(),
            kind: StepKind::Fido2Pin,
            status: StepStatus::Done,
            started_at: Some(chrono::Utc::now()),
            finished_at: Some(chrono::Utc::now()),
            detail: "PIN set".into(),
        }],
    );
    run.settle();
    assert_eq!(run.status, RunStatus::Completed);
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

fn status_of(app: &YkDistApp, serial: u32) -> KeyStatus {
    app.store
        .as_ref()
        .expect("a register is open")
        .key_by_serial(serial)
        .expect("the key reads back")
        .expect("the key is in the inventory")
        .status
}

#[test]
fn scenario_a_key_is_handed_over_once_a_run_has_made_it_ready() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
    }
    let database = home.path().join("keys.sqlite3");

    // Given a register with a key in stock and the person it is meant for
    {
        let store = Store::create_new(&StoreConfig::new(&database)).unwrap();
        store
            .upsert_key(&YubiKeyRecord::from_serial(
                SERIAL,
                SerialSource::ManualEntry,
            ))
            .unwrap();
        store
            .insert_holder(&Holder::new("Ana Silva", "ana.silva@example.org", "ESI", "1").unwrap())
            .unwrap();
        let _ = store.close();
    }

    let mut app = YkDistApp::new(Some(database.clone()));
    assert!(app.store.is_some(), "{:?}", app.db_form.error);
    app.dist_form.link_last_run = false;

    // When the operator records the hand-over of a key nothing has been applied to
    app.submit_distribution();

    // Then the refusal arrives before the record does: the register is unchanged,
    // and the operator is told what the key needs rather than being handed a
    // hand-over that half happened
    let refusal = app
        .dist_form
        .error
        .clone()
        .expect("the hand-over is refused");
    assert!(
        refusal.contains("In stock") && refusal.contains(&SERIAL.to_string()),
        "the refusal names the key and where it is: {refusal}"
    );
    assert!(
        refusal.contains("Nothing was recorded"),
        "the refusal says the register was not touched: {refusal}"
    );
    assert!(
        app.distributions.is_empty(),
        "a refused hand-over writes no distribution"
    );
    assert!(
        events(&app, "key.distributed").is_empty(),
        "and no trail entry claiming one"
    );
    assert_eq!(status_of(&app, SERIAL), KeyStatus::InStock);

    // When a run for that key ends without completing — a required step that never
    // ran is what the engine marks `Failed` and audits `bootstrap.incomplete`
    let holder = app.holders.first().cloned().expect("the holder is loaded");
    let mut half = completed_run(SERIAL, &holder, "felipe");
    half.steps[0].status = StepStatus::Failed;
    half.settle();
    assert_eq!(half.status, RunStatus::Failed);
    assert_eq!(app.settle_key_status(SERIAL, &half), Ok(false));

    // Then the key stays where it was: a run that did not finish the procedure does
    // not get to say the key is ready
    assert_eq!(status_of(&app, SERIAL), KeyStatus::InStock);
    assert!(events(&app, "key.status_changed").is_empty());

    // When a bootstrap run for that key completes
    let run = completed_run(SERIAL, &holder, "felipe");
    app.store
        .as_ref()
        .unwrap()
        .insert_run(&run)
        .expect("the run is on the register");
    assert_eq!(app.settle_key_status(SERIAL, &run), Ok(true));

    // Then the run is what moves the key, and the trail says so — the operator
    // does not have to remember a second click on another screen
    assert_eq!(status_of(&app, SERIAL), KeyStatus::Bootstrapped);
    let moved = events(&app, "key.status_changed");
    assert_eq!(moved.len(), 1, "one entry for one move: {moved:?}");
    assert!(
        moved[0].contains("to=bootstrapped") && moved[0].contains(&run.id.to_string()),
        "the entry names the state and the run that earned it: {}",
        moved[0]
    );

    // And a second completed run for the same key does not write the same fact
    // again — the key is already where that run would put it
    assert_eq!(app.settle_key_status(SERIAL, &run), Ok(false));
    assert_eq!(
        events(&app, "key.status_changed").len(),
        1,
        "a key already bootstrapped is not moved twice"
    );

    // When the hand-over is recorded now
    app.dist_form.error = None;
    app.submit_distribution();

    // Then it goes through, whole: the record, the status and the trail agree
    assert_eq!(app.dist_form.error, None, "nothing refuses a ready key");
    assert_eq!(app.distributions.len(), 1);
    assert_eq!(app.distributions[0].key_serial, SERIAL);
    assert_eq!(status_of(&app, SERIAL), KeyStatus::Distributed);
    assert_eq!(events(&app, "key.distributed").len(), 1);
}
