//! Behaviour test for setting, changing and removing the database password from
//! the application (`features/db-password-and-encryption.md` phases 2, 5 and 6).
//!
//! [`yk_dist_manager::store::Store::change_password`] is covered where it lives,
//! in `behaviour_storage.rs`; this is the part around it. Two things here are
//! easy to get wrong and impossible to notice from the store's own tests:
//!
//! * the operation **consumes** the `Store`, so a refusal that happens *after*
//!   the store has been taken closes the register in order to say "no";
//! * afterwards the session has to be pointed at the file again, under the new
//!   password, or the application is left holding a handle on a file that no
//!   longer exists.
//!
//! **One test in this file, deliberately** — it drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared by
//! every test in a binary.

#![cfg(feature = "encrypted-db")]

use yk_dist_manager::YkDistApp;
use yk_dist_manager::app::DbRequest;
use yk_dist_manager::store::{Store, StoreConfig, StoreError};

/// Passphrases that meet the policy. Not credentials: they protect nothing, and
/// the register they open is deleted when the test ends.
const FIRST: &str = "correct horse battery staple";
const SECOND: &str = "seven green bicycles waiting";

#[test]
fn scenario_the_operator_sets_changes_and_removes_the_password_from_settings() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
    }

    // Given a plain register the application has open, with something in it
    let database = home.path().join("keys.sqlite3");
    let mut app = YkDistApp::new(Some(database.clone()));
    assert!(app.store.is_some(), "{:?}", app.db_form.error);
    assert!(!app.store.as_ref().unwrap().is_encrypted());
    app.add_serial(
        20_423_633,
        yk_dist_manager::domain::SerialSource::ManualEntry,
        "intake",
    );
    assert_eq!(app.keys.len(), 1);

    // When a password below the floor is submitted
    app.password_form.new = "hunter2".into();
    app.password_form.confirm = "hunter2".into();
    app.db_request = Some(DbRequest::SetPassword { remove: false });
    app.handle_db_request();

    // Then it is refused *with the register still open*. This is the guard worth
    // pinning: the operation consumes the store, so a refusal reached after that
    // point would have closed the register in order to say no.
    assert!(
        app.store.is_some(),
        "a refused password must not cost the operator their session"
    );
    assert!(!app.store.as_ref().unwrap().is_encrypted());
    let refusal = app.password_form.error.clone().expect("a reason on screen");
    assert!(refusal.contains("12"), "{refusal}");

    // When the two fields disagree, likewise — and this is the typo that would
    // otherwise be discovered at the next unlock, with no way back
    app.password_form.new = FIRST.into();
    app.password_form.confirm = SECOND.into();
    app.db_request = Some(DbRequest::SetPassword { remove: false });
    app.handle_db_request();
    assert!(app.store.is_some());
    assert!(!app.store.as_ref().unwrap().is_encrypted());
    assert!(
        app.password_form
            .error
            .as_ref()
            .is_some_and(|e| e.contains("not the same")),
        "{:?}",
        app.password_form.error
    );

    // When a good passphrase is confirmed
    app.password_form.new = FIRST.into();
    app.password_form.confirm = FIRST.into();
    app.db_request = Some(DbRequest::SetPassword { remove: false });
    app.handle_db_request();

    // Then the register is encrypted, still open, and still complete — and the
    // typed password is gone from the form
    assert!(app.store.is_some(), "{:?}", app.db_form.error);
    assert!(app.store.as_ref().unwrap().is_encrypted());
    assert_eq!(app.keys.len(), 1, "nothing was lost in the export");
    assert!(app.password_form.new.is_empty());
    assert!(app.password_form.confirm.is_empty());
    assert!(!app.password_form.open, "the form closes on success");

    // And the file itself now needs the password
    match Store::open_existing(&StoreConfig::new(&database)) {
        Err(StoreError::PasswordRequired) => {}
        Err(other) => panic!("expected PasswordRequired, got {other}"),
        Ok(_) => panic!("the file must no longer open without a password"),
    }

    // And the change is on the record, inside the encrypted file
    assert!(
        app.audit_view.iter().any(|e| e.event == "db.encrypted"),
        "{:?}",
        events(&app)
    );

    // When the password is changed
    app.password_form.new = SECOND.into();
    app.password_form.confirm = SECOND.into();
    app.db_request = Some(DbRequest::SetPassword { remove: false });
    app.handle_db_request();

    // Then the new one is what opens it, the old one is dead, and the session
    // never had to be closed and reopened by hand
    assert!(app.store.is_some(), "{:?}", app.db_form.error);
    assert!(app.store.as_ref().unwrap().is_encrypted());
    assert_eq!(app.keys.len(), 1);
    assert!(
        app.audit_view
            .iter()
            .any(|e| e.event == "db.password.changed"),
        "{:?}",
        events(&app)
    );
    match Store::open_existing(&StoreConfig::new(&database).with_password(Some(FIRST.into()))) {
        Err(StoreError::PasswordRequired) => {}
        Err(other) => panic!("expected PasswordRequired, got {other}"),
        Ok(_) => panic!("the old password must stop working"),
    }

    // When the operator takes the password off again
    app.db_request = Some(DbRequest::SetPassword { remove: true });
    app.handle_db_request();

    // Then the file is plain, the register is intact, and the trail says so
    assert!(app.store.is_some(), "{:?}", app.db_form.error);
    assert!(!app.store.as_ref().unwrap().is_encrypted());
    assert_eq!(app.keys.len(), 1);
    let plain = Store::open_existing(&StoreConfig::new(&database)).unwrap();
    assert_eq!(plain.keys().unwrap().len(), 1);
    assert!(
        !plain.chain_status().is_broken(),
        "the audit chain has to survive every one of those exports"
    );
    let recorded: Vec<String> = plain
        .audit_entries(100)
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert_eq!(
        recorded
            .iter()
            .filter(|e| *e == "db.password.changed")
            .count(),
        2,
        "changing it and removing it are both changes: {recorded:?}"
    );
    let _ = plain.close();

    // And the copy taken before the first re-key is on disk, readable with the
    // password the register had then — none. That is the sentence the Settings card
    // puts on screen, so it had better be true.
    //
    // The count is deliberately not asserted. [`Store::backup_now`] names a backup
    // by the second it was taken in and treats an existing file of that name as
    // "already done" (`features/storage-sqlite-single-file.md` phase 6), so whether
    // three re-keys in a row leave one copy or three depends on how fast the
    // machine running the test is — under `llvm-cov` it is three, unmeasured it is
    // one. What is being tested is that the register was copied before it was
    // first rewritten, and the oldest copy is the one that answers that.
    let mut backups: Vec<std::path::PathBuf> = std::fs::read_dir(home.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().contains(".backup."))
        .collect();
    // The stamp is `%Y%m%d-%H%M%S`, so sorting the names sorts them by age.
    backups.sort();
    let oldest = backups.first().expect("a backup before the first re-key");
    let copy = Store::open_existing(&StoreConfig::new(oldest)).unwrap();
    assert_eq!(
        copy.keys().unwrap().len(),
        1,
        "the oldest backup is the register as it stood before the first re-key, and it \
         opens with the password it had then: none"
    );
    let _ = copy.close();
}

fn events(app: &YkDistApp) -> Vec<&str> {
    app.audit_view.iter().map(|e| e.event.as_str()).collect()
}
