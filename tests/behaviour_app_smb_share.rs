//! Behaviour test for the application's half of hosting the register on an SMB
//! share: connecting, opening, auditing, and putting the share back the way it was
//! found.
//!
//! **One test in this file, deliberately** — the same reason as
//! `behaviour_app_cloud_lock.rs`. It drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared by
//! every test in a binary. One test means no race, and pointing both variables at a
//! temporary directory means a test run never touches the operator's real settings
//! file.
//!
//! No file server is involved: `app.share_connector` is replaced with a mock, which
//! is what that seam exists for.

use yk_dist_manager::YkDistApp;
use yk_dist_manager::app::DbRequest;
use yk_dist_manager::store::smb::{Access, MockConnector};

#[test]
fn scenario_the_application_connects_a_share_audits_it_and_disconnects_on_close() {
    // Given a scratch home, so nothing here writes to the real settings file
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
    }

    // And a file server whose share, once connected, is reachable here
    let root = home.path().join("mounted-ti-share");
    let location = "smb://fileserver/ti-share/yubikeys/keys.sqlite3";

    let mut app = YkDistApp::new(None);
    let mount = root.clone();
    app.share_connector = Box::new(move || Box::new(MockConnector::connecting(mount.clone())));

    // When the operator names the share, chooses a service account, and creates the
    // register on it
    app.share_form.location = location.into();
    app.share_form.access = Access::Named;
    app.share_form.user = r"FGV\svc-yubikey".into();
    app.share_form.password = "typed-at-the-desk".into();
    std::fs::create_dir_all(root.join("yubikeys")).unwrap();
    app.db_request = Some(DbRequest::ConnectShare { create: true });
    app.handle_db_request();

    // Then the register is open on the share, and the share is held by this session
    assert!(
        app.store.is_some(),
        "{:?} / {:?}",
        app.share_form.error,
        app.db_form.error
    );
    assert!(
        app.share.is_some(),
        "the connection is held for the session"
    );
    assert!(app.share.as_ref().unwrap().is_ours());
    assert_eq!(
        app.config.path,
        root.join("yubikeys").join("keys.sqlite3"),
        "the database is the share's root plus the path inside it"
    );
    assert_eq!(
        app.store.as_ref().unwrap().location(),
        yk_dist_manager::store::Location::NetworkShare,
        "a file on a share must not be opened in WAL mode"
    );

    // And the password is gone from the form the moment it was used
    assert!(
        app.share_form.password.is_empty(),
        "a password held in a form is a password waiting to be written somewhere"
    );

    // And the connection is in the audit trail, naming the share and the identity
    let connected = app
        .store
        .as_ref()
        .unwrap()
        .audit_entries(20)
        .unwrap()
        .into_iter()
        .find(|entry| entry.event == "db.share.connected")
        .expect("reaching a file server to open the register is a state change");
    assert!(
        connected.details.contains("//fileserver/ti-share"),
        "{}",
        connected.details
    );
    assert!(
        connected.details.contains("svc-yubikey"),
        "the entry must say as whom: {}",
        connected.details
    );
    assert!(
        !connected.details.contains("typed-at-the-desk"),
        "no secret in the audit trail: {}",
        connected.details
    );

    // And the share is remembered for the next hand-over, without its password
    let settings_on_disk = std::fs::read_to_string(home.path().join("settings.json")).unwrap();
    assert!(settings_on_disk.contains("//fileserver/ti-share/yubikeys/keys.sqlite3"));
    assert!(settings_on_disk.contains("svc-yubikey"));
    assert!(!settings_on_disk.contains("typed-at-the-desk"));

    // When the operator closes the register and disconnects
    let database = app.config.path.clone();
    app.db_request = Some(DbRequest::DisconnectShare);
    app.handle_db_request();

    // Then both are let go, in that order — the disconnection is recorded *before*
    // the close, because after the close there is no database to record it in
    assert!(app.store.is_none());
    assert!(
        app.share.is_none(),
        "the share this session made is released"
    );

    let reopened = yk_dist_manager::store::Store::open_existing(
        &yk_dist_manager::store::StoreConfig::new(&database),
    )
    .unwrap();
    let events: Vec<String> = reopened
        .audit_entries(50)
        .unwrap()
        .into_iter()
        .map(|entry| entry.event)
        .collect();
    let disconnected = events
        .iter()
        .position(|event| event == "db.share.disconnected")
        .expect("giving the share back is a state change and is audited");
    let closed = events
        .iter()
        .position(|event| event == "db.closed")
        .expect("closing is audited");
    // `audit_entries` is newest first, so "before" is a *higher* index.
    assert!(
        disconnected > closed,
        "the share entry must be written before the close: {events:?}"
    );
    let _ = reopened.close();

    // And when a second operator's workstation already has the share mounted, this
    // session uses it and leaves it exactly as it found it
    let adopted = root.clone();
    app.share_connector = Box::new(move || Box::new(MockConnector::adopting(adopted.clone())));
    app.share_form.location = location.into();
    app.share_form.access = Access::LoggedOnUser;
    app.db_request = Some(DbRequest::ConnectShare { create: false });
    app.handle_db_request();

    assert!(app.store.is_some(), "{:?}", app.share_form.error);
    assert!(
        app.share.as_ref().is_some_and(|share| !share.is_ours()),
        "a mount this session did not make is not this session's to unmount"
    );

    app.db_request = Some(DbRequest::Close);
    app.handle_db_request();
    assert!(root.is_dir(), "somebody else's mount survives our close");

    // And a location that names a share but no file is refused, with the fix in the
    // message and nothing opened
    app.share_form.location = "smb://fileserver/ti-share".into();
    app.db_request = Some(DbRequest::ConnectShare { create: false });
    app.handle_db_request();
    assert!(app.store.is_none());
    let refusal = app.share_form.error.clone().expect("a refusal is shown");
    assert!(refusal.contains("no database inside it"), "{refusal}");

    // As is a location that names an account the chosen identity contradicts: the
    // register must never be opened as somebody the operator did not pick.
    app.share_form.location = "smb://ana@fileserver/ti-share/keys.sqlite3".into();
    app.share_form.access = Access::Anonymous;
    app.db_request = Some(DbRequest::ConnectShare { create: false });
    app.handle_db_request();
    assert!(app.store.is_none());
    let refusal = app.share_form.error.clone().expect("a refusal is shown");
    assert!(refusal.contains("ana"), "{refusal}");
    assert!(refusal.contains("guest"), "{refusal}");

    // And a share that refuses the credential opens nothing and holds nothing
    app.share_connector = Box::new(|| {
        Box::new(MockConnector::refusing(
            "the user name or password was refused",
        ))
    });
    app.share_form.location = location.into();
    app.share_form.access = Access::Named;
    app.share_form.user = "felipe".into();
    app.share_form.password = "wrong".into();
    app.db_request = Some(DbRequest::ConnectShare { create: false });
    app.handle_db_request();

    assert!(app.store.is_none(), "a refused share must open no register");
    assert!(app.share.is_none(), "and must leave nothing attached");
    let refusal = app.share_form.error.clone().expect("a refusal is shown");
    assert!(refusal.contains("//fileserver/ti-share"), "{refusal}");
    assert!(
        !refusal.contains("wrong"),
        "no password in a refusal: {refusal}"
    );
    assert!(app.share_form.password.is_empty());

    // Finally: quitting the application, which is what operators actually do,
    // releases the share as well as closing the register
    let quitting_root = home.path().join("mounted-again");
    let mut quitting = YkDistApp::new(None);
    let mount = quitting_root.clone();
    quitting.share_connector = Box::new(move || Box::new(MockConnector::connecting(mount.clone())));
    quitting.share_form.location = "smb://fileserver/ti-share/keys.sqlite3".into();
    quitting.share_form.access = Access::Anonymous;
    quitting.db_request = Some(DbRequest::ConnectShare { create: true });
    quitting.handle_db_request();
    assert!(quitting.store.is_some(), "{:?}", quitting.share_form.error);
    assert!(quitting.share.is_some());
    drop(quitting);
    // Nothing to assert about the mock's mount point beyond this: the connection
    // was dropped, and `ShareConnection`'s own suite proves a drop disconnects.
}
