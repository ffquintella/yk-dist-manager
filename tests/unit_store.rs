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
    // A sync folder needs the share's journal mode *and* the lock file, and the
    // label is what tells the operator which one they are getting.
    assert!(Location::CloudSync.label().contains("rollback"));
    assert!(Location::CloudSync.label().contains("single-writer lock"));

    assert!(Location::CloudSync.requires_lease());
    assert!(!Location::NetworkShare.requires_lease());
    assert!(!Location::LocalDisk.requires_lease());
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
    let holder = Holder::new("Ana", "ana@example.org", "ESI", "").unwrap();
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

#[test]
fn opening_a_path_that_does_not_exist_is_refused_rather_than_created() {
    // The failure that matters: a typo'd share path must not silently produce a
    // second, empty database that looks like data loss.
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("not-there.sqlite3");
    let config = StoreConfig::new(&missing);

    match Store::open_existing(&config) {
        Err(StoreError::Missing(path)) => assert_eq!(path, missing),
        Err(other) => panic!("wrong error: {other}"),
        Ok(_) => panic!("must not open a file that is not there"),
    }
    assert!(!missing.exists(), "and nothing was created");
}

#[test]
fn creating_over_an_existing_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.sqlite3");
    let config = StoreConfig::new(&path);

    let store = Store::create_new(&config).unwrap();
    store.upsert_key(&key(20_423_633)).unwrap();
    drop(store);

    match Store::create_new(&config) {
        Err(StoreError::AlreadyExists(at)) => assert_eq!(at, path),
        Err(other) => panic!("wrong error: {other}"),
        Ok(_) => panic!("create must never open somebody else's dataset"),
    }
    // The existing data is untouched.
    assert_eq!(
        Store::open_existing(&config).unwrap().keys().unwrap().len(),
        1
    );
}

#[test]
fn a_new_database_arrives_with_its_templates() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::create_new(&StoreConfig::new(dir.path().join("fresh.sqlite3"))).unwrap();
    assert!(
        !store.templates().unwrap().is_empty(),
        "a fresh database must be usable immediately"
    );
}

#[test]
fn provenance_is_upgraded_by_a_device_read_but_never_downgraded() {
    use yk_dist_manager::domain::SerialSource;

    let store = Store::open_in_memory().unwrap();

    // A scanned label first.
    let scanned = YubiKeyRecord::from_serial(20_423_633, SerialSource::ScannedLabel);
    store.upsert_key(&scanned).unwrap();
    assert_eq!(
        store
            .key_by_serial(20_423_633)
            .unwrap()
            .unwrap()
            .serial_source,
        SerialSource::ScannedLabel
    );

    // Then the key is actually read: provenance improves.
    let verified = key(20_423_633);
    store.upsert_key(&verified).unwrap();
    let stored = store.key_by_serial(20_423_633).unwrap().unwrap();
    assert_eq!(stored.serial_source, SerialSource::Device);
    assert!(stored.is_verified());

    // A later scan of the same label must not undo that.
    store
        .upsert_key(&YubiKeyRecord::from_serial(
            20_423_633,
            SerialSource::ScannedLabel,
        ))
        .unwrap();
    assert_eq!(
        store
            .key_by_serial(20_423_633)
            .unwrap()
            .unwrap()
            .serial_source,
        SerialSource::Device
    );
}

#[test]
fn a_v1_database_migrates_forward_keeping_its_rows() {
    // Simulate a database created by the first released schema, then open it with
    // this build: the migration chain must carry it to the current version
    // without touching the data.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.sqlite3");

    {
        let conn = rusqlite_open(&path);
        conn.execute_batch(
            "CREATE TABLE keys (
                 id TEXT PRIMARY KEY, serial INTEGER NOT NULL UNIQUE, model TEXT NOT NULL,
                 firmware TEXT NOT NULL, form_factor TEXT NOT NULL DEFAULT '',
                 fips INTEGER NOT NULL DEFAULT 0, applications TEXT NOT NULL DEFAULT '[]',
                 status TEXT NOT NULL, batch TEXT NOT NULL DEFAULT '',
                 notes TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL);
             CREATE TABLE holders (
                 id TEXT PRIMARY KEY, full_name TEXT NOT NULL, email TEXT NOT NULL UNIQUE,
                 unit TEXT NOT NULL DEFAULT '', registration TEXT NOT NULL DEFAULT '',
                 active INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL);
             -- v1 shipped the templates table without `retired_at`; v4 adds it,
             -- so the fixture has to carry the old shape for the ALTER to be
             -- what it will be in the field.
             CREATE TABLE templates (
                 id TEXT NOT NULL, version TEXT NOT NULL, name TEXT NOT NULL,
                 body TEXT NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY (id, version));
             CREATE TABLE bootstrap_runs (
                 id TEXT PRIMARY KEY, key_serial INTEGER NOT NULL, holder_id TEXT,
                 template_id TEXT NOT NULL, template_version TEXT NOT NULL,
                 operator TEXT NOT NULL, started_at TEXT NOT NULL, finished_at TEXT,
                 status TEXT NOT NULL, steps TEXT NOT NULL DEFAULT '[]',
                 custody TEXT NOT NULL DEFAULT '');
             INSERT INTO keys VALUES ('11111111-1111-1111-1111-111111111111', 20423633,
                 'YubiKey 5 NFC', '5.4.3', 'Keychain (USB-A)', 0, '[\"FIDO2\"]', 'in_stock',
                 '', '', '2026-08-01T10:00:00+00:00', '2026-08-01T10:00:00+00:00');
             INSERT INTO holders VALUES ('22222222-2222-2222-2222-222222222222', 'Ana Silva',
                 'ana@example.org', 'ESI', '', 1, '2026-08-01T10:00:00+00:00');
             -- A run whose steps are a JSON blob, which is what v5 turns into
             -- rows. Written in serde's spelling (`Fido2Pin`, `Done`) because
             -- that is what the old build actually stored.
             INSERT INTO bootstrap_runs VALUES ('33333333-3333-3333-3333-333333333333',
                 20423633, '22222222-2222-2222-2222-222222222222', 'org-standard', '1',
                 'operator', '2026-08-01T10:00:00+00:00', '2026-08-01T10:05:00+00:00',
                 '\"Completed\"',
                 '[{\"step_id\":\"fido2-pin\",\"kind\":\"Fido2Pin\",\"status\":\"Done\",
                    \"started_at\":null,\"finished_at\":null,\"detail\":\"transport PIN set\"},
                   {\"step_id\":\"piv-keygen\",\"kind\":\"PivKeygen\",\"status\":\"Skipped\",
                    \"started_at\":null,\"finished_at\":null,\"detail\":\"deselected\"}]',
                 'sealed-envelope');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
    }

    let store = Store::open_existing(&StoreConfig::new(&path)).unwrap();

    let keys = store.keys().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(
        keys[0].serial_source,
        yk_dist_manager::domain::SerialSource::Device,
        "rows that predate the column came from a device read"
    );

    let holders = store.holders().unwrap();
    assert_eq!(holders.len(), 1);
    assert_eq!(holders[0].identification_number, "", "new optional field");

    // The new tables exist and are usable.
    assert_eq!(store.seed_builtin_terms().unwrap(), 2);

    // v4's column arrived: a template can be retired, which the old shape could
    // not express.
    store.seed_builtin_templates().unwrap();
    let catalogue = store.template_catalogue().unwrap();
    assert!(!catalogue.is_empty(), "templates carried through v4");
    assert!(catalogue.iter().all(|stored| !stored.is_retired()));

    // v5 moved the step blob into rows. The run must read back with exactly the
    // steps it recorded, in the order it recorded them — this is the evidence of
    // what was applied to a key, so a migration that reordered or lost a step
    // would be rewriting history.
    let runs = store.runs().unwrap();
    assert_eq!(runs.len(), 1);
    let steps = &runs[0].steps;
    assert_eq!(steps.len(), 2, "both steps survived the blob-to-rows move");
    assert_eq!(steps[0].step_id, "fido2-pin");
    assert_eq!(steps[0].kind, yk_dist_manager::domain::StepKind::Fido2Pin);
    assert_eq!(steps[0].status, yk_dist_manager::domain::StepStatus::Done);
    assert_eq!(steps[0].detail, "transport PIN set");
    assert_eq!(steps[1].step_id, "piv-keygen");
    assert_eq!(
        steps[1].status,
        yk_dist_manager::domain::StepStatus::Skipped,
        "a deselected step is still part of the record"
    );
    assert_eq!(runs[0].custody, "sealed-envelope");

    // And the step rows are now queryable as rows, which is the point of v5.
    let tally = store.step_outcome_tally().unwrap();
    assert_eq!(
        tally.get(&(
            yk_dist_manager::domain::StepKind::Fido2Pin,
            yk_dist_manager::domain::StepStatus::Done
        )),
        Some(&1)
    );
}

#[test]
fn a_run_with_an_unreadable_step_blob_keeps_its_record() {
    // Given a v4 database whose run carries a step list this build cannot parse
    // — a truncated write, or a blob from a build that spelled a kind
    // differently. The register must still open: losing the step detail of one
    // historical run is bad, and refusing to open the register of who holds
    // which security token is worse.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("damaged.sqlite3");

    {
        let conn = rusqlite_open(&path);
        conn.execute_batch(
            // The tables the v2..v4 migrations alter have to be present for the
            // chain to reach v5 at all; only `bootstrap_runs` carries the fixture.
            "CREATE TABLE keys (
                 id TEXT PRIMARY KEY, serial INTEGER NOT NULL UNIQUE, model TEXT NOT NULL,
                 firmware TEXT NOT NULL, form_factor TEXT NOT NULL DEFAULT '',
                 fips INTEGER NOT NULL DEFAULT 0, applications TEXT NOT NULL DEFAULT '[]',
                 status TEXT NOT NULL, batch TEXT NOT NULL DEFAULT '',
                 notes TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL);
             CREATE TABLE holders (
                 id TEXT PRIMARY KEY, full_name TEXT NOT NULL, email TEXT NOT NULL UNIQUE,
                 unit TEXT NOT NULL DEFAULT '', registration TEXT NOT NULL DEFAULT '',
                 active INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL);
             CREATE TABLE templates (
                 id TEXT NOT NULL, version TEXT NOT NULL, name TEXT NOT NULL,
                 body TEXT NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY (id, version));
             CREATE TABLE bootstrap_runs (
                 id TEXT PRIMARY KEY, key_serial INTEGER NOT NULL, holder_id TEXT,
                 template_id TEXT NOT NULL, template_version TEXT NOT NULL,
                 operator TEXT NOT NULL, started_at TEXT NOT NULL, finished_at TEXT,
                 status TEXT NOT NULL, steps TEXT NOT NULL DEFAULT '[]',
                 custody TEXT NOT NULL DEFAULT '');
             INSERT INTO bootstrap_runs VALUES ('44444444-4444-4444-4444-444444444444',
                 20423633, NULL, 'org-standard', '1', 'operator',
                 '2026-08-01T10:00:00+00:00', NULL, '\"Planned\"',
                 'this is not json', '');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
    }

    // When the register is opened by this build.
    let store = Store::open_existing(&StoreConfig::new(&path)).unwrap();

    // Then the run is still there, with the facts that did survive.
    let runs = store.runs().unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].template_id, "org-standard");
    assert_eq!(runs[0].operator, "operator");
    assert!(
        runs[0].steps.is_empty(),
        "the step list could not be recovered, and is empty rather than invented"
    );
}

#[test]
fn a_cloud_sync_folder_is_recognised_whatever_the_client() {
    use std::path::Path;
    use yk_dist_manager::store::looks_like_cloud_sync;

    // The real path that prompted this check, from a --diagnose report.
    assert!(looks_like_cloud_sync(Path::new(
        "/Users/felipe/Library/CloudStorage/OneDrive-Contoso/ykman/yk-dist-manager-v1.sqlite3"
    )));

    for path in [
        "/Users/x/OneDrive/keys.sqlite3",
        "/Users/x/Dropbox/ti/keys.sqlite3",
        "/Users/x/Google Drive/keys.sqlite3",
        "/Users/x/Library/Mobile Documents/com~apple~CloudDocs/keys.sqlite3",
        "C:\\Users\\x\\OneDrive - Contoso\\keys.sqlite3",
        "/home/x/pCloud Drive/keys.sqlite3",
    ] {
        assert!(looks_like_cloud_sync(Path::new(path)), "missed: {path}");
    }

    for path in [
        "/Users/x/Library/Application Support/yk-dist-manager/keys.sqlite3",
        "/Volumes/ti-share/keys.sqlite3",
        "/srv/ti/keys.sqlite3",
    ] {
        assert!(
            !looks_like_cloud_sync(Path::new(path)),
            "false positive: {path}"
        );
    }
}

#[test]
fn a_cloud_sync_database_avoids_wal_and_says_so() {
    use std::path::Path;

    // Its own classification, taking the shares' pragmas: WAL's shared-memory
    // sidecars cannot survive a sync client, so the journal mode must be the
    // conservative one — and the lock file is what makes two operators safe.
    assert_eq!(
        Location::detect(Path::new("/Users/x/OneDrive/keys.sqlite3")),
        Location::CloudSync
    );

    // And the warning reaches the operator through the status line.
    let dir = tempfile::tempdir().unwrap();
    let cloudish = dir.path().join("OneDrive-Contoso");
    std::fs::create_dir_all(&cloudish).unwrap();
    let store = Store::open(
        &StoreConfig::new(cloudish.join("keys.sqlite3"))
            .with_sync_policy(yk_dist_manager::store::SyncPolicy::immediate()),
    )
    .unwrap();

    assert!(store.on_cloud_sync());
    assert!(
        store.describe().contains("cloud-sync"),
        "the status line must warn: {}",
        store.describe()
    );

    // No WAL sidecars, as for a share.
    let names: Vec<String> = std::fs::read_dir(&cloudish)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !names
            .iter()
            .any(|n| n.ends_with("-wal") || n.ends_with("-shm")),
        "found: {names:?}"
    );
}

// ------------------------------------------- observations and removal

#[test]
fn an_observation_is_stored_against_the_serial() {
    let store = Store::open_in_memory().unwrap();
    store.upsert_key(&key(20_423_633)).unwrap();

    store
        .set_key_notes(20_423_633, "arrived in shipment NF-8891, box 2")
        .unwrap();

    let stored = store.key_by_serial(20_423_633).unwrap().unwrap();
    assert_eq!(stored.notes, "arrived in shipment NF-8891, box 2");
}

#[test]
fn an_observation_on_an_unknown_serial_is_not_found() {
    // Updating nothing must not report success: the operator would believe the
    // observation was filed.
    let store = Store::open_in_memory().unwrap();
    match store.set_key_notes(999, "note") {
        Err(StoreError::NotFound(what)) => assert!(what.contains("999")),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn removing_a_key_deletes_the_row_and_returns_what_was_removed() {
    let store = Store::open_in_memory().unwrap();
    store.upsert_key(&key(20_423_633)).unwrap();
    store
        .set_key_notes(20_423_633, "typed the wrong serial")
        .unwrap();

    let removed = store.delete_key(20_423_633).unwrap();

    assert_eq!(removed.serial, 20_423_633);
    assert_eq!(removed.notes, "typed the wrong serial");
    assert!(store.key_by_serial(20_423_633).unwrap().is_none());
    assert!(store.keys().unwrap().is_empty());
}

#[test]
fn removing_an_unknown_serial_is_not_found() {
    let store = Store::open_in_memory().unwrap();
    match store.delete_key(999) {
        Err(StoreError::NotFound(what)) => assert!(what.contains("999")),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn a_key_with_a_bootstrap_run_cannot_be_removed() {
    // A run is evidence about a physical key; deleting the key would leave it
    // pointing at a serial nobody can look up.
    let store = Store::open_in_memory().unwrap();
    store.upsert_key(&key(20_423_633)).unwrap();
    let run = yk_dist_manager::domain::BootstrapRun::new(
        20_423_633,
        None,
        "standard",
        "1",
        "felipe",
        Vec::new(),
    );
    store.insert_run(&run).unwrap();

    match store.delete_key(20_423_633) {
        Err(StoreError::HasHistory { serial, reason }) => {
            assert_eq!(serial, 20_423_633);
            assert!(reason.contains("bootstrap run"), "reason was: {reason}");
            assert!(reason.contains("retire"), "the alternative is named");
        }
        other => panic!("expected HasHistory, got {other:?}"),
    }
    assert!(
        store.key_by_serial(20_423_633).unwrap().is_some(),
        "the refusal left the row alone"
    );
}

#[test]
fn history_counts_report_what_refers_to_a_serial() {
    let store = Store::open_in_memory().unwrap();
    store.upsert_key(&key(20_423_633)).unwrap();
    assert_eq!(store.key_history_counts(20_423_633).unwrap(), (0, 0));

    let run = yk_dist_manager::domain::BootstrapRun::new(
        20_423_633,
        None,
        "standard",
        "1",
        "felipe",
        Vec::new(),
    );
    store.insert_run(&run).unwrap();
    assert_eq!(store.key_history_counts(20_423_633).unwrap(), (0, 1));
}
