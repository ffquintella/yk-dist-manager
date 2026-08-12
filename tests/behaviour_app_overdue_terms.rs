//! Behaviour test for the one part of the signature state machine that is not a
//! pure function: recording `receipt.pending_overdue`
//! (`features/receipts-and-terms.md` phase 4).
//!
//! A term going overdue has no click behind it — nobody *does* anything to make a
//! day pass — so there is no natural moment to audit it, and the naive placements
//! are both wrong: from the paint pass it would write an entry per frame, and on
//! every open it would rewrite the same fact every session until the trail is
//! mostly duplicates.
//!
//! So the trail itself is the marker: the existing entries are read, and only a
//! hand-over not already named is written. This test is what says that holds across
//! a restart, which is the case a single session cannot show.
//!
//! **One test in this file, deliberately** — it drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared by
//! every test in a binary.

use yk_dist_manager::YkDistApp;
use yk_dist_manager::domain::SerialSource;
use yk_dist_manager::domain::{DeliveryMethod, DistributionRecord, Holder, YubiKeyRecord};
use yk_dist_manager::store::{Store, StoreConfig};

/// A hand-over `days` ago with nothing recorded against it.
fn unsigned(key: &YubiKeyRecord, holder: &Holder, days: i64) -> DistributionRecord {
    DistributionRecord {
        id: uuid::Uuid::new_v4(),
        key_id: key.id,
        key_serial: key.serial,
        holder_id: holder.id,
        holder_display: holder.display(),
        distributed_at: chrono::Utc::now() - chrono::Duration::days(days),
        distributed_by: "felipe".into(),
        method: DeliveryMethod::Post,
        receipt_ref: String::new(),
        bootstrap_run_id: None,
        returned_at: None,
        returned_to: None,
        notes: String::new(),
    }
}

fn events(app: &YkDistApp, event: &str) -> Vec<String> {
    app.store
        .as_ref()
        .expect("a register is open")
        .audit_entries(200)
        .expect("the trail reads back")
        .into_iter()
        .filter(|entry| entry.event == event)
        .map(|entry| entry.target)
        .collect()
}

#[test]
fn scenario_an_overdue_term_is_recorded_once_and_not_again_on_the_next_open() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
    }
    let database = home.path().join("keys.sqlite3");

    // Given a register with two hand-overs whose terms never came back: one from a
    // month ago, one from yesterday
    let (old_id, fresh_id) = {
        let store = Store::create_new(&StoreConfig::new(&database)).unwrap();
        let key = YubiKeyRecord::from_serial(20_423_633, SerialSource::ManualEntry);
        store.upsert_key(&key).unwrap();
        let second = YubiKeyRecord::from_serial(31_000_001, SerialSource::ManualEntry);
        store.upsert_key(&second).unwrap();
        let holder = Holder::new("Ana Silva", "ana.silva@example.org", "ESI", "1").unwrap();
        store.insert_holder(&holder).unwrap();

        let old = unsigned(&key, &holder, 30);
        let fresh = unsigned(&second, &holder, 1);
        store.insert_distribution(&old).unwrap();
        store.insert_distribution(&fresh).unwrap();
        let _ = store.close();
        (old.id, fresh.id)
    };

    // When the application opens it
    let mut app = YkDistApp::new(Some(database.clone()));
    assert!(app.store.is_some(), "{:?}", app.db_form.error);

    // Then the overdue one is on the record and the fresh one is not: a warning
    // that fired for every unsigned term the day it was handed over would be a
    // warning nobody reads
    let recorded = events(&app, "receipt.pending_overdue");
    assert_eq!(
        recorded,
        vec![format!("distribution:{old_id}")],
        "only the one past the threshold"
    );
    assert!(
        !recorded.contains(&format!("distribution:{fresh_id}")),
        "yesterday's hand-over is pending, not overdue"
    );

    // And the states agree with the trail
    let old = app
        .distributions
        .iter()
        .find(|d| d.id == old_id)
        .cloned()
        .unwrap();
    let fresh = app
        .distributions
        .iter()
        .find(|d| d.id == fresh_id)
        .cloned()
        .unwrap();
    assert!(app.signature_state(&old).is_overdue());
    assert!(!app.signature_state(&fresh).is_overdue());
    assert!(app.signature_state(&fresh).needs_chasing());

    let tally = app.outstanding_paperwork();
    assert_eq!(tally.overdue, 1);
    assert_eq!(tally.pending, 1);
    assert!(tally.needs_attention());
    assert!(
        tally
            .describe()
            .is_some_and(|line| line.contains("overdue")),
        "{:?}",
        tally.describe()
    );

    // When the check runs again in the same session — as it does after a return,
    // or after the threshold is changed in Settings
    app.check_overdue_signatures();
    app.check_overdue_signatures();
    assert_eq!(
        events(&app, "receipt.pending_overdue").len(),
        1,
        "the same fact must not be written twice"
    );

    // And when the register is closed and opened again — the case a single session
    // cannot show, and the one where a naive implementation duplicates
    app.db_request = Some(yk_dist_manager::app::DbRequest::Close);
    app.handle_db_request();
    let mut app = YkDistApp::new(Some(database.clone()));
    assert!(app.store.is_some(), "{:?}", app.db_form.error);
    assert_eq!(
        events(&app, "receipt.pending_overdue").len(),
        1,
        "a restart must not re-record what the trail already holds"
    );

    // When the operator records the unit's own document reference for the overdue
    // one — a unit that files paper elsewhere has answered the question
    {
        let store = app.store.as_ref().unwrap();
        store
            .set_receipt_ref(old_id, "processo 2026/114")
            .expect("the reference is recorded");
    }
    app.refresh();
    let old = app
        .distributions
        .iter()
        .find(|d| d.id == old_id)
        .cloned()
        .unwrap();

    // Then it is settled, and the tally has moved
    assert!(
        app.signature_state(&old).is_settled(),
        "{:?}",
        app.signature_state(&old)
    );
    assert_eq!(app.outstanding_paperwork().overdue, 0);

    // And the entry that recorded it as overdue stays: the audit trail is not
    // rewritten because the situation improved. It happened.
    assert_eq!(events(&app, "receipt.pending_overdue").len(), 1);
    assert!(
        !app.store.as_ref().unwrap().chain_status().is_broken(),
        "the chain still verifies"
    );

    // And a unit that does not use terms is not nagged at all: nothing new is
    // recorded, and every state settles.
    app.settings.signatures.required = false;
    app.check_overdue_signatures();
    assert_eq!(events(&app, "receipt.pending_overdue").len(), 1);
    let fresh = app
        .distributions
        .iter()
        .find(|d| d.id == fresh_id)
        .cloned()
        .unwrap();
    assert!(app.signature_state(&fresh).is_settled());
    assert_eq!(app.outstanding_paperwork().describe(), None);
}
