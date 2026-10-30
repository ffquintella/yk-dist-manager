//! Behaviour test for putting a returned key back in stock
//! (`features/key-inventory.md`).
//!
//! The inventory offers *return to stock* on a returned key, so it can be handed
//! out again by the ordinary route. It is a reissue: refused until sanitised, and
//! audited once it goes through.
//!
//! **One test in this file, deliberately** — `YkDistApp` reads process-global
//! environment variables.

use yk_dist_manager::YkDistApp;
use yk_dist_manager::domain::{KeyStatus, SerialSource, YubiKeyRecord};
use yk_dist_manager::store::{Store, StoreConfig};

const SERIAL: u32 = 20_423_635;

fn status_of(app: &YkDistApp) -> KeyStatus {
    app.store
        .as_ref()
        .unwrap()
        .key_by_serial(SERIAL)
        .unwrap()
        .unwrap()
        .status
}

#[test]
fn scenario_a_returned_key_goes_back_to_stock_and_is_audited() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
    }
    let database = home.path().join("keys.sqlite3");

    // Given a returned key that never carried a bootstrap
    {
        let store = Store::create_new(&StoreConfig::new(&database)).unwrap();
        store
            .upsert_key(&YubiKeyRecord::from_serial(
                SERIAL,
                SerialSource::ManualEntry,
            ))
            .unwrap();
        store
            .set_key_status(SERIAL, KeyStatus::Bootstrapped)
            .unwrap();
        store
            .set_key_status(SERIAL, KeyStatus::Distributed)
            .unwrap();
        store.set_key_status(SERIAL, KeyStatus::Returned).unwrap();
        let _ = store.close();
    }
    let mut app = YkDistApp::new(Some(database));
    assert!(app.store.is_some(), "{:?}", app.db_form.error);

    // When the operator returns it to stock
    app.change_key_status(SERIAL, KeyStatus::InStock);

    // Then the status moved and the trail says so
    assert_eq!(status_of(&app), KeyStatus::InStock);
    let moved = app
        .store
        .as_ref()
        .unwrap()
        .audit_entries(200)
        .unwrap()
        .into_iter()
        .filter(|e| e.event == "key.status_changed" && e.details.contains("in_stock"))
        .count();
    assert_eq!(moved, 1);

    // And a refused move is said, not applied
    app.change_key_status(SERIAL, KeyStatus::Returned);
    assert_eq!(status_of(&app), KeyStatus::InStock);
    assert!(app.status.starts_with("refused"), "{}", app.status);
}
