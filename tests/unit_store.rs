//! Unit tests for the store's edges: configuration, error paths, and the
//! refusals that protect a shared file.

use yk_dist_manager::device::DeviceInfo;
use yk_dist_manager::domain::{
    DeliveryMethod, DistributionRecord, Holder, KeyStatus, YubiKeyRecord,
};
use yk_dist_manager::store::{
    Location, SCHEMA_VERSION, Store, StoreConfig, StoreError, delivery_from, delivery_str,
    key_status_from, key_status_str,
};

fn key(serial: u32) -> YubiKeyRecord {
    YubiKeyRecord::from_device(&DeviceInfo {
        serial,
        model: "YubiKey 5 NFC".into(),
        firmware: "5.4.3".into(),
        form_factor: "Keychain (USB-A)".into(),
        nfc: true,
        usb_applications: vec!["FIDO2".into()],
    })
}

#[test]
fn default_path_honours_the_environment_override() {
    // Serialised implicitly: this is the only test touching YKDM_DB.
    let previous = std::env::var("YKDM_DB").ok();
    unsafe { std::env::set_var("YKDM_DB", "/tmp/ykdm-test.sqlite3") };
    assert_eq!(
        Store::default_path(),
        std::path::PathBuf::from("/tmp/ykdm-test.sqlite3")
    );

    // A blank value must fall through to the per-user location, not produce "".
    unsafe { std::env::set_var("YKDM_DB", "   ") };
    let fallback = Store::default_path();
    assert!(
        fallback
            .to_string_lossy()
            .ends_with("yk-dist-manager.sqlite3")
    );
    assert!(fallback.parent().is_some());

    match previous {
        Some(value) => unsafe { std::env::set_var("YKDM_DB", value) },
        None => unsafe { std::env::remove_var("YKDM_DB") },
    }
}

#[test]
fn config_treats_an_empty_password_as_no_password() {
    let config = StoreConfig::new("/tmp/x.sqlite3").with_password(Some(String::new()));
    assert!(
        config.password.is_none(),
        "a blank prompt must not create an unopenable file"
    );
}

#[test]
fn location_labels_describe_the_locking_strategy() {
    assert!(Location::LocalDisk.label().contains("WAL"));
    assert!(Location::NetworkShare.label().contains("rollback"));
}

#[test]
fn describe_reports_path_locking_and_password_state() {
    let store = Store::open_in_memory().unwrap();
    let described = store.describe();
    assert!(described.contains(":memory:"));
    assert!(described.contains("local disk"));
    assert!(described.contains("no password"));
    assert!(!store.is_encrypted());
    assert_eq!(store.location(), Location::LocalDisk);
    assert_eq!(store.path().to_string_lossy(), ":memory:");
}

#[test]
fn a_fresh_database_is_at_the_current_schema_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v.sqlite3");
    let store = Store::open(&StoreConfig::new(&path)).unwrap();
    drop(store);

    let conn = rusqlite_open(&path);
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
fn a_database_from_a_newer_build_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("future.sqlite3");
    {
        let store = Store::open(&StoreConfig::new(&path)).unwrap();
        drop(store);
    }
    // Simulate a colleague's newer build having migrated the shared file.
    let conn = rusqlite_open(&path);
    conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    drop(conn);

    match Store::open(&StoreConfig::new(&path)) {
        Err(StoreError::SchemaTooNew { found, supported }) => {
            assert_eq!(found, SCHEMA_VERSION + 1);
            assert_eq!(supported, SCHEMA_VERSION);
        }
        Err(other) => panic!("wrong error: {other}"),
        Ok(_) => panic!("a newer schema must not be opened"),
    }
}

#[test]
fn reopening_does_not_re_run_the_migration() {
    let dir = tempfile::tempdir().unwrap();
    let config = StoreConfig::new(dir.path().join("m.sqlite3"));
    let first = Store::open(&config).unwrap();
    first.upsert_key(&key(1)).unwrap();
    drop(first);

    let second = Store::open(&config).unwrap();
    assert_eq!(second.keys().unwrap().len(), 1, "data survived");
}

#[test]
fn a_status_change_on_an_unknown_serial_is_not_found() {
    let store = Store::open_in_memory().unwrap();
    match store.set_key_status(999, KeyStatus::Bootstrapped) {
        Err(StoreError::NotFound(what)) => assert!(what.contains("999")),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn setting_the_same_status_twice_is_allowed() {
    let store = Store::open_in_memory().unwrap();
    store.upsert_key(&key(7)).unwrap();
    // Idempotence matters: two operators may click the same button.
    store.set_key_status(7, KeyStatus::InStock).unwrap();
    assert_eq!(
        store.key_by_serial(7).unwrap().unwrap().status,
        KeyStatus::InStock
    );
}

#[test]
fn an_unknown_serial_reads_back_as_none() {
    let store = Store::open_in_memory().unwrap();
    assert!(store.key_by_serial(12_345).unwrap().is_none());
}

#[test]
fn returning_an_unknown_distribution_is_not_found() {
    let store = Store::open_in_memory().unwrap();
    let outcome = store.mark_returned(uuid::Uuid::new_v4(), "felipe");
    assert!(matches!(outcome, Err(StoreError::NotFound(_))));
}

#[test]
fn a_distribution_for_an_unknown_key_violates_the_foreign_key() {
    let store = Store::open_in_memory().unwrap();
    let holder = Holder::new("Ana", "ana@fgv.br", "ESI", "").unwrap();
    store.insert_holder(&holder).unwrap();

    let orphan = DistributionRecord {
        id: uuid::Uuid::new_v4(),
        key_id: uuid::Uuid::new_v4(), // never inserted
        key_serial: 1,
        holder_id: holder.id,
        holder_display: holder.display(),
        distributed_at: chrono::Utc::now(),
        distributed_by: "felipe".into(),
        method: DeliveryMethod::InPerson,
        receipt_ref: String::new(),
        bootstrap_run_id: None,
        returned_at: None,
        returned_to: None,
        notes: String::new(),
    };
    assert!(
        store.insert_distribution(&orphan).is_err(),
        "foreign keys must be enforced (PRAGMA foreign_keys = ON)"
    );
}

#[test]
fn enum_columns_round_trip_through_readable_strings() {
    for status in [
        KeyStatus::InStock,
        KeyStatus::Bootstrapped,
        KeyStatus::Distributed,
        KeyStatus::Returned,
        KeyStatus::Lost,
        KeyStatus::Retired,
    ] {
        let raw = key_status_str(status);
        assert!(
            raw.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "stored value should be readable snake_case, got `{raw}`"
        );
        assert_eq!(key_status_from(raw).unwrap(), status);
    }

    for method in DeliveryMethod::ALL {
        assert_eq!(delivery_from(delivery_str(method)).unwrap(), method);
    }
}

#[test]
fn an_unknown_stored_enum_value_is_a_decode_error() {
    assert!(matches!(
        key_status_from("teleported"),
        Err(StoreError::Decode { .. })
    ));
    assert!(matches!(
        delivery_from("carrier-pigeon"),
        Err(StoreError::Decode { .. })
    ));
}

#[test]
fn a_backup_of_an_empty_database_is_still_usable() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&StoreConfig::new(dir.path().join("empty.sqlite3"))).unwrap();
    let target = dir.path().join("empty-backup.sqlite3");
    store.backup_to(&target).unwrap();

    let restored = Store::open(&StoreConfig::new(&target)).unwrap();
    assert!(restored.keys().unwrap().is_empty());
    assert_eq!(restored.verify_audit().unwrap(), 0);
}

#[test]
fn audit_entries_are_capped_by_the_requested_limit() {
    let store = Store::open_in_memory().unwrap();
    for n in 0..5 {
        store
            .append_audit("felipe", "event", &format!("t{n}"), "")
            .unwrap();
    }
    assert_eq!(store.audit_entries(2).unwrap().len(), 2);
    assert_eq!(store.audit_entries(50).unwrap().len(), 5);
    // Newest first.
    assert_eq!(store.audit_entries(1).unwrap()[0].seq, 5);
}

/// Open the file directly, to inspect or tamper with what the store wrote.
fn rusqlite_open(path: &std::path::Path) -> rusqlite::Connection {
    rusqlite::Connection::open(path).expect("open for inspection")
}
