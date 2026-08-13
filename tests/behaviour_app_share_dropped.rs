//! Behaviour test for a share that goes away under an open register
//! (`features/smb-share-hosting.md` phase 9).
//!
//! The failure this replaces: until now a dropped share surfaced as whatever SQLite
//! error the next operation happened to hit — mid-hand-over, in the middle of
//! recording a distribution — and the operator had to work out from it that the file
//! server had gone, then find the share card and retype the location.
//!
//! Three things have to hold, and each is a way of getting it wrong:
//!
//! 1. The register is **let go of, not closed politely**. The polite close writes
//!    `db.closed` into the register first, and the register is exactly what is no
//!    longer reachable — so the write fails and reports an audit failure for a fault
//!    that is not one.
//! 2. A share reached as the signed-in user or as a guest is **reconnected without
//!    being asked**, because nothing needs typing; a named account is **not**,
//!    because its password was used once and dropped.
//! 3. Coming back is **audited on the register that came back**, which is the only
//!    place it can be written.
//!
//! **One test in this file, deliberately** — it drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared by
//! every test in a binary. No file server: `app.share_connector` is a mock, which is
//! what that seam exists for.

use yk_dist_manager::YkDistApp;
use yk_dist_manager::app::DbRequest;
use yk_dist_manager::store::smb::{Access, MockConnector};

fn events(app: &YkDistApp) -> Vec<String> {
    app.store
        .as_ref()
        .expect("a register is open")
        .audit_entries(200)
        .expect("the trail reads back")
        .into_iter()
        .map(|entry| entry.event)
        .collect()
}

#[test]
fn scenario_a_share_that_drops_is_noticed_reconnected_and_recorded() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
    }

    // Given a register on a share, reached as the signed-in user — the identity that
    // needs no password, and so the one that can be reconnected without asking
    let root = home.path().join("mounted-ti-share");
    let location = "smb://fileserver/ti-share/yubikeys/keys.sqlite3";
    let mount = root.clone();

    let mut app = YkDistApp::new(None);
    app.share_connector = Box::new(move || Box::new(MockConnector::connecting(mount.clone())));
    app.share_form.location = location.into();
    app.share_form.access = Access::LoggedOnUser;
    std::fs::create_dir_all(root.join("yubikeys")).unwrap();
    app.db_request = Some(DbRequest::ConnectShare { create: true });
    app.handle_db_request();

    assert!(
        app.store.is_some(),
        "{:?} / {:?}",
        app.share_form.error,
        app.db_form.error
    );
    assert!(app.share.is_some());
    let database = app.config.path.clone();
    assert!(database.is_file());

    // And something recorded on it, so there is history to be intact about
    app.add_serial(
        20_423_633,
        yk_dist_manager::domain::SerialSource::ManualEntry,
        "intake",
    );
    assert_eq!(app.keys.len(), 1);

    // When the file server goes away. Simulated by moving the mount aside rather
    // than deleting it, because that is what actually happens: the path stops
    // resolving and **the register keeps existing on the server**. Deleting it would
    // be testing data loss, which is a different — and not real — scenario.
    let parked = home.path().join("server-side-ti-share");
    std::fs::rename(&root, &parked).unwrap();
    assert!(!database.is_file(), "the register is not reachable");

    // Then the very next health tick notices, lets go of the register, and — because
    // this identity needs no password — tries immediately. The share is still gone,
    // so it says so and offers the way back rather than failing silently.
    app.tick_share_health();
    assert!(
        app.store.is_none(),
        "a register whose file is gone must not be held open"
    );
    assert!(app.share.is_none(), "the dead connection goes with it");
    assert!(
        app.keys.is_empty(),
        "no screen may keep showing rows from a register this session cannot read"
    );
    let lost = app
        .share_lost
        .as_ref()
        .expect("the share is remembered while it is lost");
    assert_eq!(lost.access, Access::LoggedOnUser);
    // The message is about the *share*, not about a missing file: the automatic
    // attempt ran and the share was still gone, and "no database file at …" would
    // send the operator looking for a register they never moved.
    let said = app.open_error.clone().expect("the operator is told");
    assert!(said.contains("reachable"), "{said}");
    assert!(
        said.contains("intact"),
        "the operator has to be told the register survived: {said}"
    );
    assert!(
        !said.starts_with("no database file"),
        "the share is the story here, not the path: {said}"
    );

    // And nothing tried to write a closing entry into a file that is not there: no
    // audit failure, which is what the polite close would have produced
    assert!(
        !app.status.contains("AUDIT FAILURE"),
        "letting go is not a failure to audit: {}",
        app.status
    );

    // When the file server comes back, with the register as it was
    std::fs::rename(&parked, &root).unwrap();
    assert!(database.is_file());

    // Then reconnecting reopens the register with its history
    app.reconnect_dropped_share();
    assert!(
        app.store.is_some(),
        "{:?} / {:?}",
        app.open_error,
        app.db_form.error
    );
    assert!(app.share.is_some());
    assert!(app.share_lost.is_none(), "no longer lost");
    assert_eq!(app.keys.len(), 1, "the register came back with its rows");
    assert!(
        app.status.contains("is back"),
        "the operator is told: {}",
        app.status
    );

    // And the round trip is on the record, on the register that came back — the only
    // place it can be written
    let recorded = events(&app);
    assert!(
        recorded.contains(&"db.share.reconnected".to_owned()),
        "{recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .filter(|e| *e == "db.share.connected")
            .count()
            >= 2,
        "the first connection and the reconnection: {recorded:?}"
    );
    assert!(
        !app.store.as_ref().unwrap().chain_status().is_broken(),
        "the chain still verifies across the gap"
    );

    // And a share reached with a **named account** is not reconnected for the
    // operator: the password was used once and dropped, which is the rule the whole
    // share feature is built on.
    app.share_lost = Some(yk_dist_manager::app::LostShare {
        location: location.into(),
        identity: r"FGV\svc-yubikey".into(),
        access: Access::Named,
        user: r"FGV\svc-yubikey".into(),
    });
    std::fs::rename(&root, &parked).unwrap();
    app.share_checked = None;
    app.tick_share_health();
    assert!(app.store.is_none() || app.share.is_none());
    assert!(
        app.share_lost.is_some(),
        "the card stays up, waiting for the password"
    );
}
