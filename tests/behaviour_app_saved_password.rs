//! Behaviour test for a database password kept in the workstation's credential
//! store (`features/db-password-and-encryption.md` phase 8).
//!
//! [`yk_dist_manager::vault`] is unit-tested where it lives. What is tested here is
//! the wiring, which is where the interesting mistakes are: *when* a password is
//! saved (only after it has actually opened the register, and only when asked),
//! *when* it is used (before the probe, on every route into an existing register,
//! and never in place of one somebody typed), and — the one that matters most —
//! *what happens when it stops working*.
//!
//! **One test per configuration, deliberately**, for the reason
//! `behaviour_app_password_change.rs` and `behaviour_app_unlock_throttle.rs` both
//! give: these drive `YkDistApp`, which reads `$YKDM_SETTINGS` and
//! `$YKDM_DATA_DIR`, and the process environment is shared by every test in a
//! binary. Two tests here would be two threads rewriting each other's environment,
//! so the scenario runs in phases instead, each with a register of its own.
//!
//! **Nothing here touches the real Keychain.** The application is built through
//! `YkDistApp::with_vault` with a `MemoryVault`, and `$YKDM_NO_SAVED_PASSWORD` is
//! set as well, so a route that reached for the platform store would be refused
//! rather than quietly writing to the operator's own login keyring.

use std::path::Path;

/// Redirect everything this test would otherwise write to the operator's home —
/// including the credential store.
fn scratch(home: &Path) {
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home);
        std::env::set_var("YKDM_SETTINGS", home.join("settings.json"));
        std::env::set_var("YKDM_NO_SAVED_PASSWORD", "1");
        // No waiting for a sync client that is not there — the last phase puts a
        // register in a folder the location heuristic reads as OneDrive.
        std::env::set_var("YKDM_SYNC_QUIET_MS", "0");
        std::env::set_var("YKDM_SYNC_TIMEOUT_MS", "0");
    }
}

/// A passphrase that meets the policy. Not a credential: it protects nothing and
/// the databases it opens are deleted when the test ends.
#[cfg(feature = "encrypted-db")]
const A_GOOD_PASSPHRASE: &str = "correct horse battery staple";

/// The one a register is changed to, halfway through.
#[cfg(feature = "encrypted-db")]
const ANOTHER_GOOD_PASSPHRASE: &str = "seven brass lanterns dozing";

/// Make an encrypted register at this path and close it again.
#[cfg(feature = "encrypted-db")]
fn encrypted_register(path: &Path, password: &str) {
    use yk_dist_manager::store::{Store, StoreConfig};

    let store = Store::create_new(&StoreConfig::new(path).with_password(Some(password.to_owned())))
        .unwrap();
    let _ = store.close();
}

#[cfg(feature = "encrypted-db")]
#[test]
fn scenario_the_workstation_keeps_the_password_uses_it_repairs_it_and_gives_it_back() {
    use std::sync::Arc;

    use yk_dist_manager::YkDistApp;
    use yk_dist_manager::app::DbRequest;
    use yk_dist_manager::vault::{MemoryVault, Vault, account_for_path};

    let home = tempfile::tempdir().unwrap();
    scratch(home.path());

    // The vault outlives the applications built on it, which is what makes it a
    // stand-in for a credential store: it is the *workstation*, and several
    // sessions in a row see the same one.
    let vault = Arc::new(MemoryVault::new());
    let as_vault = || -> Box<dyn Vault> { Box::new(Arc::clone(&vault)) };

    // ---------------------------------------------------------------- phase 1
    // Saving is opt-in, and happens only once the password has opened the file.

    // Given an encrypted register
    let database = home.path().join("keys.sqlite3");
    encrypted_register(&database, A_GOOD_PASSPHRASE);
    let account = account_for_path(&database);

    // When the application starts on it, nothing is saved, so it is locked
    let mut app = YkDistApp::with_vault(Some(database.clone()), as_vault());
    assert!(
        app.store.is_none(),
        "no saved password: the register is locked"
    );
    assert!(!app.saved_password);
    assert!(vault.is_empty(), "nothing is saved before anybody asks");

    // When the operator types the password *without* ticking the box
    app.db_form.path = database.display().to_string();
    app.db_form.password = A_GOOD_PASSPHRASE.to_owned();
    app.db_request = Some(DbRequest::Open(database.clone()));
    app.handle_db_request();

    // Then the register opens and nothing is kept. Saving is opt-in, every time.
    assert!(app.store.is_some(), "{:?}", app.open_error);
    assert!(
        vault.is_empty(),
        "a password is saved only when the operator asks for it to be"
    );
    assert!(!app.saved_password);

    // When they close it and open it again, this time ticking the box
    app.db_request = Some(DbRequest::Close);
    app.handle_db_request();
    app.db_form.password = A_GOOD_PASSPHRASE.to_owned();
    app.db_form.remember = true;
    app.db_request = Some(DbRequest::Open(database.clone()));
    app.handle_db_request();

    // Then it is saved, the screen knows, and the register says so in its trail
    assert!(app.store.is_some(), "{:?}", app.open_error);
    assert!(app.saved_password);
    assert_eq!(
        vault.get(&account).unwrap().as_deref(),
        Some(A_GOOD_PASSPHRASE)
    );
    assert!(
        !app.db_form.remember,
        "the tick is consumed by the open it applied to, not left set for the next register"
    );
    let trail = app
        .store
        .as_ref()
        .unwrap()
        .audit_entries(usize::MAX)
        .unwrap();
    let saved = trail
        .iter()
        .find(|entry| entry.event == "db.password.saved")
        .expect("saving a password is a state change, so it is audited");
    assert!(
        !saved.details.contains(A_GOOD_PASSPHRASE),
        "custody is recorded, never the value: {}",
        saved.details
    );
    assert!(
        saved.details.contains(&account),
        "the entry has to say which register: {}",
        saved.details
    );

    // ---------------------------------------------------------------- phase 2
    // The next launch is simply open.

    let close = app.store.take().unwrap();
    let _ = close.close();
    let mut app = YkDistApp::with_vault(Some(database.clone()), as_vault());

    assert!(
        app.store.is_some(),
        "the saved password should have opened it: {:?}",
        app.open_error
    );
    assert!(app.saved_password);
    assert!(
        app.db_form.password.is_empty(),
        "nothing is typed, so nothing is left in the field"
    );
    let trail = app
        .store
        .as_ref()
        .unwrap()
        .audit_entries(usize::MAX)
        .unwrap();
    assert!(
        trail.iter().any(|entry| entry.event == "db.unlocked"),
        "an encrypted register that opened was unlocked, however the password arrived"
    );

    // ---------------------------------------------------------------- phase 3
    // A typed password wins over a saved one, so a saved one is never a trap.

    app.db_request = Some(DbRequest::Close);
    app.handle_db_request();
    app.db_form.password = "not this register's password".to_owned();
    app.db_request = Some(DbRequest::Open(database.clone()));
    app.handle_db_request();

    assert!(
        app.store.is_none(),
        "a password in the field is the operator saying *this* one; silently \
         substituting the saved one would make a wrong saved password impossible \
         to get past"
    );
    assert!(
        vault.get(&account).unwrap().is_some(),
        "and the operator's own typo must not cost them the saved password"
    );

    // ---------------------------------------------------------------- phase 4
    // Changing the password carries the saved copy with it.

    app.db_form.password = A_GOOD_PASSPHRASE.to_owned();
    app.db_request = Some(DbRequest::Open(database.clone()));
    app.handle_db_request();
    assert!(app.store.is_some(), "{:?}", app.open_error);
    assert!(app.saved_password);

    app.password_form.open = true;
    app.password_form.remember = true;
    app.password_form.new = ANOTHER_GOOD_PASSPHRASE.to_owned();
    app.password_form.confirm = ANOTHER_GOOD_PASSPHRASE.to_owned();
    app.db_request = Some(DbRequest::SetPassword { remove: false });
    app.handle_db_request();

    // The entry holding the old password would have been offered, refused and
    // dropped at the next launch, in front of a prompt the operator was told they
    // would not see.
    assert!(app.store.is_some(), "{:?}", app.open_error);
    assert!(app.saved_password);
    assert_eq!(
        vault.get(&account).unwrap().as_deref(),
        Some(ANOTHER_GOOD_PASSPHRASE)
    );

    // ---------------------------------------------------------------- phase 5
    // Taking the password off the register takes it off the workstation.

    app.password_form.open = true;
    app.password_form.removing = true;
    app.db_request = Some(DbRequest::SetPassword { remove: true });
    app.handle_db_request();

    assert!(app.store.is_some(), "{:?}", app.open_error);
    assert!(!app.store.as_ref().unwrap().is_encrypted());
    assert!(!app.saved_password);
    assert!(
        vault.is_empty(),
        "a plain file has no secret for a credential store to hold"
    );

    // ---------------------------------------------------------------- phase 6
    // Giving it back does not touch the register itself.

    let second = home.path().join("second.sqlite3");
    encrypted_register(&second, A_GOOD_PASSPHRASE);
    let second_account = account_for_path(&second);

    app.db_request = Some(DbRequest::Close);
    app.handle_db_request();
    app.db_form.path = second.display().to_string();
    app.db_form.password = A_GOOD_PASSPHRASE.to_owned();
    app.db_form.remember = true;
    app.db_request = Some(DbRequest::Open(second.clone()));
    app.handle_db_request();
    assert!(app.store.is_some(), "{:?}", app.open_error);
    assert!(app.saved_password);

    app.db_request = Some(DbRequest::ForgetSavedPassword);
    app.handle_db_request();

    assert!(vault.get(&second_account).unwrap().is_none());
    assert!(!app.saved_password);
    let trail = app
        .store
        .as_ref()
        .unwrap()
        .audit_entries(usize::MAX)
        .unwrap();
    assert!(
        trail
            .iter()
            .any(|entry| entry.event == "db.password.forgotten"),
        "forgetting is a state change too"
    );

    let close = app.store.take().unwrap();
    let _ = close.close();
    let app = YkDistApp::with_vault(Some(second.clone()), as_vault());
    assert!(
        app.store.is_none(),
        "forgetting the saved password must not have removed the register's own password"
    );

    // ---------------------------------------------------------------- phase 7
    // A saved password that stopped working is dropped, explained, and never
    // counted against the throttle.

    let third = home.path().join("third.sqlite3");
    encrypted_register(&third, A_GOOD_PASSPHRASE);
    let third_account = account_for_path(&third);
    // What a re-key on another workstation leaves behind.
    vault.set(&third_account, ANOTHER_GOOD_PASSPHRASE).unwrap();

    let app = YkDistApp::with_vault(Some(third.clone()), as_vault());

    assert!(app.store.is_none());
    assert!(
        vault.get(&third_account).unwrap().is_none(),
        "a saved password that no longer opens the register is dropped rather than \
         left to fail again tomorrow"
    );
    assert!(!app.saved_password);
    assert!(
        app.status.contains("no longer opens it"),
        "the reason has to reach the screen: {}",
        app.status
    );
    assert!(
        !app.status.contains(ANOTHER_GOOD_PASSPHRASE) && !app.status.contains(A_GOOD_PASSPHRASE),
        "and it must not carry either password: {}",
        app.status
    );
    // Nobody guessed: the application offered a password it had been asked to
    // keep. Counting that would start every launch one attempt down for the one
    // operator who cannot fix it by typing more carefully.
    assert!(
        !app.throttle.must_wait(),
        "a refused *saved* password is not a failed attempt"
    );
    assert_eq!(app.throttle.audit_detail(), "consecutive_failures=0");

    // ---------------------------------------------------------------- phase 8
    // An abandoned single-writer lock is taken over with the saved password.
    //
    // The route the gap was on: a register whose password is saved is refused at
    // startup by the *lock*, not the password, so the field the take-over button
    // reads is empty by design. Before this, the one control offered on that
    // screen could not work.

    use yk_dist_manager::store::cloud::{self, LeaseHolder};

    let synced = home.path().join("OneDrive - Contoso");
    std::fs::create_dir_all(&synced).unwrap();
    let hosted = synced.join("keys.sqlite3");
    encrypted_register(&hosted, A_GOOD_PASSPHRASE);
    let hosted_account = account_for_path(&hosted);
    vault.set(&hosted_account, A_GOOD_PASSPHRASE).unwrap();

    let hours_ago = chrono::Utc::now() - chrono::Duration::hours(4);
    std::fs::write(
        cloud::lock_path(&hosted),
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

    let mut app = YkDistApp::with_vault(Some(hosted.clone()), as_vault());
    assert!(app.store.is_none(), "a held register must not open");
    assert!(
        vault.get(&hosted_account).unwrap().is_some(),
        "an unmounted share, another workstation's lock, a schema from a newer \
         build: none of those is the password being wrong, and forgetting a saved \
         password over one would be quiet loss"
    );
    assert!(
        app.db_form.locked.is_some(),
        "the chooser needs the holder, to offer taking the lock over"
    );
    assert!(
        app.db_form.password.is_empty(),
        "the field is empty by design: this operator was told they would not type it"
    );

    app.db_request = Some(DbRequest::TakeOverLock(hosted.clone()));
    app.handle_db_request();

    assert!(
        app.store.is_some(),
        "taking a lock over is an assertion about the *other* workstation, not a \
         reason to retype a password this one already has: {:?}",
        app.db_form.error
    );
    assert!(app.saved_password);
    let trail = app
        .store
        .as_ref()
        .unwrap()
        .audit_entries(usize::MAX)
        .unwrap();
    assert!(
        trail
            .iter()
            .any(|entry| entry.event == "db.lock.taken_over"),
        "breaking somebody else's lock is audited, however the register was unlocked"
    );

    // ---------------------------------------------------------------- phase 9
    // A workstation with no credential store still opens the register by typing,
    // and says out loud that the save did not happen.

    let fourth = home.path().join("fourth.sqlite3");
    encrypted_register(&fourth, A_GOOD_PASSPHRASE);

    let mut app = YkDistApp::with_vault(Some(fourth.clone()), Box::new(MemoryVault::unavailable()));
    app.db_form.path = fourth.display().to_string();
    app.db_form.password = A_GOOD_PASSPHRASE.to_owned();
    app.db_form.remember = true;
    app.db_request = Some(DbRequest::Open(fourth.clone()));
    app.handle_db_request();

    assert!(
        app.store.is_some(),
        "no credential store is a reason to keep typing the password, never a \
         reason to refuse the register: {:?}",
        app.open_error
    );
    assert!(!app.saved_password);
    assert!(
        app.status.starts_with("WARNING:"),
        "the operator ticked a box, and is owed the news now rather than at the \
         next launch: {}",
        app.status
    );
}

/// The other side of the rule, in a build that cannot encrypt anything.
///
/// A plain register has no password, so there is nothing to save and nothing to
/// remember — and ticking the box must not invent something to put in the
/// credential store.
#[cfg(not(feature = "encrypted-db"))]
#[test]
fn scenario_a_plain_register_has_no_password_to_save() {
    use std::sync::Arc;

    use yk_dist_manager::YkDistApp;
    use yk_dist_manager::app::DbRequest;
    use yk_dist_manager::vault::{MemoryVault, Vault};

    let home = tempfile::tempdir().unwrap();
    scratch(home.path());

    let vault = Arc::new(MemoryVault::new());
    let as_vault = || -> Box<dyn Vault> { Box::new(Arc::clone(&vault)) };

    // Given a plain register
    let database = home.path().join("keys.sqlite3");
    let mut app = YkDistApp::with_vault(Some(database.clone()), as_vault());
    assert!(app.store.is_some(), "{:?}", app.open_error);

    // When the operator ticks the box and opens it again
    app.db_request = Some(DbRequest::Close);
    app.handle_db_request();
    app.db_form.remember = true;
    app.db_request = Some(DbRequest::Open(database.clone()));
    app.handle_db_request();

    // Then it opens, and nothing was saved: there is no secret here to keep. A
    // plain file is readable by everybody who can read it, and an entry in the
    // credential store would say otherwise.
    assert!(app.store.is_some(), "{:?}", app.open_error);
    assert!(vault.is_empty());
    assert!(!app.saved_password);
}
