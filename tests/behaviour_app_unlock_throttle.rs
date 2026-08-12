//! Behaviour test for the unlock throttle as the *application* applies it
//! (`features/db-password-and-encryption.md` phase 3).
//!
//! [`yk_dist_manager::password::Throttle`] is unit-tested where it lives; what is
//! tested here is the wiring, which is where the interesting mistakes are: which
//! failures count, which do not, and whether a wait is actually enforced rather
//! than merely displayed on a disabled button.
//!
//! **One test per configuration, deliberately.** These drive `YkDistApp`, which
//! reads `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is
//! shared by every test in a binary. Exactly one of the two tests below is
//! compiled in any given build, so nothing here races, and pointing both variables
//! at a temporary directory means the operator's real settings file is never
//! touched by a test run.

use std::path::Path;

/// Redirect everything this test would otherwise write to the operator's home.
fn scratch(home: &Path) {
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home);
        std::env::set_var("YKDM_SETTINGS", home.join("settings.json"));
    }
}

/// A passphrase that meets the policy. Not a credential: it protects nothing and
/// the database it opens is deleted when the test ends.
#[cfg(feature = "encrypted-db")]
const A_GOOD_PASSPHRASE: &str = "correct horse battery staple";

#[cfg(feature = "encrypted-db")]
#[test]
fn scenario_wrong_passwords_earn_a_wait_that_is_enforced_and_a_right_one_clears_it() {
    use std::time::Duration;

    use yk_dist_manager::YkDistApp;
    use yk_dist_manager::app::DbRequest;
    use yk_dist_manager::password::FREE_ATTEMPTS;
    use yk_dist_manager::store::{Store, StoreConfig};

    let home = tempfile::tempdir().unwrap();
    scratch(home.path());

    // Given an encrypted register
    let database = home.path().join("keys.sqlite3");
    {
        let store = Store::create_new(
            &StoreConfig::new(&database).with_password(Some(A_GOOD_PASSPHRASE.into())),
        )
        .unwrap();
        let _ = store.close();
    }

    // When the application starts on it, it probes without a password first
    let mut app = YkDistApp::new(Some(database.clone()));

    // Then the register is locked — and that probe is *not* a failed attempt.
    // Counting it would have every session with an encrypted register start one
    // failure down, for something no operator typed.
    assert!(app.store.is_none(), "an encrypted register must not open");
    assert_eq!(
        app.throttle.failures(),
        0,
        "the startup probe is the application's question, not a guess"
    );

    // When the operator gets it wrong as many times as the policy allows for free
    let wrong = |app: &mut YkDistApp| {
        app.db_form.password = "not the passphrase".into();
        app.db_request = Some(DbRequest::Open(database.clone()));
        app.handle_db_request();
    };
    for _ in 0..FREE_ATTEMPTS {
        wrong(&mut app);
    }

    // Then nothing has been slowed down yet: an operator mistyping is not an
    // attack, and the first thing a delay would achieve is annoying them.
    assert_eq!(app.throttle.failures(), FREE_ATTEMPTS);
    assert!(!app.throttle.must_wait(), "the free attempts are free");

    // When they get it wrong once more
    wrong(&mut app);

    // Then a wait is owed, and it says so without implying a lockout — there is
    // no administrator to lift one on a shared register
    assert!(app.throttle.must_wait());
    let shown = app.db_form.error.clone().expect("a refusal on screen");
    assert!(shown.contains("wait"), "{shown}");
    assert!(!shown.to_lowercase().contains("locked"), "{shown}");
    assert!(!shown.to_lowercase().contains("remaining"), "{shown}");

    // And the wait is *enforced*, not merely painted: the correct passphrase,
    // submitted while it runs, is refused without touching the file — otherwise
    // the throttle would be a property of the paint pass that any other caller
    // could route around.
    app.db_form.password = A_GOOD_PASSPHRASE.into();
    app.db_request = Some(DbRequest::Open(database.clone()));
    app.handle_db_request();
    assert!(
        app.store.is_none(),
        "an attempt made during the wait must not be tried"
    );
    assert_eq!(
        app.db_form.password, A_GOOD_PASSPHRASE,
        "a refusal must not eat the typed password: the operator submits it again"
    );

    // When the wait has run out
    std::thread::sleep(Duration::from_millis(1_200));
    assert!(!app.throttle.must_wait(), "the delay was one second");
    app.db_request = Some(DbRequest::Open(database.clone()));
    app.handle_db_request();

    // Then the register opens, the history is cleared — the next mistyped
    // password gets its free attempts back — and the unlock is on the record
    assert!(app.store.is_some(), "{:?}", app.db_form.error);
    assert_eq!(app.throttle.failures(), 0);
    assert!(
        app.db_form.password.is_empty(),
        "the field is cleared on use"
    );
    assert!(
        app.audit_view
            .iter()
            .any(|entry| entry.event == "db.unlocked"),
        "opening an encrypted register is a state change worth recording: {:?}",
        app.audit_view
            .iter()
            .map(|e| e.event.as_str())
            .collect::<Vec<_>>()
    );
}

#[cfg(not(feature = "encrypted-db"))]
#[test]
fn scenario_a_build_that_cannot_encrypt_is_not_a_wrong_password() {
    // The throttle must count wrong passwords and nothing else. This build refuses
    // a password because it has no SQLCipher, which is a rebuild rather than a
    // guess — slowing the prompt down would punish an operator for a decision
    // somebody made at compile time, and would do it however many times they try.
    use yk_dist_manager::YkDistApp;
    use yk_dist_manager::app::DbRequest;
    use yk_dist_manager::store::{Store, StoreConfig};

    let home = tempfile::tempdir().unwrap();
    scratch(home.path());

    let database = home.path().join("keys.sqlite3");
    {
        let store = Store::create_new(&StoreConfig::new(&database)).unwrap();
        let _ = store.close();
    }

    let mut app = YkDistApp::new(Some(database.clone()));
    assert!(app.store.is_some(), "a plain register needs no password");
    app.db_request = Some(DbRequest::Close);
    app.handle_db_request();

    for _ in 0..6 {
        app.db_form.password = "correct horse battery staple".into();
        app.db_request = Some(DbRequest::Open(database.clone()));
        app.handle_db_request();
    }

    assert_eq!(app.throttle.failures(), 0, "not a wrong password");
    assert!(!app.throttle.must_wait());
    let shown = app.db_form.error.clone().expect("a refusal on screen");
    assert!(
        shown.contains("encrypted-db"),
        "the refusal must name the feature to rebuild with: {shown}"
    );
}
