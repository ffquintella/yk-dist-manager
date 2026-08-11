//! Unit tests for the cloud-sync single-writer lock (`store::cloud`).
//!
//! One behaviour per test, no sync client involved: the protocol is a lock file
//! and a settle wait, both of which are ordinary file-system operations. A folder
//! whose name contains `OneDrive` is enough to make the store treat it as
//! synchronising, which is exactly the heuristic real installations hit.
//!
//! Every test uses [`SyncPolicy::immediate`] unless it is testing the waiting
//! itself: the default policy waits 1.5s per open, which belongs in a real
//! session and not in a test suite.

use std::path::{Path, PathBuf};
use std::time::Duration;

use yk_dist_manager::store::cloud::{
    self, LeaseError, LeaseHolder, Renewal, Settled, SyncLease, SyncPolicy,
};
use yk_dist_manager::store::{Location, Store, StoreConfig, StoreError};

/// A directory that the location heuristic reads as a sync folder.
fn onedrive_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("OneDrive - Contoso")).unwrap();
    dir
}

fn database_in(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("OneDrive - Contoso").join("keys.sqlite3")
}

fn quick(path: &Path) -> StoreConfig {
    StoreConfig::new(path).with_sync_policy(SyncPolicy::immediate())
}

// ------------------------------------------------------- the lock file itself

#[test]
fn a_lock_is_taken_next_to_the_database_and_names_the_operator() {
    let dir = onedrive_dir();
    let database = database_in(&dir);
    std::fs::write(&database, b"").unwrap();

    let lease = SyncLease::acquire(&database, "felipe", &SyncPolicy::immediate()).unwrap();

    assert_eq!(lease.lock_file(), cloud::lock_path(&database));
    assert!(lease.lock_file().is_file(), "the lock file must be there");
    assert_eq!(lease.holder().operator, "felipe");
    assert!(lease.holder().is_local());

    // Readable by another process — that is the whole point of it.
    let seen = cloud::read_holder(lease.lock_file()).unwrap();
    assert_eq!(seen.operator, "felipe");
    assert_eq!(seen.pid, std::process::id());
}

#[test]
fn a_second_workstation_is_refused_by_name() {
    let dir = onedrive_dir();
    let database = database_in(&dir);
    std::fs::write(&database, b"").unwrap();

    // What the other computer's lock looks like when it arrives here.
    let other = LeaseHolder {
        host: "MAC-RECEPCAO".into(),
        operator: "ana".into(),
        pid: 4242,
        session: uuid::Uuid::from_u128(0xA1A1),
        app_version: "0.5.0".into(),
        acquired_at: chrono::Utc::now(),
        renewed_at: chrono::Utc::now(),
    };
    std::fs::write(
        cloud::lock_path(&database),
        serde_json::to_string(&other).unwrap(),
    )
    .unwrap();

    match SyncLease::acquire(&database, "felipe", &SyncPolicy::immediate()) {
        Err(LeaseError::Held { holder, stale }) => {
            assert_eq!(holder.operator, "ana");
            assert_eq!(holder.host, "MAC-RECEPCAO");
            assert!(!stale, "a lock renewed just now is not abandoned");
            // The refusal has to be readable by the person who gets it.
            let message = LeaseError::Held { holder, stale }.to_string();
            assert!(message.contains("ana"), "{message}");
            assert!(message.contains("MAC-RECEPCAO"), "{message}");
        }
        Err(other) => panic!("expected a refusal naming the holder, got {other}"),
        Ok(_) => panic!("a database another workstation holds must not open"),
    }
}

#[test]
fn an_abandoned_lock_is_reported_as_stale_and_can_be_taken_over() {
    let dir = onedrive_dir();
    let database = database_in(&dir);
    std::fs::write(&database, b"").unwrap();

    let long_gone = chrono::Utc::now() - chrono::Duration::hours(3);
    let crashed = LeaseHolder {
        host: "MAC-RECEPCAO".into(),
        operator: "ana".into(),
        pid: 4242,
        session: uuid::Uuid::from_u128(0xA1A1),
        app_version: "0.5.0".into(),
        acquired_at: long_gone,
        renewed_at: long_gone,
    };
    std::fs::write(
        cloud::lock_path(&database),
        serde_json::to_string(&crashed).unwrap(),
    )
    .unwrap();

    // Still refused by default: only the operator can know the machine is off.
    match SyncLease::acquire(&database, "felipe", &SyncPolicy::immediate()) {
        Err(LeaseError::Held { stale, .. }) => assert!(stale, "3 hours of silence is abandoned"),
        Err(other) => panic!("expected a stale refusal, got {other}"),
        Ok(_) => panic!("an abandoned lock is still a lock until it is taken over"),
    }

    // Taken over deliberately, and the previous holder is carried out so it can
    // be audited.
    let lease = SyncLease::take_over(&database, "felipe", &SyncPolicy::immediate()).unwrap();
    let broken = lease
        .report()
        .took_over
        .as_ref()
        .expect("who was holding it");
    assert_eq!(broken.operator, "ana");
    assert!(lease.holder().is_local());
}

#[test]
fn a_lock_file_that_cannot_be_parsed_still_counts_as_held() {
    let dir = onedrive_dir();
    let database = database_in(&dir);
    std::fs::write(&database, b"").unwrap();
    // A sync client can show a partially written file. Treating that as free is
    // the one reading that can corrupt the register.
    std::fs::write(cloud::lock_path(&database), b"{ half a jso").unwrap();

    match SyncLease::acquire(&database, "felipe", &SyncPolicy::immediate()) {
        Err(LeaseError::Held { holder, .. }) => assert!(!holder.is_local()),
        Err(other) => panic!("expected a refusal, got {other}"),
        Ok(_) => panic!("an unreadable lock must not be treated as free"),
    }
}

#[test]
fn releasing_removes_the_lock_and_reports_the_wait() {
    let dir = onedrive_dir();
    let database = database_in(&dir);
    std::fs::write(&database, b"").unwrap();

    let lease = SyncLease::acquire(&database, "felipe", &SyncPolicy::immediate()).unwrap();
    let lock = lease.lock_file().to_path_buf();

    let settled = lease.release(&SyncPolicy::immediate());

    assert!(!lock.exists(), "the next workstation needs it gone");
    assert!(matches!(settled, Settled::Quiet { .. }), "{settled:?}");
}

#[test]
fn a_dropped_lease_still_frees_the_register() {
    let dir = onedrive_dir();
    let database = database_in(&dir);
    std::fs::write(&database, b"").unwrap();
    let lock = cloud::lock_path(&database);

    {
        let _lease = SyncLease::acquire(&database, "felipe", &SyncPolicy::immediate()).unwrap();
        assert!(lock.exists());
    }
    // A panic or a hard quit must not leave the shared register unopenable for
    // fifteen minutes.
    assert!(!lock.exists());
}

#[test]
fn a_lease_taken_over_by_another_workstation_is_reported_as_lost() {
    let dir = onedrive_dir();
    let database = database_in(&dir);
    std::fs::write(&database, b"").unwrap();

    let mut lease = SyncLease::acquire(&database, "felipe", &SyncPolicy::immediate()).unwrap();

    // The other computer broke it and wrote its own.
    let thief = LeaseHolder {
        host: "MAC-RECEPCAO".into(),
        operator: "ana".into(),
        pid: 4242,
        session: uuid::Uuid::from_u128(0xA1A1),
        app_version: "0.5.0".into(),
        acquired_at: chrono::Utc::now(),
        renewed_at: chrono::Utc::now(),
    };
    std::fs::write(lease.lock_file(), serde_json::to_string(&thief).unwrap()).unwrap();

    match lease.renew().unwrap() {
        Renewal::Lost(holder) => assert_eq!(holder.operator, "ana"),
        other => panic!("renewing over somebody else's lock must not succeed: {other:?}"),
    }
}

#[test]
fn a_renewal_that_is_not_due_does_not_rewrite_the_lock() {
    let dir = onedrive_dir();
    let database = database_in(&dir);
    std::fs::write(&database, b"").unwrap();

    let mut lease = SyncLease::acquire(&database, "felipe", &SyncPolicy::immediate()).unwrap();
    let first = cloud::read_holder(lease.lock_file()).unwrap();

    assert_eq!(lease.renew_if_due().unwrap(), Renewal::NotDue);
    assert_eq!(cloud::read_holder(lease.lock_file()).unwrap(), first);

    // A forced renewal does rewrite it, so another workstation can see the
    // session is alive.
    assert_eq!(lease.renew().unwrap(), Renewal::Renewed);
    let second = cloud::read_holder(lease.lock_file()).unwrap();
    assert!(second.renewed_at >= first.renewed_at);
    assert_eq!(second.acquired_at, first.acquired_at);
}

// ------------------------------------------------------------- settle waiting

#[test]
fn a_file_still_being_written_is_reported_as_unsettled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.sqlite3");
    std::fs::write(&path, b"first").unwrap();

    let policy = SyncPolicy {
        quiet: Duration::from_millis(200),
        timeout: Duration::from_millis(120),
        poll: Duration::from_millis(10),
    };

    // The timeout is shorter than the quiet period it is waiting for, which is
    // the shape of "the sync client is still going": bounded, and reported.
    let settled = cloud::wait_until_settled(&path, &policy);
    assert!(settled.is_unsettled(), "{settled:?}");
    assert!(settled.describe().contains("still changing"));
}

// -------------------------------------------------------- sync conflict copies

#[test]
fn conflict_copies_are_found_and_backups_are_not() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("keys.sqlite3");
    for name in [
        "keys.sqlite3",
        "keys (1).sqlite3",                    // OneDrive / Google Drive
        "keys (felipe's conflicted copy).db",  // Dropbox
        "keys.20260811-120000.backup.sqlite3", // ours
        "keys.sqlite3.lock",                   // ours
        "other.sqlite3",
    ] {
        std::fs::write(dir.path().join(name), b"x").unwrap();
    }

    let found: Vec<String> = cloud::conflict_copies(&database)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();

    assert!(found.contains(&"keys (1).sqlite3".to_owned()), "{found:?}");
    assert!(
        found.contains(&"keys (felipe's conflicted copy).db".to_owned()),
        "{found:?}"
    );
    assert_eq!(
        found.len(),
        2,
        "a backup and a lock are not a fork: {found:?}"
    );
}

// --------------------------------------------------------- through the `Store`

#[test]
fn opening_a_cloud_hosted_database_takes_the_lock_and_closing_releases_it() {
    let dir = onedrive_dir();
    let database = database_in(&dir);
    let lock = cloud::lock_path(&database);

    let store = Store::create_new(&quick(&database)).unwrap();
    assert_eq!(store.location(), Location::CloudSync);
    assert!(store.lease().is_some(), "the lock is not optional here");
    assert!(lock.is_file());

    let settled = store
        .close()
        .expect("a cloud-hosted close waits and reports");
    assert!(!settled.is_unsettled());
    assert!(!lock.exists(), "closing must free the register");
}

#[test]
fn a_locked_cloud_database_refuses_a_second_open_with_the_holder_in_the_error() {
    let dir = onedrive_dir();
    let database = database_in(&dir);

    let first = Store::create_new(&quick(&database)).unwrap();

    match Store::open_existing(&quick(&database)) {
        Err(StoreError::Lease(LeaseError::Held { holder, .. })) => {
            assert!(holder.is_local(), "this process is the holder here");
        }
        Err(other) => panic!("expected a lock refusal, got {other}"),
        Ok(_) => panic!("a held database must not open twice"),
    }

    // And it opens once the first session lets go.
    first.close();
    let second = Store::open_existing(&quick(&database)).unwrap();
    assert!(second.lease().is_some());
}

#[test]
fn a_local_database_takes_no_lock_file() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("keys.sqlite3");

    let store = Store::create_new(&StoreConfig::new(&database)).unwrap();
    assert_eq!(store.location(), Location::LocalDisk);
    assert!(
        store.lease().is_none(),
        "SQLite's own locking is the right mechanism on a local file"
    );
    assert!(!cloud::lock_path(&database).exists());
    assert!(store.close().is_none(), "nothing to wait for");
}

#[test]
fn the_lock_can_be_declined_for_a_read_only_look() {
    let dir = onedrive_dir();
    let database = database_in(&dir);

    let store = Store::create_new(&quick(&database).without_lease()).unwrap();
    assert!(store.lease().is_none());
    assert!(!cloud::lock_path(&database).exists());

    // And the status line says so, because an unlocked sync-hosted database is
    // the dangerous configuration this whole module exists to remove.
    let described = store.describe();
    assert!(described.contains("no single-writer lock"), "{described}");
}
