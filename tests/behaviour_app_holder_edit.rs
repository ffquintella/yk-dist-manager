//! Behaviour test for correcting a holder record from the Holders screen
//! (`features/holder-registry.md` phase 8).
//!
//! A register that can only ever *add* a person makes a typo permanent: the
//! upsert on the e-mail can fix a name, but not the address it matches on, so
//! the mistyped record stays and a second one appears beside it. This is the
//! edit path end to end, through `YkDistApp` the way the screen drives it — the
//! correction, the refusal when the new address is somebody else's, and the
//! trail entry that says which fields moved.
//!
//! **One test in this file, deliberately** — it drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared
//! by every test in a binary.

use yk_dist_manager::YkDistApp;
use yk_dist_manager::domain::Holder;
use yk_dist_manager::store::{Store, StoreConfig};

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
fn scenario_a_holder_record_is_corrected_after_it_was_saved() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
    }
    let database = home.path().join("keys.sqlite3");

    // Given a register holding two people, one of them entered with a mistyped
    // name and address
    {
        let store = Store::create_new(&StoreConfig::new(&database)).unwrap();
        store
            .insert_holder(
                &Holder::new("Ana Sliva", "ana.sliva@example.org", "ESI", "1")
                    .unwrap()
                    .with_optional("123.456.789-00", "+55 21 0000-0000", "Rua A, 1")
                    .unwrap(),
            )
            .unwrap();
        store
            .insert_holder(
                &Holder::new("Bruno Lima", "bruno.lima@example.org", "ESI", "2").unwrap(),
            )
            .unwrap();
        let _ = store.close();
    }

    let mut app = YkDistApp::new(Some(database.clone()));
    assert!(app.store.is_some(), "{:?}", app.db_form.error);
    let ana = app
        .holders
        .iter()
        .find(|holder| holder.email == "ana.sliva@example.org")
        .cloned()
        .expect("the mistyped record is loaded");

    // When the operator opens that record for editing
    app.begin_holder_edit(ana.id);

    // Then the form arrives filled in with what is stored — an edit that started
    // from a blank form would blank whatever the operator did not retype
    assert_eq!(app.holder_form.full_name, "Ana Sliva");
    assert_eq!(app.holder_form.email, "ana.sliva@example.org");
    assert_eq!(app.holder_form.unit, "ESI");
    assert_eq!(app.holder_form.registration, "1");
    assert_eq!(app.holder_form.identification_number, "123.456.789-00");
    assert_eq!(app.holder_form.phone, "+55 21 0000-0000");
    assert_eq!(app.holder_form.address, "Rua A, 1");
    assert!(
        app.holder_form.editing.is_some(),
        "the form is in edit mode"
    );

    // When they save it untouched, nothing is written and nothing is claimed
    app.submit_holder();
    assert!(
        events(&app, "holder.updated").is_empty(),
        "an edit that changed nothing does not appear in the trail"
    );
    assert!(app.status.contains("nothing to save"), "{}", app.status);

    // When they try to move Ana onto Bruno's address
    app.holder_form.email = "bruno.lima@example.org".into();
    app.submit_holder();

    // Then it is refused, naming who holds that address, and neither record moved
    let refusal = app
        .holder_form
        .error
        .clone()
        .expect("the correction is refused");
    assert!(
        refusal.contains("Bruno Lima") && refusal.contains("bruno.lima@example.org"),
        "the refusal names the address and its owner: {refusal}"
    );
    assert!(
        refusal.contains("Nothing was saved"),
        "and says the register was not touched: {refusal}"
    );
    assert!(events(&app, "holder.updated").is_empty());
    assert!(
        app.holder_form.editing.is_some(),
        "the operator is left in the form with their typing, not thrown out of it"
    );

    // When they correct the spelling instead — name, address, unit — and clear an
    // optional field that was wrong
    app.holder_form.full_name = "Ana Silva".into();
    app.holder_form.email = "ana.silva@example.org".into();
    app.holder_form.unit = "DCI".into();
    app.holder_form.phone = String::new();
    app.submit_holder();

    // Then the record is corrected in place: one person, the same id, so every
    // hand-over and run that names her still does
    assert_eq!(app.holder_form.error, None);
    assert_eq!(app.holders.len(), 2, "still two people on the register");
    let corrected = app
        .holders
        .iter()
        .find(|holder| holder.id == ana.id)
        .expect("the same record, corrected");
    assert_eq!(corrected.full_name, "Ana Silva");
    assert_eq!(corrected.email, "ana.silva@example.org");
    assert_eq!(corrected.unit, "DCI");
    assert_eq!(corrected.created_at, ana.created_at);
    assert_eq!(
        corrected.identification_number, "123.456.789-00",
        "what was not touched is kept"
    );
    assert_eq!(corrected.phone, "", "and what was emptied is cleared");

    // And the trail says which fields moved, naming the address old and new,
    // because that is the value the certificate on her key was issued against
    let updates = events(&app, "holder.updated");
    assert_eq!(
        updates.len(),
        1,
        "one entry for one correction: {updates:?}"
    );
    let detail = &updates[0];
    assert!(
        detail.contains("name")
            && detail.contains("e-mail ana.sliva@example.org -> ana.silva@example.org")
            && detail.contains("unit")
            && detail.contains("phone"),
        "the entry names what changed: {detail}"
    );
    assert!(
        !detail.contains("123.456.789-00"),
        "and never spells out an identification number: {detail}"
    );

    // And the form is back to registering the next person
    assert!(app.holder_form.editing.is_none());
    assert!(app.holder_form.full_name.is_empty());
}
