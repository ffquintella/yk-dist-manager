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

#[test]
fn scenario_the_register_is_copied_on_a_schedule_and_old_copies_are_rotated_away() {
    use chrono::{Duration, Utc};
    use yk_dist_manager::store::BackupPolicy;

    // Given a register with a backup policy of "every day, keep three"
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.sqlite3");
    let policy = BackupPolicy {
        every: Duration::days(1),
        keep: 3,
        before_first_write: false,
    };
    let store = Store::create_new(&StoreConfig::new(&path).with_backup_policy(policy)).unwrap();
    store.upsert_key(&sample_key(20_423_633)).unwrap();

    // When five days pass, with the application opened once a day
    let day_one = Utc::now();
    for day in 0..5 {
        store.backup_if_due(day_one + Duration::days(day)).unwrap();
    }

    // Then three copies survive — the newest three, not the first three
    let mut backups: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".backup."))
        .collect();
    backups.sort();
    assert_eq!(
        backups.len(),
        3,
        "rotation kept exactly `keep` copies: {backups:?}"
    );

    // And every survivor is a usable register in its own right, which is the
    // only property that makes a backup worth having
    for name in &backups {
        let copy = Store::open_existing(&StoreConfig::new(dir.path().join(name))).unwrap();
        assert_eq!(copy.keys().unwrap().len(), 1);
        copy.verify_audit().unwrap();
    }

    // And a second check on the same day does not pile up another copy
    store.backup_if_due(day_one + Duration::days(4)).unwrap();
    let after = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".backup."))
        .count();
    assert_eq!(
        after, 3,
        "the schedule, not the number of launches, decides"
    );
}

#[test]
fn scenario_a_cloud_hosted_register_is_copied_before_the_session_can_write_to_it() {
    use yk_dist_manager::store::cloud::SyncPolicy;

    // Given a register in a sync folder, where a clash is resolved by keeping
    // both copies and the losing side has no other copy
    let dir = tempfile::tempdir().unwrap();
    let synced = dir.path().join("OneDrive - Contoso");
    std::fs::create_dir_all(&synced).unwrap();
    let path = synced.join("keys.sqlite3");

    {
        // The first session takes no snapshot of its own, so what the second
        // session finds is unambiguously the second session's doing. (Two opens
        // inside one second would otherwise share a filename, the stamp having
        // second resolution.)
        let store = Store::create_new(
            &StoreConfig::new(&path)
                .with_sync_policy(SyncPolicy::immediate())
                .with_backup_policy(yk_dist_manager::store::BackupPolicy {
                    before_first_write: false,
                    ..Default::default()
                })
                .with_operator("ana"),
        )
        .unwrap();
        store.upsert_key(&sample_key(20_423_633)).unwrap();
        store.close();
    }

    let copies = || -> Vec<std::path::PathBuf> {
        std::fs::read_dir(&synced)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().contains(".backup."))
            .collect()
    };

    // When a second session opens it — the moment before it could overwrite
    // anything the other workstation had done
    let before = copies();
    let store = Store::open_existing(
        &StoreConfig::new(&path)
            .with_sync_policy(SyncPolicy::immediate())
            .with_operator("bruno"),
    )
    .unwrap();

    // Then the state it found is already on disk as a copy
    let after = copies();
    assert!(
        after.len() > before.len(),
        "opening a cloud-hosted register must snapshot it before this session writes"
    );

    // And that copy is the register as it stood, openable on its own
    let snapshot = after
        .iter()
        .find(|p| !before.contains(p))
        .expect("the new copy");
    let copy = Store::open_existing(&StoreConfig::new(snapshot)).unwrap();
    assert_eq!(copy.keys().unwrap()[0].serial, 20_423_633);
    store.close();
}

#[test]
fn scenario_a_unit_imports_the_spreadsheet_this_tool_replaces() {
    use yk_dist_manager::store::import;

    // Given the register a unit actually keeps: a spreadsheet, edited by hand
    // for years, with a decorated serial and one row that is simply wrong
    let dir = tempfile::tempdir().unwrap();
    let csv = dir.path().join("registro.csv");
    std::fs::write(
        &csv,
        "Número de Série;Nome;E-mail;Unidade;Observações\n\
         20423633;Ana Silva;ana@example.org;ESI;chaveiro\n\
         '20423634;Bruno Costa;bruno@example.org;ESI;\n\
         SEM-NUMERO;Carla Dias;carla@example.org;ESI;perdida\n",
    )
    .unwrap();

    let store = Store::create_new(&StoreConfig::new(dir.path().join("keys.sqlite3"))).unwrap();

    // When the operator previews the import
    let plan = import::plan(&csv, &store.existing_for_import().unwrap()).unwrap();

    // Then they are told exactly what it would do, before anything is written
    assert_eq!(
        plan.summary(),
        "3 rows: 2 new keys, 2 new holders, 1 refused"
    );
    assert!(store.keys().unwrap().is_empty(), "a preview writes nothing");
    let refusal = plan.refusals().next().unwrap();
    assert_eq!(refusal.line, 4, "the line the spreadsheet shows");

    // And the column they use for their own notes is named as unrecognised
    // rather than silently dropped — `Observações` is recognised, so nothing
    // should be reported here
    assert!(
        plan.ignored_columns.is_empty(),
        "{:?}",
        plan.ignored_columns
    );

    // When they accept it
    let outcome = store.apply_import(&plan).unwrap();

    // Then the register holds the two good rows, and says where they came from
    assert_eq!(outcome.keys_added, 2);
    assert_eq!(outcome.holders_added, 2);
    assert_eq!(outcome.refused, 1);

    let keys = store.keys().unwrap();
    assert_eq!(keys.len(), 2);
    assert!(
        keys.iter()
            .all(|k| k.serial_source == yk_dist_manager::domain::SerialSource::ManualEntry),
        "a serial from a spreadsheet is a claim, not a device read"
    );
    assert!(
        keys.iter().any(|k| k.serial == 20_423_634),
        "a serial a spreadsheet decorated with a leading apostrophe still reads"
    );
    assert_eq!(store.holders().unwrap().len(), 2);

    // And importing the same file again changes nothing, because both natural
    // keys are unique — an operator who clicks twice does not double the register
    let plan = import::plan(&csv, &store.existing_for_import().unwrap()).unwrap();
    assert_eq!(plan.new_keys(), 0, "the second pass has nothing new");
    let outcome = store.apply_import(&plan).unwrap();
    assert_eq!(outcome.keys_refreshed, 2);
    assert_eq!(store.keys().unwrap().len(), 2);
}

#[test]
fn scenario_an_import_never_downgrades_a_serial_read_from_the_hardware() {
    use yk_dist_manager::store::import;

    // Given a key whose serial was read from the device
    let dir = tempfile::tempdir().unwrap();
    let store = Store::create_new(&StoreConfig::new(dir.path().join("keys.sqlite3"))).unwrap();
    store.upsert_key(&sample_key(20_423_633)).unwrap();

    // When a spreadsheet claiming the same key is imported
    let csv = dir.path().join("register.csv");
    std::fs::write(&csv, "Serial,Model\n20423633,Something Else\n").unwrap();
    let plan = import::plan(&csv, &store.existing_for_import().unwrap()).unwrap();
    store.apply_import(&plan).unwrap();

    // Then the provenance stays `device`: a typed claim must never outrank a key
    // somebody actually held, or a mis-typed serial could bind a certificate to
    // the wrong hardware
    let key = store.key_by_serial(20_423_633).unwrap().unwrap();
    assert_eq!(
        key.serial_source,
        yk_dist_manager::domain::SerialSource::Device
    );
}

#[test]
fn scenario_a_second_operator_can_look_at_a_locked_register_without_taking_it() {
    use yk_dist_manager::store::StoreError;
    use yk_dist_manager::store::cloud::SyncPolicy;

    // Given a register in a sync folder that Ana has open, and therefore locked
    let dir = tempfile::tempdir().unwrap();
    let synced = dir.path().join("OneDrive - Contoso");
    std::fs::create_dir_all(&synced).unwrap();
    let path = synced.join("keys.sqlite3");
    let config = || StoreConfig::new(&path).with_sync_policy(SyncPolicy::immediate());

    let ana = Store::create_new(&config().with_operator("ana")).unwrap();
    ana.upsert_key(&sample_key(20_423_633)).unwrap();
    ana.insert_holder(&Holder::new("Ana", "ana@example.org", "ESI", "").unwrap())
        .unwrap();

    // When Bruno, who only wants to know who holds serial 20423633, opens it for
    // reading
    let bruno = Store::open_read_only(&config().with_operator("bruno")).unwrap();

    // Then he can read the register
    assert!(bruno.is_read_only());
    assert_eq!(bruno.keys().unwrap().len(), 1);
    assert_eq!(bruno.holders().unwrap().len(), 1);
    assert!(
        bruno.describe().contains("READ ONLY"),
        "the status line has to say so: {}",
        bruno.describe()
    );

    // And Ana's lock is untouched — a reader sequences nothing, so it takes
    // nothing
    assert!(bruno.lease().is_none());
    assert_eq!(
        yk_dist_manager::store::cloud::read_holder(&yk_dist_manager::store::cloud::lock_path(
            &path
        ))
        .unwrap()
        .operator,
        "ana",
        "a read-only open must not disturb the workstation that has it"
    );

    // And nothing he does can change the register, because the refusal comes
    // from the database and not from a check this code might forget
    let refused = bruno.upsert_key(&sample_key(20_423_634));
    assert!(
        matches!(refused, Err(StoreError::ReadOnly)),
        "a write must be refused with the reason, got: {refused:?}"
    );
    assert!(matches!(
        bruno.append_audit("bruno", "key.added", "serial:2", ""),
        Err(StoreError::ReadOnly)
    ));
    assert!(matches!(
        bruno.set_key_notes(20_423_633, "spare"),
        Err(StoreError::ReadOnly)
    ));

    // And Ana's register is exactly as she left it
    assert_eq!(ana.keys().unwrap().len(), 1);
    ana.close();
}

#[test]
fn scenario_a_register_needing_a_migration_will_not_open_read_only() {
    // Migrating is a write. Opening an old file read-only would mean reading
    // rows with a schema they do not have — so it is refused, naming both
    // versions, instead of quietly misreading the register.
    use yk_dist_manager::store::StoreError;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.sqlite3");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE keys (id TEXT PRIMARY KEY);")
            .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
    }

    match Store::open_read_only(&StoreConfig::new(&path)) {
        Err(StoreError::MigrationNeedsWriteAccess { found, supported }) => {
            assert_eq!(found, 1);
            assert!(supported >= 5);
        }
        Err(other) => panic!("expected a refusal naming both versions, got: {other}"),
        Ok(_) => panic!("a register needing a migration must not open read-only"),
    }
}

#[test]
fn scenario_the_audit_trail_is_mirrored_to_segregated_storage_and_divergence_is_reported() {
    use yk_dist_manager::audit::MirrorStatus;

    // Given a register whose audit trail is mirrored to a second location — the
    // norm wants audit data apart from operational data, and the deployment
    // wants one file, so the mirror is how both are answered
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.sqlite3");
    let mirror_path = dir.path().join("segregated").join("audit.jsonl");
    let config = || StoreConfig::new(&path).with_audit_mirror(Some(mirror_path.clone()));

    {
        let store = Store::create_new(&config()).unwrap();

        // When work is recorded
        store.upsert_key(&sample_key(20_423_633)).unwrap();
        store
            .append_audit("ana", "key.added", "serial:20423633", "")
            .unwrap();
        store
            .append_audit("ana", "key.distributed", "serial:20423633", "holder=ana")
            .unwrap();

        // Then both copies agree
        assert_eq!(
            store.mirror_status(),
            MirrorStatus::InSync { entries: 2 },
            "every entry reaches the segregated copy"
        );
        assert!(mirror_path.is_file(), "the mirror is a file of its own");
    }

    // When somebody rebuilds the chain in the database — dropping the trigger,
    // rewriting the entry *and recomputing its hash*, so the trail still
    // verifies against itself. This is the tamper worth defending against: a
    // sloppy edit is caught by `verify_audit` alone, and needs no mirror.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        let (at, actor, event, target, prev_hash): (String, String, String, String, String) = conn
            .query_row(
                "SELECT at, actor, event, target, prev_hash FROM audit WHERE seq = 2",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();

        let mut rebuilt = yk_dist_manager::audit::AuditEntry {
            seq: 2,
            at: chrono::DateTime::parse_from_rfc3339(&at)
                .unwrap()
                .with_timezone(&chrono::Utc),
            actor,
            event,
            target,
            details: "holder=bruno".into(),
            prev_hash,
            hash: String::new(),
        };
        rebuilt.hash = rebuilt.compute_hash();

        conn.execute_batch("DROP TRIGGER audit_no_update;").unwrap();
        conn.execute(
            "UPDATE audit SET details = ?1, hash = ?2 WHERE seq = 3 - 1",
            rusqlite::params![rebuilt.details, rebuilt.hash],
        )
        .unwrap();
    }

    // Then the database's own trail still verifies — the rebuild was consistent,
    // which is precisely why the trigger and the hash chain are not enough
    let store = Store::open_existing(&config()).unwrap();
    assert_eq!(
        *store.chain_status(),
        yk_dist_manager::store::ChainStatus::Verified { entries: 2 },
        "a rebuilt chain verifies against itself; that is the point"
    );

    // And the mirror catches it, naming the entry
    match store.mirror_status() {
        MirrorStatus::Diverged { seq } => assert_eq!(seq, 2),
        other => panic!("expected a divergence, got: {other:?}"),
    }
    assert!(store.mirror_status().is_alert());
    assert!(
        store.mirror_status().describe().contains("DIVERGED"),
        "{}",
        store.mirror_status().describe()
    );
}

#[test]
fn scenario_an_edit_that_forgets_the_hash_is_caught_without_any_mirror() {
    // The other half: most tampering is not careful. An entry edited in place
    // breaks its own hash, and the chain verification finds it at open with no
    // second copy involved.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.sqlite3");
    {
        let store = Store::create_new(&StoreConfig::new(&path)).unwrap();
        store
            .append_audit("ana", "key.distributed", "serial:20423633", "holder=ana")
            .unwrap();
    }

    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "DROP TRIGGER audit_no_update;
         UPDATE audit SET details = 'holder=bruno' WHERE seq = 1;",
    )
    .unwrap();
    drop(conn);

    let store = Store::open_existing(&StoreConfig::new(&path)).unwrap();
    assert!(
        store.chain_status().is_broken(),
        "a broken chain is found at open, not when somebody happens to press Verify"
    );
}

#[test]
fn scenario_a_healthy_register_reports_its_chain_verified_at_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.sqlite3");
    {
        let store = Store::create_new(&StoreConfig::new(&path)).unwrap();
        store
            .append_audit("ana", "key.added", "serial:20423633", "")
            .unwrap();
    }

    let store = Store::open_existing(&StoreConfig::new(&path)).unwrap();
    assert_eq!(
        *store.chain_status(),
        yk_dist_manager::store::ChainStatus::Verified { entries: 1 }
    );
    assert_eq!(
        store.mirror_status(),
        yk_dist_manager::audit::MirrorStatus::NotConfigured,
        "no mirror is normal until the ESI decides one is required"
    );
}

#[test]
fn scenario_the_audit_screen_can_answer_a_question_about_one_key() {
    use yk_dist_manager::audit::AuditFilter;

    // Given a trail with two keys and two operators in it
    let dir = tempfile::tempdir().unwrap();
    let store = Store::create_new(&StoreConfig::new(dir.path().join("keys.sqlite3"))).unwrap();
    for (actor, event, target) in [
        ("ana", "key.added", "serial:20423633"),
        ("ana", "key.distributed", "serial:20423633"),
        ("bruno", "key.added", "serial:20423634"),
        ("bruno", "template.changed", "org-standard v2"),
    ] {
        store.append_audit(actor, event, target, "").unwrap();
    }

    // When an auditor asks what happened to one key
    let filter = AuditFilter {
        target: "20423633".into(),
        ..Default::default()
    };
    let matched = store.audit_entries_matching(&filter, 500).unwrap();

    // Then they get that key's history and nothing else
    assert_eq!(matched.len(), 2);
    assert!(matched.iter().all(|e| e.target.contains("20423633")));

    // And the limit applies after the filter, so a key whose events are old is
    // still found rather than falling off the end of the newest N
    let narrow = store.audit_entries_matching(&filter, 1).unwrap();
    assert_eq!(narrow.len(), 1);
}

#[test]
fn scenario_a_local_register_is_not_copied_just_for_being_opened() {
    // The pre-write snapshot answers a failure that only happens behind a sync
    // client. A local file with SQLite's own locking does not need it, and a
    // copy per launch would be a surprise on a workstation's disk.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.sqlite3");
    let store = Store::create_new(&StoreConfig::new(&path)).unwrap();
    store.close();

    let copies = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".backup."))
        .count();
    assert_eq!(copies, 0);
}

/// A passphrase that meets the policy. Not a credential: it protects nothing,
/// exists only inside this test, and the databases it opens are in a temp
/// directory that is deleted when the test ends.
#[cfg(feature = "encrypted-db")]
const A_GOOD_PASSPHRASE: &str = "correct horse battery staple";
#[cfg(feature = "encrypted-db")]
const ANOTHER_GOOD_PASSPHRASE: &str = "seven green bicycles waiting";

#[cfg(feature = "encrypted-db")]
#[test]
fn scenario_a_plain_register_can_be_encrypted_without_losing_anything() {
    use yk_dist_manager::store::StoreError;

    // Given a plain register with real content in it
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.sqlite3");
    {
        let store = Store::create_new(&StoreConfig::new(&path)).unwrap();
        store.upsert_key(&sample_key(20_423_633)).unwrap();
        store
            .insert_holder(&Holder::new("Ana", "ana@example.org", "ESI", "").unwrap())
            .unwrap();
        store
            .append_audit("felipe", "key.added", "serial:20423633", "")
            .unwrap();

        // When it is encrypted
        let returned = store
            .change_password("felipe", Some(A_GOOD_PASSPHRASE))
            .unwrap();
        assert_eq!(returned, path, "the register keeps its path");
    }

    // Then it needs the password
    match Store::open_existing(&StoreConfig::new(&path)) {
        Err(StoreError::PasswordRequired) => {}
        Err(other) => panic!("expected PasswordRequired, got {other}"),
        Ok(_) => panic!("the file must no longer open without a password"),
    }

    // And with it, everything is still there and the chain still verifies
    let store = Store::open_existing(
        &StoreConfig::new(&path).with_password(Some(A_GOOD_PASSPHRASE.into())),
    )
    .unwrap();
    assert_eq!(store.keys().unwrap().len(), 1);
    assert_eq!(store.holders().unwrap().len(), 1);
    assert!(!store.chain_status().is_broken());

    // And the change is on the record, inside the encrypted file — written
    // before the export precisely so the export would carry it
    let events: Vec<String> = store
        .audit_entries(50)
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert!(events.contains(&"db.encrypted".to_string()), "{events:?}");
}

#[cfg(feature = "encrypted-db")]
#[test]
fn scenario_the_password_can_be_changed_and_the_old_one_stops_working() {
    use yk_dist_manager::store::StoreError;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.sqlite3");
    let with = |password: &str| StoreConfig::new(&path).with_password(Some(password.to_owned()));

    {
        let store = Store::create_new(&with(A_GOOD_PASSPHRASE)).unwrap();
        store.upsert_key(&sample_key(20_423_633)).unwrap();
        store
            .change_password("felipe", Some(ANOTHER_GOOD_PASSPHRASE))
            .unwrap();
    }

    // The old password no longer opens it…
    match Store::open_existing(&with(A_GOOD_PASSPHRASE)) {
        Err(StoreError::PasswordRequired) => {}
        Err(other) => panic!("expected PasswordRequired, got {other}"),
        Ok(_) => panic!("the old password must stop working"),
    }

    // …the new one does, with the data intact…
    let store = Store::open_existing(&with(ANOTHER_GOOD_PASSPHRASE)).unwrap();
    assert_eq!(store.keys().unwrap().len(), 1);
    // …the schema version came across, so the next open does not re-migrate…
    assert!(!store.chain_status().is_broken());
    // …and the change is audited.
    let events: Vec<String> = store
        .audit_entries(50)
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert!(
        events.contains(&"db.password.changed".to_string()),
        "{events:?}"
    );
}

#[cfg(feature = "encrypted-db")]
#[test]
fn scenario_a_weak_password_is_refused_before_anything_is_touched() {
    // The order matters: doing the backup and the whole export only to refuse
    // would be a lot of work to arrive at "no", and would leave a stray file.
    use yk_dist_manager::store::StoreError;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.sqlite3");
    let store = Store::create_new(&StoreConfig::new(&path)).unwrap();
    store.upsert_key(&sample_key(20_423_633)).unwrap();

    match store.change_password("felipe", Some("short")) {
        Err(StoreError::WeakPassword(_)) => {}
        Err(other) => panic!("expected WeakPassword, got {other}"),
        Ok(_) => panic!("a five-character password must be refused"),
    }

    // The register is untouched and still opens with no password.
    let store = Store::open_existing(&StoreConfig::new(&path)).unwrap();
    assert_eq!(store.keys().unwrap().len(), 1);

    let strays: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("rekey") || n.contains("replaced"))
        .collect();
    assert!(strays.is_empty(), "no half-finished files left: {strays:?}");
}

#[cfg(feature = "encrypted-db")]
#[test]
fn scenario_encrypting_takes_a_backup_first_because_it_rewrites_everything() {
    // The one operation that rewrites the whole register is the one that most
    // needs a copy of what it started from — and that copy is readable with the
    // *old* password, which for a plain file means none.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.sqlite3");
    {
        let store = Store::create_new(&StoreConfig::new(&path)).unwrap();
        store.upsert_key(&sample_key(20_423_633)).unwrap();
        store
            .change_password("felipe", Some(A_GOOD_PASSPHRASE))
            .unwrap();
    }

    let backups: Vec<std::path::PathBuf> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().contains(".backup."))
        .collect();
    assert_eq!(backups.len(), 1, "exactly one backup: {backups:?}");

    // It is the register as it stood before the change: still plain, still
    // complete.
    let before = Store::open_existing(&StoreConfig::new(&backups[0])).unwrap();
    assert_eq!(before.keys().unwrap().len(), 1);
}

#[cfg(not(feature = "encrypted-db"))]
#[test]
fn scenario_changing_the_password_without_encryption_support_says_which_build_is_needed() {
    use yk_dist_manager::store::StoreError;

    let dir = tempfile::tempdir().unwrap();
    let store = Store::create_new(&StoreConfig::new(dir.path().join("keys.sqlite3"))).unwrap();
    match store.change_password("felipe", Some("correct horse battery staple")) {
        Err(StoreError::EncryptionUnavailable) => {}
        Err(other) => panic!("expected EncryptionUnavailable, got {other}"),
        Ok(_) => panic!("a build without SQLCipher cannot encrypt anything"),
    }
}
