//! Behaviour tests for a register hosted on an SMB share.
//!
//! The share is a mock connection over a temporary directory, so these run with no
//! file server, no credentials that exist anywhere and no network. What is being
//! exercised is the whole sequence an operator goes through: reach the share, create
//! or open the register on it, write a hand-over, close, and have the share put back
//! the way it was found.

use yk_dist_manager::device::DeviceInfo;
use yk_dist_manager::domain::{Holder, YubiKeyRecord};
use yk_dist_manager::settings::{AppSettings, ShareEntry};
use yk_dist_manager::store::smb::{self, Access, Credential, MockConnector, ShareConnection};
use yk_dist_manager::store::{Location, Store, StoreConfig, StoreError};

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

/// A share whose root is a temporary directory, connected by this session.
fn connect(root: &std::path::Path, location: &str, credential: &Credential) -> ShareConnection {
    let target = smb::parse(location).unwrap().target;
    ShareConnection::open(
        &target,
        credential,
        Box::new(MockConnector::connecting(root)),
    )
    .unwrap()
}

#[test]
fn scenario_a_register_on_a_guest_share_is_created_written_and_reopened() {
    // Given a NAS share that allows guest access, and no register on it yet
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("public");
    let location = "smb://nas/public/yubikeys/keys.sqlite3";

    // When the operator connects as a guest and creates the register
    let share = connect(&root, location, &Credential::anonymous());
    let database = share.database_path();
    std::fs::create_dir_all(database.parent().unwrap()).unwrap();
    let store = Store::create_new(&share.store_config()).unwrap();

    // Then it is on the share, and the register knows it is on a share: a shared
    // file must not be in WAL mode, whose shared-memory sidecar cannot cross a
    // network filesystem.
    assert!(database.starts_with(&root));
    assert_eq!(store.location(), Location::NetworkShare);
    assert!(share.describe().contains("guest"));

    // And a hand-over written there survives being closed and opened again
    store.upsert_key(&sample_key(20_423_633)).unwrap();
    store
        .insert_holder(&Holder::new("Ana", "ana@example.org", "ESI", "").unwrap())
        .unwrap();
    store.close();

    let reopened = Store::open_existing(&share.store_config()).unwrap();
    assert_eq!(reopened.keys().unwrap().len(), 1);
    assert_eq!(reopened.holders().unwrap().len(), 1);
    reopened.close();
    share.close().unwrap();
}

#[test]
fn scenario_a_share_hosted_register_avoids_wal_and_leaves_no_sidecars() {
    // Given a register created on a share
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ti-share");
    let share = connect(
        &root,
        "smb://fileserver/ti-share/keys.sqlite3",
        &Credential::logged_on_user(),
    );
    // When it is used
    let store = Store::create_new(&share.store_config()).unwrap();
    store.upsert_key(&sample_key(1)).unwrap();
    store.close();

    // Then no `-wal` or `-shm` file was ever created next to it: those are the
    // files a network filesystem cannot keep in step with the database.
    let leftovers: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with("-wal") || name.ends_with("-shm"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    share.close().unwrap();
}

#[test]
fn scenario_a_typo_on_a_share_is_refused_rather_than_creating_a_second_register() {
    // Given a share with the real register on it
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ti-share");
    let share = connect(
        &root,
        "smb://fileserver/ti-share/keys.sqlite3",
        &Credential::logged_on_user(),
    );
    Store::create_new(&share.store_config()).unwrap().close();

    // When the operator mistypes the file name and opens it
    let mistyped = smb::parse("smb://fileserver/ti-share/kesy.sqlite3")
        .unwrap()
        .target
        .database_path(&root);
    let outcome = Store::open_existing(&StoreConfig::new(&mistyped));

    // Then it is a refusal, not an empty register that looks like total data loss
    let Err(error) = outcome else {
        panic!("a mistyped file name on a share must not open");
    };
    assert!(matches!(error, StoreError::Missing(_)), "{error}");
    assert!(!mistyped.exists(), "nothing may have been created");
    share.close().unwrap();
}

#[test]
fn scenario_a_named_credential_is_remembered_by_identity_and_never_by_password() {
    // Given an operator who reaches the unit's share with a service account
    let dir = tempfile::tempdir().unwrap();
    let settings_file = dir.path().join("settings.json");
    unsafe { std::env::set_var("YKDM_SETTINGS", &settings_file) };

    let root = dir.path().join("ti-share");
    let location = "smb://fileserver/ti-share/yubikeys/keys.sqlite3";
    let share = connect(
        &root,
        location,
        &Credential::named(r"FGV\svc-yubikey", "typed-at-the-desk"),
    );

    // When the share is remembered so the next hand-over need not retype it
    let mut settings = AppSettings::load();
    settings.remember_share(ShareEntry {
        location: share.target().location().to_owned(),
        access: Access::Named,
        user: r"FGV\svc-yubikey".into(),
    });
    settings.save().unwrap();

    // Then the file on disk carries the share and the account, and nowhere in it is
    // there a password: it sits next to the register, so storing one would defeat
    // both the share's permissions and the database password.
    let written = std::fs::read_to_string(&settings_file).unwrap();
    assert!(
        written.contains("//fileserver/ti-share/yubikeys/keys.sqlite3"),
        "{written}"
    );
    assert!(written.contains("svc-yubikey"), "{written}");
    assert!(written.contains("named"), "{written}");
    assert!(!written.contains("typed-at-the-desk"), "{written}");
    assert!(!written.to_lowercase().contains("password"), "{written}");

    // And it comes back on the next launch, with the password field empty.
    let reloaded = AppSettings::load();
    assert_eq!(reloaded.recent_shares.len(), 1);
    assert_eq!(reloaded.recent_shares[0].access, Access::Named);
    assert_eq!(reloaded.recent_shares[0].user, r"FGV\svc-yubikey");

    share.close().unwrap();
    unsafe { std::env::remove_var("YKDM_SETTINGS") };
}

#[test]
fn scenario_a_share_the_operator_had_already_mounted_survives_the_register_closing() {
    // Given a share Finder mounted this morning, with the register on it
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ti-share");
    std::fs::create_dir_all(&root).unwrap();

    let target = smb::parse("smb://fileserver/ti-share/keys.sqlite3")
        .unwrap()
        .target;
    let connector = MockConnector::adopting(&root);
    let calls = connector.calls();
    let share =
        ShareConnection::open(&target, &Credential::logged_on_user(), Box::new(connector)).unwrap();

    // When this application uses it and then closes the register
    let store = Store::create_new(&share.store_config()).unwrap();
    store.upsert_key(&sample_key(7)).unwrap();
    store.close();
    share.close().unwrap();

    // Then the mount is exactly as it was found. Unmounting a share the operator
    // mounted for their own work would be worse than never mounting one.
    assert!(root.is_dir());
    assert!(
        !calls.lock().unwrap().contains(&"disconnect".to_owned()),
        "this session did not mount it, so it is not this session's to unmount"
    );
    assert!(share_line_says_already_mounted(&target, &root));
}

fn share_line_says_already_mounted(target: &smb::ShareTarget, root: &std::path::Path) -> bool {
    let connection = ShareConnection::open(
        target,
        &Credential::logged_on_user(),
        Box::new(MockConnector::adopting(root)),
    )
    .unwrap();
    let line = connection.describe();
    line.contains("already mounted")
}

#[test]
fn scenario_a_refused_share_opens_no_database_and_names_what_to_fix() {
    // Given a share that refuses the account the operator typed
    let target = smb::parse("smb://fileserver/ti-share/keys.sqlite3")
        .unwrap()
        .target;

    // When the connection is attempted
    let error = ShareConnection::open(
        &target,
        &Credential::named("felipe", "wrong"),
        Box::new(MockConnector::refusing(
            "the user name or password was refused",
        )),
    )
    .unwrap_err();

    // Then the operator is told which share, which account and what was wrong —
    // and no register was opened, so there is nothing to audit and nothing to close
    let message = error.to_string();
    assert!(message.contains("//fileserver/ti-share"), "{message}");
    assert!(message.contains("felipe"), "{message}");
    assert!(message.contains("user name or password"), "{message}");
    assert!(
        !message.contains("wrong"),
        "no password in a refusal: {message}"
    );
}
