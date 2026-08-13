//! Behaviour test for the application's half of the cloud-sync lock protocol:
//! opening takes the lock, a refusal is turned into an action, taking a lock over
//! is audited, and closing releases it.
//!
//! **One test in this file, deliberately.** It drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared
//! by every test in a binary. One test means no race, and pointing both variables
//! at a temporary directory means the operator's real settings file is never
//! touched by a test run.

use std::path::PathBuf;

use yk_dist_manager::YkDistApp;
use yk_dist_manager::app::DbRequest;
use yk_dist_manager::store::cloud::{self, LeaseHolder};

#[test]
fn scenario_the_application_takes_the_lock_reports_a_refusal_and_releases_on_close() {
    // Given a scratch home, so nothing here writes to the real settings file
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
        // No waiting for a sync client that is not there.
        std::env::set_var("YKDM_SYNC_QUIET_MS", "0");
        std::env::set_var("YKDM_SYNC_TIMEOUT_MS", "0");
    }

    // And a register in a folder the location heuristic reads as OneDrive
    let synced = home.path().join("OneDrive - Contoso");
    std::fs::create_dir_all(&synced).unwrap();
    let database: PathBuf = synced.join("yk-dist-manager.sqlite3");
    let lock = cloud::lock_path(&database);

    // When the application starts on that path (`$YKDM_DB`'s path, here passed
    // straight in) and opens the register
    let mut app = YkDistApp::new(Some(database.clone()));

    // Then it is open and the lock is held by this workstation
    assert!(app.store.is_some(), "{:?}", app.db_form.error);
    assert!(lock.is_file(), "opening must take the single-writer lock");
    assert!(app.store.as_ref().unwrap().lease().is_some());

    // When the operator closes it
    app.db_request = Some(DbRequest::Close);
    app.handle_db_request();

    // Then the lock is gone — the next workstation may open the register — and the
    // close is in the audit trail, naming the lock it gave up
    assert!(!lock.exists(), "closing must release the lock");
    let reopened = yk_dist_manager::store::Store::open_existing(
        &yk_dist_manager::store::StoreConfig::new(&database),
    )
    .unwrap();
    let closed = reopened
        .audit_entries(20)
        .unwrap()
        .into_iter()
        .find(|entry| entry.event == "db.closed")
        .expect("closing a database is a state change and is audited");
    assert!(
        closed.details.contains("single-writer lock"),
        "the entry must say the lock was given up: {}",
        closed.details
    );
    let _ = reopened.close();

    // When another workstation's lock is sitting there, abandoned
    let hours_ago = chrono::Utc::now() - chrono::Duration::hours(4);
    std::fs::write(
        &lock,
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

    // And the operator tries to open the register
    app.db_request = Some(DbRequest::Open(database.clone()));
    app.handle_db_request();

    // Then it stays closed, and the refusal is an *action* rather than a message:
    // who has it, and that it is old enough to take over
    assert!(app.store.is_none(), "a held register must not open");
    let refused = app
        .db_form
        .locked
        .as_ref()
        .expect("the chooser needs the holder, to offer taking the lock over");
    assert!(refused.holder.contains("ana"), "{}", refused.holder);
    assert!(refused.holder.contains("MAC-RECEPCAO"));
    assert!(refused.stale, "four hours of silence is abandoned");
    assert!(!refused.same_host, "another computer, not this one");

    // When the operator takes it over deliberately
    app.db_request = Some(DbRequest::TakeOverLock(database.clone()));
    app.handle_db_request();

    // Then the register opens, the refusal card is gone, and who was holding it is
    // in the audit trail — the record of a decision that could have cost a hand-over
    assert!(app.store.is_some(), "{:?}", app.db_form.error);
    assert!(app.db_form.locked.is_none());
    let entry = app
        .store
        .as_ref()
        .unwrap()
        .audit_entries(20)
        .unwrap()
        .into_iter()
        .find(|entry| entry.event == "db.lock.taken_over")
        .expect("breaking somebody else's lock is audited");
    assert!(entry.details.contains("ana"), "{}", entry.details);
    assert!(entry.details.contains("MAC-RECEPCAO"), "{}", entry.details);

    app.db_request = Some(DbRequest::Close);
    app.handle_db_request();
    assert!(!lock.exists());

    // And when the operator quits the application instead of closing the database
    // — which is what actually happens at the end of an afternoon
    let quitting = YkDistApp::new(Some(database.clone()));
    assert!(quitting.store.is_some());
    assert!(lock.is_file());
    drop(quitting);

    // Then the register is free for the next workstation all the same
    assert!(
        !lock.exists(),
        "quitting must release the lock, not leave it to go stale"
    );

    // When the lock waiting there belongs to a session that is still alive — the
    // ordinary case, a second window somebody left open — and the application is
    // *started* on that register, which is how an operator meets this at all
    let a_moment_ago = chrono::Utc::now() - chrono::Duration::seconds(47);
    let alive = LeaseHolder {
        host: "ARJ2247.local".into(),
        operator: "felipe".into(),
        pid: 60136,
        session: uuid::Uuid::from_u128(0xF00D),
        app_version: "0.12.0".into(),
        acquired_at: a_moment_ago,
        renewed_at: a_moment_ago,
    };
    std::fs::write(&lock, serde_json::to_string(&alive).unwrap()).unwrap();
    let mut app = YkDistApp::new(Some(database.clone()));

    // Then the refusal is a card and not just a sentence. Startup used to be the
    // one path that reported a held register as a bare message, which left the
    // operator reading who had it with no way to act on it.
    assert!(app.store.is_none(), "a held register must not open");
    let refused = app
        .db_form
        .locked
        .as_ref()
        .expect("a refusal at startup is a refusal like any other");
    assert!(refused.holder.contains("felipe"), "{}", refused.holder);
    assert!(!refused.stale, "forty-seven seconds is a live session");
    assert!(
        !refused.break_confirmed,
        "the tick that arms taking a live lock over starts clear"
    );

    // When the operator takes that live lock over deliberately
    app.db_request = Some(DbRequest::TakeOverLock(database.clone()));
    app.handle_db_request();

    // Then the register opens, and the trail says it was taken from a session that
    // was still refreshing — the difference between clearing up after a crash and
    // cutting in on somebody, which is the part a later reader has to be able to see
    assert!(app.store.is_some(), "{:?}", app.db_form.error);
    let entry = app
        .store
        .as_ref()
        .unwrap()
        .audit_entries(20)
        .unwrap()
        .into_iter()
        .find(|entry| entry.event == "db.lock.taken_over")
        .expect("breaking a live lock is audited");
    assert!(entry.details.contains("felipe"), "{}", entry.details);
    assert!(
        entry.details.contains("still being refreshed"),
        "the entry must say the lock was live: {}",
        entry.details
    );

    app.db_request = Some(DbRequest::Close);
    app.handle_db_request();
    assert!(!lock.exists());
}
