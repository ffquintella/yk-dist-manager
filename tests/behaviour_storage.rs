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
        .insert_holder(&Holder::new("Ana", "ana@example.org", "ESI", "").unwrap())
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
    let mut config = StoreConfig::new(dir.path().join("share.sqlite3"));
    config.location = Location::NetworkShare;
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
fn scenario_two_operators_share_a_onedrive_folder_one_at_a_time() {
    use yk_dist_manager::store::cloud::{self, SyncPolicy};

    // Given the register in a OneDrive folder — the deployment a real
    // installation chose, and the one a sync client can fork
    let dir = tempfile::tempdir().unwrap();
    let synced = dir.path().join("OneDrive - Contoso");
    std::fs::create_dir_all(&synced).unwrap();
    let path = synced.join("yk-dist-manager.sqlite3");
    let config = || {
        StoreConfig::new(&path)
            // The wait for the sync client is real in a session and pointless in
            // a test: there is no sync client here to wait for.
            .with_sync_policy(SyncPolicy::immediate())
    };

    // When the first operator opens it
    let ana = Store::create_new(&config().with_operator("ana")).unwrap();

    // Then the location is recognised, the lock is taken, and the journal mode is
    // the conservative one a sync client can carry
    assert_eq!(ana.location(), Location::CloudSync);
    assert!(cloud::lock_path(&path).is_file());
    assert_eq!(
        cloud::read_holder(&cloud::lock_path(&path))
            .unwrap()
            .operator,
        "ana"
    );

    ana.upsert_key(&sample_key(20_423_633)).unwrap();
    ana.append_audit("ana", "key.added", "serial:20423633", "")
        .unwrap();

    // When the second operator tries the same file from another workstation
    let refused = Store::open_existing(&config().with_operator("felipe"));

    // Then it is refused, by name, instead of writing to a copy that will be
    // resolved by keeping both
    let message = refused
        .err()
        .expect("a second open must be refused")
        .to_string();
    assert!(
        message.contains("ana"),
        "the refusal must name the holder: {message}"
    );
    assert!(message.contains("in use"), "{message}");

    // When the first operator closes the database
    let settled = ana.close().expect("closing waits for the upload");
    assert!(!settled.is_unsettled());

    // Then the lock is gone and the second operator gets the register, with the
    // first operator's work in it
    assert!(!cloud::lock_path(&path).exists());
    let felipe = Store::open_existing(&config().with_operator("felipe")).unwrap();
    assert_eq!(felipe.keys().unwrap().len(), 1);
    assert_eq!(felipe.verify_audit().unwrap(), 1);
    assert!(
        felipe.lease().is_some(),
        "the lock passes to whoever has it open"
    );
}

#[test]
fn scenario_a_lock_left_by_a_crashed_session_can_be_taken_over_and_is_recorded() {
    use yk_dist_manager::store::cloud::{self, LeaseHolder, SyncPolicy};

    // Given a register in a sync folder whose last session died without releasing
    // the lock — a closed laptop, a crash, a machine switched off
    let dir = tempfile::tempdir().unwrap();
    let synced = dir.path().join("OneDrive - Contoso");
    std::fs::create_dir_all(&synced).unwrap();
    let path = synced.join("yk-dist-manager.sqlite3");
    let config = || StoreConfig::new(&path).with_sync_policy(SyncPolicy::immediate());
    Store::create_new(&config().with_operator("ana"))
        .unwrap()
        .close();

    let hours_ago = chrono::Utc::now() - chrono::Duration::hours(4);
    std::fs::write(
        cloud::lock_path(&path),
        serde_json::to_string(&LeaseHolder {
            host: "MAC-RECEPCAO".into(),
            operator: "ana".into(),
            pid: 4242,
            session: uuid::Uuid::from_u128(0xBEEF),
            app_version: "0.5.0".into(),
            acquired_at: hours_ago,
            renewed_at: hours_ago,
        })
        .unwrap(),
    )
    .unwrap();

    // When another operator opens it normally
    // Then it is still refused: an abandoned lock is not an invitation
    assert!(Store::open_existing(&config().with_operator("felipe")).is_err());

    // When that operator deliberately takes the lock over
    let felipe =
        Store::open_existing(&config().with_operator("felipe").taking_over_stale_lease()).unwrap();

    // Then the register opens, and who was holding it survives in the audit
    // trail — the one record of a decision that could have cost a hand-over
    let broken = felipe
        .lease()
        .and_then(|lease| lease.report().took_over.as_ref())
        .expect("the previous holder must be carried out for the audit entry");
    assert_eq!(broken.operator, "ana");
    assert_eq!(broken.host, "MAC-RECEPCAO");

    felipe
        .append_audit(
            "felipe",
            "db.lock.taken_over",
            "database",
            &broken.to_string(),
        )
        .unwrap();
    let recorded = &felipe.audit_entries(1).unwrap()[0];
    assert_eq!(recorded.event, "db.lock.taken_over");
    assert!(recorded.details.contains("MAC-RECEPCAO"));
    assert_eq!(felipe.verify_audit().unwrap(), 1);
}

#[test]
fn scenario_a_sync_client_that_forked_the_register_is_reported_not_ignored() {
    use yk_dist_manager::store::cloud::SyncPolicy;

    // Given a register in a sync folder, and the copy a sync client leaves when
    // two workstations wrote to it and it could not merge them
    let dir = tempfile::tempdir().unwrap();
    let synced = dir.path().join("OneDrive - Contoso");
    std::fs::create_dir_all(&synced).unwrap();
    let path = synced.join("yk-dist-manager.sqlite3");
    Store::create_new(&StoreConfig::new(&path).with_sync_policy(SyncPolicy::immediate()))
        .unwrap()
        .close();
    std::fs::copy(&path, synced.join("yk-dist-manager (1).sqlite3")).unwrap();

    // When the register is opened
    let store =
        Store::open_existing(&StoreConfig::new(&path).with_sync_policy(SyncPolicy::immediate()))
            .unwrap();

    // Then the fork is surfaced: the register may already exist in two versions,
    // which is the failure this location is dangerous for
    let conflicts: Vec<String> = store
        .conflict_copies()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(conflicts, vec!["yk-dist-manager (1).sqlite3".to_owned()]);
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
        .find(|t| t.id == "org-standard")
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
