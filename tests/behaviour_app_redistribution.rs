//! Behaviour test for handing a returned key to somebody else
//! (`features/distribution-records.md`; `features/key-lifecycle-and-revocation.md`
//! phase 6).
//!
//! A key that has come back is the commonest key to give out again, and the
//! lifecycle used to refuse it: `Returned → Distributed` was not in the table, so
//! the operator had to detour through *In stock* and *Bootstrapped*. The move is
//! allowed now, and it is a **reissue** — so it is gated on the same sanitisation
//! as every other way a key can reach a new holder, and refused *before* the new
//! hand-over is written.
//!
//! **One test in this file, deliberately** — it drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared by
//! every test in a binary.

use yk_dist_manager::YkDistApp;
use yk_dist_manager::device::reset::Applet;
use yk_dist_manager::domain::{
    BootstrapRun, Holder, KeyStatus, RunStatus, SerialSource, StepKind, StepOutcome, StepStatus,
    YubiKeyRecord,
};
use yk_dist_manager::store::{Store, StoreConfig};

const SERIAL: u32 = 20_423_634;

fn completed_run(serial: u32, holder: &Holder) -> BootstrapRun {
    let mut run = BootstrapRun::new(
        serial,
        Some(holder.id),
        "org-standard",
        "2",
        "felipe",
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
fn scenario_a_returned_key_goes_to_another_holder_once_it_is_sanitised() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
    }
    let database = home.path().join("keys.sqlite3");

    // Given a key bootstrapped for Ana, handed to her, and returned
    let ana = Holder::new("Ana Silva", "ana.silva@example.org", "ESI", "1").unwrap();
    let bruno = Holder::new("Bruno Costa", "bruno.costa@example.org", "ESI", "2").unwrap();
    {
        let store = Store::create_new(&StoreConfig::new(&database)).unwrap();
        store
            .upsert_key(&YubiKeyRecord::from_serial(
                SERIAL,
                SerialSource::ManualEntry,
            ))
            .unwrap();
        store.insert_holder(&ana).unwrap();
        store.insert_holder(&bruno).unwrap();
        store.insert_run(&completed_run(SERIAL, &ana)).unwrap();
        store
            .set_key_status(SERIAL, KeyStatus::Bootstrapped)
            .unwrap();
        let _ = store.close();
    }
    let mut app = YkDistApp::new(Some(database.clone()));
    assert!(app.store.is_some(), "{:?}", app.db_form.error);
    app.dist_form.link_last_run = false;
    app.dist_form.holder_index = app
        .holders
        .iter()
        .position(|h| h.id == ana.id)
        .expect("Ana is loaded");
    app.submit_distribution();
    assert_eq!(app.dist_form.error, None);
    let first = app.distributions[0].id;
    app.return_key(first, SERIAL);
    assert_eq!(status_of(&app, SERIAL), KeyStatus::Returned);

    // When the key is handed to Bruno while Ana's credentials are still on it
    app.dist_form.holder_index = app
        .holders
        .iter()
        .position(|h| h.id == bruno.id)
        .expect("Bruno is loaded");
    app.submit_distribution();

    // Then it is refused for what it is — a key not yet sanitised, not an illegal
    // transition — and nothing was written
    let refusal = app.dist_form.error.clone().expect("the reissue is refused");
    assert!(
        refusal.contains("FIDO2") && refusal.contains("factory default"),
        "{refusal}"
    );
    assert!(refusal.contains("Nothing was recorded"), "{refusal}");
    assert_eq!(
        app.distributions.len(),
        1,
        "no second hand-over was written"
    );
    assert_eq!(status_of(&app, SERIAL), KeyStatus::Returned);

    // When the FIDO2 applet is reset and the operator records that
    app.lifecycle.serial = Some(SERIAL);
    app.lifecycle.sanitised_applets.clear();
    app.toggle_sanitised_applet(Applet::Fido2, true);
    app.lifecycle.reference = "reset with ykman on the bench".into();
    app.record_manual_sanitisation();
    assert_eq!(app.lifecycle.error, None);

    // And the hand-over to Bruno is recorded again
    app.dist_form.error = None;
    app.submit_distribution();

    // Then it goes through whole: record, status and trail agree, and Ana's
    // closed hand-over is untouched beside Bruno's open one
    assert_eq!(app.dist_form.error, None);
    assert_eq!(app.distributions.len(), 2);
    assert_eq!(status_of(&app, SERIAL), KeyStatus::Distributed);
    let open: Vec<_> = app.distributions.iter().filter(|d| d.is_open()).collect();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].holder_id, bruno.id);
}
