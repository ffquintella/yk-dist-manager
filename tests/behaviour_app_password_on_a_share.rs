//! Behaviour test: changing the password of a register that lives on an SMB share
//! must not disconnect the share it lives on.
//!
//! A file of its own for the usual reason — it drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared by
//! every test in a binary, so each of these gets one test.
//!
//! Why this case earns a test rather than a comment: the obvious way to reopen a
//! register after the swap is [`YkDistApp::open_database`], and that begins by
//! releasing whatever is open — which includes disconnecting a share this session
//! connected. On a local file nothing notices. On a share it takes the file away
//! before the reopen, so a password change that worked reports as one that failed,
//! and the operator is left at the chooser wondering which of the two happened to
//! their register. No file server is involved: `app.share_connector` is replaced
//! with a mock, which is what that seam exists for.

#![cfg(feature = "encrypted-db")]

use yk_dist_manager::YkDistApp;
use yk_dist_manager::app::DbRequest;
use yk_dist_manager::store::smb::{Access, MockConnector};

/// Meets the policy. Not a credential — the register it protects is deleted with
/// the temporary directory it sits in.
const PASSPHRASE: &str = "correct horse battery staple";

#[test]
fn scenario_a_register_on_a_share_can_be_encrypted_without_losing_the_share() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
    }

    // Given a register created on a share this session connected
    let root = home.path().join("mounted-ti-share");
    std::fs::create_dir_all(root.join("yubikeys")).unwrap();

    let mut app = YkDistApp::new(None);
    let mount = root.clone();
    app.share_connector = Box::new(move || Box::new(MockConnector::connecting(mount.clone())));
    app.share_form.location = "smb://fileserver/ti-share/yubikeys/keys.sqlite3".into();
    app.share_form.access = Access::Named;
    app.share_form.user = r"FGV\svc-yubikey".into();
    app.share_form.password = "typed-at-the-desk".into();
    app.db_request = Some(DbRequest::ConnectShare { create: true });
    app.handle_db_request();

    assert!(
        app.store.is_some(),
        "{:?} / {:?}",
        app.share_form.error,
        app.db_form.error
    );
    assert!(app.share.as_ref().is_some_and(|share| share.is_ours()));
    let on_the_share = app.config.path.clone();
    app.add_serial(
        20_423_633,
        yk_dist_manager::domain::SerialSource::ManualEntry,
        "intake",
    );

    // When the operator gives it a password
    app.password_form.new = PASSPHRASE.into();
    app.password_form.confirm = PASSPHRASE.into();
    app.db_request = Some(DbRequest::SetPassword { remove: false });
    app.handle_db_request();

    // Then the register is encrypted and open, at the same path on the same share…
    assert!(app.store.is_some(), "{:?}", app.db_form.error);
    assert!(app.store.as_ref().unwrap().is_encrypted());
    assert_eq!(app.config.path, on_the_share);
    assert_eq!(app.keys.len(), 1, "nothing was lost in the export");

    // …the share is still connected, and still this session's to disconnect…
    assert!(
        app.share.as_ref().is_some_and(|share| share.is_ours()),
        "re-keying a register must not take down the share it lives on"
    );

    // …and it is still opened as a file on a share rather than in WAL mode, which
    // is what reusing the stated location rather than re-detecting it protects.
    assert_eq!(
        app.store.as_ref().unwrap().location(),
        yk_dist_manager::store::Location::NetworkShare
    );

    // And closing still puts the share back the way it was found.
    app.db_request = Some(DbRequest::DisconnectShare);
    app.handle_db_request();
    assert!(app.store.is_none());
    assert!(app.share.is_none(), "the share is released on close");
}
