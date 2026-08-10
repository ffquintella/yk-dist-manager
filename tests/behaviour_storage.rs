//! Behaviour tests for the single-file database: it is one file, it survives a
//! reopen, it can be backed up while in use, and its audit table cannot be
//! rewritten.

use yk_dist_manager::device::DeviceInfo;
use yk_dist_manager::domain::{Holder, YubiKeyRecord};
use yk_dist_manager::store::{Location, Store, StoreConfig};

fn sample_key(serial: u32) -> YubiKeyRecord {
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
fn scenario_everything_lives_in_one_file() {
    // Given a fresh database on disk
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.sqlite3");
    let store = Store::open(&StoreConfig::new(&path)).unwrap();

    // When records of every kind are written
    store.upsert_key(&sample_key(20_423_633)).unwrap();
    store
        .insert_holder(&Holder::new("Ana", "ana@fgv.br", "ESI", "").unwrap())
        .unwrap();
    store.seed_builtin_templates().unwrap();
    store
        .append_audit("felipe", "key.added", "serial:1", "")
        .unwrap();
    drop(store);

    // Then the data file is the only thing that must be copied
    assert!(path.exists());
    let leftovers: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "keys.sqlite3")
        .collect();
    assert!(
        leftovers.is_empty(),
        "expected a single file, also found: {leftovers:?}"
    );
}

#[test]
fn scenario_records_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.sqlite3");
    let config = StoreConfig::new(&path);

    // Given records written by one session
    {
        let store = Store::open(&config).unwrap();
        store.upsert_key(&sample_key(20_423_633)).unwrap();
        store
            .append_audit("felipe", "key.added", "serial:1", "")
            .unwrap();
    }

    // When the app is opened again
    let store = Store::open(&config).unwrap();

    // Then everything is still there, and the chain still verifies
    assert_eq!(store.keys().unwrap().len(), 1);
    assert_eq!(store.verify_audit().unwrap(), 1);
}

#[test]
fn scenario_a_share_hosted_database_avoids_wal() {
    // Given a path that looks like a mounted share
    let location = Location::detect(std::path::Path::new("/Volumes/ti-share/yubikeys.sqlite3"));
    assert_eq!(location, Location::NetworkShare);

    // And a local path
    assert_eq!(
        Location::detect(std::path::Path::new("/Users/felipe/keys.sqlite3")),
        Location::LocalDisk
    );

    // When a store is opened in share mode
    let dir = tempfile::tempdir().unwrap();
    let config = StoreConfig {
        path: dir.path().join("share.sqlite3"),
        password: None,
        location: Location::NetworkShare,
    };
    let store = Store::open(&config).unwrap();

    // Then it works, and no WAL sidecar files are created
    store.upsert_key(&sample_key(20_423_633)).unwrap();
    assert_eq!(store.keys().unwrap().len(), 1);
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !names
            .iter()
            .any(|n| n.ends_with("-wal") || n.ends_with("-shm")),
        "share mode must not use WAL, found: {names:?}"
    );
}

#[test]
fn scenario_windows_unc_paths_are_treated_as_shares() {
    let location = Location::detect(std::path::Path::new(r"\\fileserver\ti\yubikeys.sqlite3"));
    assert_eq!(location, Location::NetworkShare);
}

#[test]
fn scenario_the_audit_trail_cannot_be_rewritten() {
    // Given a database with audit entries
    let store = Store::open_in_memory().unwrap();
    store
        .append_audit("felipe", "key.added", "serial:1", "")
        .unwrap();
    store
        .append_audit("felipe", "key.distributed", "serial:1", "to=ana")
        .unwrap();

    // Then the chain verifies
    assert_eq!(store.verify_audit().unwrap(), 2);

    // And the entries are ordered newest first for display
    let entries = store.audit_entries(10).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].seq, 2);
    assert_eq!(entries[1].prev_hash, yk_dist_manager::audit::GENESIS);
}

#[test]
fn scenario_a_backup_is_a_usable_copy() {
    // Given a database with data
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&StoreConfig::new(dir.path().join("keys.sqlite3"))).unwrap();
    store.upsert_key(&sample_key(20_423_633)).unwrap();
    store
        .append_audit("felipe", "key.added", "serial:1", "")
        .unwrap();

    // When a backup is taken while the store is open
    let backup = dir.path().join("backup.sqlite3");
    store.backup_to(&backup).unwrap();

    // Then the copy opens on its own and carries the same records
    let restored = Store::open(&StoreConfig::new(&backup)).unwrap();
    assert_eq!(restored.keys().unwrap().len(), 1);
    assert_eq!(restored.verify_audit().unwrap(), 1);
}

#[test]
fn scenario_integrity_check_reports_ok_on_a_healthy_file() {
    let store = Store::open_in_memory().unwrap();
    assert_eq!(store.integrity_check().unwrap(), "ok");
}

#[test]
fn scenario_builtin_templates_are_seeded_once() {
    let store = Store::open_in_memory().unwrap();
    let first = store.seed_builtin_templates().unwrap();
    assert!(first >= 2, "at least the two built-in templates");

    // Seeding again must not duplicate or overwrite
    assert_eq!(store.seed_builtin_templates().unwrap(), 0);
    assert_eq!(store.templates().unwrap().len(), first);
}

#[test]
fn scenario_a_stored_template_round_trips() {
    let store = Store::open_in_memory().unwrap();
    store.seed_builtin_templates().unwrap();
    let templates = store.templates().unwrap();
    let standard = templates
        .iter()
        .find(|t| t.id == "fgv-standard")
        .expect("standard template stored");
    assert!(standard.validate().is_ok());
    assert!(standard.steps.iter().any(|s| s.id == "piv-csr"));
}

#[test]
fn scenario_opening_a_plain_file_without_encryption_support_needs_no_password() {
    // The default build has no SQLCipher; an unprotected file must still open.
    let dir = tempfile::tempdir().unwrap();
    let config = StoreConfig::new(dir.path().join("keys.sqlite3")).with_password(None);
    assert!(config.password.is_none());
    assert!(Store::open(&config).is_ok());
}

#[cfg(not(feature = "encrypted-db"))]
#[test]
fn scenario_asking_for_a_password_without_the_feature_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let config =
        StoreConfig::new(dir.path().join("keys.sqlite3")).with_password(Some("hunter2".into()));
    let err = match Store::open(&config) {
        Ok(_) => panic!("must refuse a password without SQLCipher"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("encrypted-db"),
        "the error must name the feature to enable, got: {err}"
    );
}
