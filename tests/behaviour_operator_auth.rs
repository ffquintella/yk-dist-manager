//! Behaviour: signing in, and what changes when somebody has
//! (`features/operator-auth-and-roles.md` phases 2, 3, 5, 6, 7 and 8).
//!
//! `tests/unit_store_operators.rs` proves the refusals exist below the screen.
//! What it cannot show is the part that only exists once a `YkDistApp` is
//! holding the store: that opening a register decides its authority before
//! anything is read through it, that the audit `actor` becomes the authenticated
//! identity rather than a workstation label, that the idle clock actually locks a
//! session and ends it, and that a sensitive operation is refused and then let
//! through by the credential prompt. Every one of those was a claim in the
//! feature file with nothing driving it.
//!
//! The promise these tests exist to hold on to is the first one: **a register
//! written before this release stays fully usable.** A control that locks an
//! operator out of their own register is a worse outcome than the control being
//! absent, so `scenario_a_register_migrated_to_v9_refuses_nothing` is the test
//! that must never be weakened.
//!
//! Every test here builds a `YkDistApp`, which reads `$YKDM_SETTINGS` and
//! `$YKDM_DATA_DIR` — see [`isolated_home`] for why every one of them must.
//! No test requires a key to be attached: the FIDO2 scenario drives
//! `device::write::MockWriter`, and no hardware is touched.

use std::path::Path;

use yk_dist_manager::YkDistApp;
use yk_dist_manager::device::write::MockWriter;
use yk_dist_manager::domain::{SerialSource, YubiKeyRecord};
use yk_dist_manager::operator::{Action, Authority, NewOperator, Role, session};
use yk_dist_manager::store::{Store, StoreConfig, StoreError};

const SERIAL: u32 = 20_423_631;
const PASSWORD: &str = "correct horse battery staple";
const OTHER_PASSWORD: &str = "seven yellow bicycles arrive";

/// A settings home this binary owns, redirected once for the whole process.
///
/// `YkDistApp` remembers the register it opens, so a test that builds one without
/// this writes into the settings file of whoever ran `cargo test`: the temp
/// databases end up in a real operator's recent list, and `last_database` points
/// the next launch at a path under `/var/folders` that no longer exists.
/// Forgetting the entries by hand does not help, because the next test run puts
/// them straight back. `tests/unit_settings.rs` fails the build if a test file
/// skips this.
///
/// One home for the whole binary, set inside the `OnceLock`: the variables are
/// process-global, so a per-test home would be a race between two threads over
/// which one the settings file follows.
fn isolated_home() -> &'static Path {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::tempdir().expect("a temporary home");
        // SAFETY: written exactly once per process, under the `OnceLock`, and
        // every test in this binary calls this before anything reads the
        // environment — a second caller blocks here until the write is done.
        unsafe {
            std::env::set_var("YKDM_DATA_DIR", home.path());
            std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
        }
        home
    })
    .path()
}

/// A register with one key in it and nobody enrolled — a register as it was
/// before this feature, which is what every existing file looks like after the
/// migration to v9.
fn unenrolled_register(path: &Path) {
    let store =
        Store::create_new(&StoreConfig::new(path).with_operator("felipe")).expect("a new register");
    store
        .upsert_key(&YubiKeyRecord::from_serial(SERIAL, SerialSource::Device))
        .expect("one key");
    let _ = store.close();
}

/// The same register with an administrator and a distributor in it.
fn enrolled_register(path: &Path) {
    unenrolled_register(path);
    let store = Store::open(&StoreConfig::new(path).with_operator("felipe")).expect("reopened");
    store
        .enrol_first_administrator("ana", "Ana Silva", PASSWORD, "felipe")
        .expect("the first administrator");
    store.act_as(Authority::SignedIn(Role::Administrator));
    store.mark_reverified(chrono::Utc::now());
    let bruno = store
        .enrol_operator(
            &NewOperator {
                username: "bruno".into(),
                display_name: "Bruno Costa".into(),
                role: Role::Distributor,
            },
            Some(OTHER_PASSWORD),
            "ana",
        )
        .expect("a distributor");
    assert_eq!(bruno.role, Role::Distributor);
    let _ = store.close();
}

fn entries(app: &YkDistApp, event: &str) -> Vec<String> {
    app.store
        .as_ref()
        .expect("a register is open")
        .audit_entries(500)
        .expect("the trail reads back")
        .into_iter()
        .filter(|entry| entry.event == event)
        .map(|entry| entry.details)
        .collect()
}

fn actors(app: &YkDistApp, event: &str) -> Vec<String> {
    app.store
        .as_ref()
        .expect("a register is open")
        .audit_entries(500)
        .expect("the trail reads back")
        .into_iter()
        .filter(|entry| entry.event == event)
        .map(|entry| entry.actor)
        .collect()
}

/// Everything ever written to the trail, as one string, for the assertion that
/// nothing typed reached it.
fn whole_trail(app: &YkDistApp) -> String {
    app.store
        .as_ref()
        .expect("a register is open")
        .audit_entries(500)
        .expect("the trail reads back")
        .into_iter()
        .map(|entry| {
            format!(
                "{} {} {} {}",
                entry.actor, entry.event, entry.target, entry.details
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ------------------------------------------------------------ the first run

/// The promise that makes this feature safe to release. An existing register is
/// migrated to v9 with an empty `operators` table, and must stay exactly as
/// usable as it was — nothing refused, and the actor honestly labelled.
#[test]
fn scenario_a_register_migrated_to_v9_refuses_nothing() {
    isolated_home();
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("unenrolled.sqlite3");

    // Given a register with no operators in it
    unenrolled_register(&database);

    // When it is opened
    let mut app = YkDistApp::new(Some(database.clone()));
    assert!(app.store.is_some(), "{:?}", app.db_form.error);

    // Then authorisation is off, and the screen has a name to say so with
    assert!(!app.is_enrolled());
    assert_eq!(app.session.authority(), Authority::Unenrolled);
    assert!(
        app.session.display().contains("not authenticated"),
        "the label has to say it is a label: {}",
        app.session.display()
    );
    assert!(
        !app.operator().is_empty() && app.operator() != "(not signed in)",
        "the actor is the workstation user, not a placeholder: {}",
        app.operator()
    );

    // And the store refuses nothing at all — including the four operations that
    // are not a table write and therefore have no authorizer behind them
    let store = app.store.as_ref().expect("open");
    for action in [
        Action::Read,
        Action::ManageInventory,
        Action::ManageTemplates,
        Action::ResetApplet,
        Action::ChangeDatabasePassword,
        Action::Export,
        Action::ManageOperators,
    ] {
        store.require(action).unwrap_or_else(|e| {
            panic!(
                "an unenrolled register refuses nothing, but {}: {e}",
                action.slug()
            )
        });
    }

    // And the ordinary work still works, with the workstation user as the actor
    let who = app.operator().to_owned();
    app.record("key.added", &format!("serial:{SERIAL}"), "by hand");
    assert_eq!(actors(&app, "key.added"), vec![who]);
}

/// Turning the control on is one deliberate act at the keyboard, and it is the
/// moment the trail records as the beginning of authorisation.
#[test]
fn scenario_the_first_administrator_switches_authorisation_on() {
    isolated_home();
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("first-run.sqlite3");
    unenrolled_register(&database);

    let mut app = YkDistApp::new(Some(database.clone()));
    assert!(!app.is_enrolled());

    // Given the first-run panel filled in
    app.operator_panel.first_username = "Ana".into();
    app.operator_panel.first_display_name = "Ana Silva".into();
    app.operator_panel.first_password = PASSWORD.into();
    app.operator_panel.first_password_again = PASSWORD.into();

    // When the administrator is created
    app.enrol_first_administrator();

    // Then it is audited as the moment authorisation began
    let enrolled = entries(&app, "operator.enrolled");
    assert_eq!(enrolled.len(), 1, "{enrolled:?}");
    assert!(enrolled[0].contains("first=true"), "{}", enrolled[0]);
    assert!(
        enrolled[0].contains("role=administrator"),
        "{}",
        enrolled[0]
    );

    // And the register now demands a sign-in rather than trusting the workstation
    assert!(app.is_enrolled());
    assert_eq!(app.session.authority(), Authority::SignedOut);
    assert_eq!(app.operator(), "(not signed in)");

    // And nothing typed reached the trail
    let trail = whole_trail(&app);
    assert!(!trail.contains(PASSWORD), "the password is on the trail");

    // And the panel kept nothing either
    assert!(app.operator_panel.first_password.is_empty());
    assert!(app.operator_panel.first_password_again.is_empty());

    // When the register is closed and opened again
    drop(app);
    let app = YkDistApp::new(Some(database.clone()));

    // Then it comes back signed out rather than unenrolled: the control is a
    // property of the file, not of this run of the application
    assert_eq!(app.session.authority(), Authority::SignedOut);
    assert!(app.is_enrolled());
}

// --------------------------------------------------------------- signing in

/// Phase 8's whole point: the actor stops being a label.
#[test]
fn scenario_signing_in_makes_the_audit_actor_the_authenticated_identity() {
    isolated_home();
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("sign-in.sqlite3");
    enrolled_register(&database);

    // Given an enrolled register, opened, with nobody signed in
    let mut app = YkDistApp::new(Some(database.clone()));
    assert_eq!(app.session.authority(), Authority::SignedOut);

    // When the distributor signs in
    app.sign_in.username = "  BRUNO  ".into();
    app.sign_in.password = OTHER_PASSWORD.into();
    app.sign_in_with_password();

    // Then the session carries their role, and the login is audited
    assert_eq!(
        app.session.authority(),
        Authority::SignedIn(Role::Distributor)
    );
    assert_eq!(app.operator(), "bruno");
    let logins = entries(&app, "operator.login");
    assert_eq!(logins.len(), 1, "{logins:?}");
    assert!(logins[0].contains("method=local"), "{}", logins[0]);

    // And the password is gone from the form the instant it was used
    assert!(app.sign_in.password.is_empty());

    // And from here on the actor on the trail is the authenticated identity
    app.record(
        "key.status.changed",
        &format!("serial:{SERIAL}"),
        "to issued",
    );
    assert_eq!(actors(&app, "key.status.changed"), vec!["bruno".to_owned()]);

    // When they sign out
    app.sign_out("explicit");

    // Then the logout is audited, the store stops trusting the session, and the
    // actor is a name no person owns — so an entry written between sign-ins
    // cannot be mistaken for somebody's work
    assert_eq!(entries(&app, "operator.logout").len(), 1);
    assert_eq!(app.session.authority(), Authority::SignedOut);
    assert_eq!(app.operator(), "(not signed in)");
    assert_eq!(
        app.store.as_ref().expect("open").authority(),
        Authority::SignedOut,
        "the store must not still be acting as the operator who left"
    );

    // And nothing anybody typed is anywhere on the trail
    let trail = whole_trail(&app);
    assert!(!trail.contains(OTHER_PASSWORD));
    assert!(!trail.contains(PASSWORD));
}

/// A wrong password is audited as a security event, counted, and eventually
/// locked out — and the entry says nothing about what was typed, not even its
/// length.
#[test]
fn scenario_wrong_passwords_are_audited_counted_and_locked_out() {
    isolated_home();
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("lockout.sqlite3");
    enrolled_register(&database);

    let mut app = YkDistApp::new(Some(database.clone()));

    // When three wrong passwords are typed for an operator who exists
    for _ in 0..3 {
        app.sign_in.username = "ana".into();
        app.sign_in.password = "not-the-password-1234".into();
        app.sign_in_with_password();
        assert_eq!(app.session.authority(), Authority::SignedOut);
    }

    // Then each failure is audited with its reason and its count
    let failures = entries(&app, "operator.login.failed");
    assert_eq!(failures.len(), 3, "{failures:?}");
    assert!(failures[0].contains("attempt=3"), "{}", failures[0]);
    assert!(
        failures.iter().any(|detail| detail.contains("lockout=60")),
        "the third failure locks for a minute: {failures:?}"
    );

    // And nothing typed is in any of them
    for detail in &failures {
        assert!(!detail.contains("not-the-password"), "{detail}");
        assert!(!detail.contains("length"), "not even a length: {detail}");
    }

    // And now even the right password is refused, because the account is locked
    app.sign_in.username = "ana".into();
    app.sign_in.password = PASSWORD.into();
    app.sign_in_with_password();
    assert_eq!(app.session.authority(), Authority::SignedOut);
    // And the refusal says how long to wait rather than whether the account
    // exists — the count and the wait are safe to state, the username is not
    let locked = app.sign_in.error.clone().expect("refused");
    assert!(
        locked.contains("consecutive failed sign-ins") && locked.contains("locked for another"),
        "{locked}"
    );
    assert!(
        !locked.contains("ana"),
        "a lockout must not confirm the username: {locked}"
    );

    // And an unknown username is refused in exactly the same words, so the
    // screen never answers "is there an operator called carla"
    app.sign_in.username = "carla".into();
    app.sign_in.password = "whatever-1234-abcd".into();
    app.sign_in_with_password();
    let unknown = app.sign_in.error.clone().expect("refused");
    app.sign_in.username = "bruno".into();
    app.sign_in.password = "wrong-1234-abcd-efgh".into();
    app.sign_in_with_password();
    assert_eq!(
        unknown,
        app.sign_in.error.clone().expect("refused"),
        "an unknown username and a wrong password must read the same"
    );

    let trail = whole_trail(&app);
    assert!(!trail.contains("not-the-password"));
    assert!(!trail.contains(PASSWORD));
}

/// Phase 3, with no key attached and none required: the assertion goes through
/// `MockWriter`, which is the same seam every other hardware behaviour test in
/// this repository uses.
#[test]
fn scenario_an_operator_signs_in_with_their_own_security_key() {
    isolated_home();
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("fido2-sign-in.sqlite3");
    enrolled_register(&database);

    let mut app = YkDistApp::new(Some(database.clone()));
    app.sign_in.username = "ana".into();
    app.sign_in.password = PASSWORD.into();
    app.sign_in_with_password();
    assert_eq!(
        app.session.authority(),
        Authority::SignedIn(Role::Administrator)
    );

    // Given the administrator registers a key for the distributor
    let bruno = app
        .operator_panel
        .operators
        .iter()
        .find(|operator| operator.username == "bruno")
        .expect("the distributor is on the list")
        .id;
    let mut writer = MockWriter::factory_fresh(SERIAL);
    app.operator_panel.key_pin = "471102".into();
    app.register_key_for_operator(bruno, SERIAL, &mut writer);

    // Then the registration is audited as a credential change, naming the method
    // and the key but never the PIN
    let changed = entries(&app, "operator.credential.changed");
    let fido2: Vec<&String> = changed
        .iter()
        .filter(|detail| detail.contains("method=fido2"))
        .collect();
    assert_eq!(fido2.len(), 1, "{changed:?}");
    assert!(
        fido2[0].contains(&format!("serial={SERIAL}")),
        "{}",
        fido2[0]
    );
    assert!(
        !whole_trail(&app).contains("471102"),
        "the PIN is on the trail"
    );
    assert!(
        app.operator_panel.key_pin.is_empty(),
        "the PIN is still in the form"
    );

    // And the register now knows which key answers for them
    app.refresh_operators();
    let credential = app
        .operator_panel
        .operators
        .iter()
        .find(|operator| operator.username == "bruno")
        .and_then(|operator| operator.credential.clone())
        .expect("a registered credential");
    assert_eq!(credential.serial, SERIAL);
    assert_eq!(credential.relying_party, app.org);

    // When the distributor signs in with that key
    app.sign_out("explicit");
    app.sign_in.username = "bruno".into();
    app.sign_in.password = "471102".into();
    app.sign_in_with_key(&mut writer);

    // Then they are in, with the method recorded as the key rather than a
    // password
    assert_eq!(
        app.session.authority(),
        Authority::SignedIn(Role::Distributor),
        "{:?}",
        app.sign_in.error
    );
    let logins = entries(&app, "operator.login");
    assert!(
        logins.iter().any(|detail| detail.contains("method=fido2")),
        "{logins:?}"
    );
    assert!(app.sign_in.password.is_empty());
    assert!(!whole_trail(&app).contains("471102"));

    // And a key whose signature counter does not advance is refused — the classic
    // sign of a cloned authenticator. `MockWriter::factory_fresh` starts its
    // counter over, which is exactly that shape.
    app.sign_out("explicit");
    let mut replayed = MockWriter::factory_fresh(SERIAL);
    app.sign_in.username = "bruno".into();
    app.sign_in.password = "471102".into();
    app.sign_in_with_key(&mut replayed);
    assert_eq!(
        app.session.authority(),
        Authority::SignedOut,
        "a counter that went backwards must not sign anybody in"
    );
    assert!(
        entries(&app, "operator.login.failed")
            .iter()
            .any(|detail| detail.contains("counter-replay")),
        "{:?}",
        entries(&app, "operator.login.failed")
    );
}

// ---------------------------------------------------- the session's own clock

/// Phase 6. The thresholds were a constant nothing read until the frame loop
/// called the clock; this is the test that would have caught that.
#[test]
fn scenario_a_session_locks_when_idle_and_ends_when_abandoned() {
    isolated_home();
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("idle.sqlite3");
    enrolled_register(&database);

    let mut app = YkDistApp::new(Some(database.clone()));
    app.sign_in.username = "bruno".into();
    app.sign_in.password = OTHER_PASSWORD.into();
    app.sign_in_with_password();
    let signed_in_at = chrono::Utc::now();
    assert_eq!(
        app.session.authority(),
        Authority::SignedIn(Role::Distributor)
    );

    // Given a minute of work: the clock is advanced, but so is the activity
    let a_minute_later = signed_in_at + chrono::Duration::seconds(60);
    app.touch_session(a_minute_later);
    app.tick_session(a_minute_later);
    assert_eq!(
        app.session.authority(),
        Authority::SignedIn(Role::Distributor),
        "somebody who is working must not be locked out"
    );

    // When the workstation is left alone for longer than the lock threshold
    let idle_at = a_minute_later
        + chrono::Duration::from_std(session::LOCK_AFTER).unwrap()
        + chrono::Duration::seconds(1);
    app.tick_session(idle_at);

    // Then the session is locked: the store stops trusting it at the same instant
    // the screen does, and nothing on the desk was lost
    assert!(
        app.session.session().is_some_and(|s| s.is_locked()),
        "the session should be locked"
    );
    assert_eq!(app.session.authority(), Authority::SignedOut);
    assert_eq!(
        app.store.as_ref().expect("open").authority(),
        Authority::SignedOut,
        "a locked session must not still be able to write"
    );
    let locks = entries(&app, "operator.session.locked");
    assert_eq!(locks.len(), 1, "{locks:?}");
    assert!(locks[0].contains("reason=idle"), "{}", locks[0]);

    // And the same person signing in again carries on
    app.sign_in.username = "bruno".into();
    app.sign_in.password = OTHER_PASSWORD.into();
    app.sign_in_with_password();
    assert_eq!(
        app.session.authority(),
        Authority::SignedIn(Role::Distributor),
        "{:?}",
        app.sign_in.error
    );

    // When the workstation is abandoned for longer than the timeout
    let gone_home = chrono::Utc::now()
        + chrono::Duration::from_std(session::TIMEOUT_AFTER).unwrap()
        + chrono::Duration::seconds(1);
    app.tick_session(gone_home);

    // Then the session ends outright, and says so on the trail — a session that
    // quietly stopped existing is indistinguishable from one nobody closed
    assert_eq!(app.session.authority(), Authority::SignedOut);
    assert!(app.session.session().is_none());
    assert!(
        entries(&app, "operator.logout")
            .iter()
            .any(|detail| detail.contains("timeout")),
        "{:?}",
        entries(&app, "operator.logout")
    );
}

// ---------------------------------------------- refusals and re-verification

/// The refusal is the security event, and the store is what produces it. A
/// distributor asking for a template edit gets refused whatever the screen shows.
#[test]
fn scenario_a_distributor_is_refused_a_template_edit_and_the_refusal_is_recorded() {
    isolated_home();
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("refusal.sqlite3");
    enrolled_register(&database);

    let mut app = YkDistApp::new(Some(database.clone()));
    app.sign_in.username = "bruno".into();
    app.sign_in.password = OTHER_PASSWORD.into();
    app.sign_in_with_password();

    // Given a distributor, signed in, with a live re-verification from the
    // sign-in itself — so the only thing left to refuse is the role
    let store = app.store.as_ref().expect("open");
    assert_eq!(store.authority(), Authority::SignedIn(Role::Distributor));

    // When they try to store a procedure through the store the screen uses
    let refused =
        store.upsert_template(&yk_dist_manager::template::BootstrapTemplate::org_standard());

    // Then the database itself refuses it
    assert!(
        matches!(refused, Err(StoreError::NotAuthorised)),
        "{refused:?}"
    );

    // And the same is true of managing operators, which is not a table write the
    // authorizer can catch on its own
    let refused = store.require(Action::ManageOperators);
    let Err(StoreError::Forbidden { who, what }) = refused else {
        panic!("a distributor may not manage operators, got {refused:?}");
    };
    // Authorisation is by role, never per user — so the refusal names the role
    // and not the person, and neither the message nor the trail identifies them
    assert_eq!(who, "Distributor");
    assert!(what.contains("operators"), "{what}");

    // And when the screen offers a button it should have hidden, the refusal
    // reaches the trail as a security event in its own right
    app.change_operator_role(
        app.operator_panel
            .operators
            .iter()
            .find(|operator| operator.username == "ana")
            .expect("the administrator is on the list")
            .id,
        Role::Auditor,
    );
    let refusals = entries(&app, "operator.authorisation.refused");
    assert_eq!(refusals.len(), 1, "{refusals:?}");
    assert!(
        refusals[0].contains("action=manage-operators"),
        "{}",
        refusals[0]
    );
    assert!(refusals[0].contains("reason=role"), "{}", refusals[0]);

    // And the role did not change
    app.refresh_operators();
    assert_eq!(
        app.operator_panel
            .operators
            .iter()
            .find(|operator| operator.username == "ana")
            .map(|operator| operator.role),
        Some(Role::Administrator)
    );
}

/// An operator who forgot their password is otherwise shut out of a register
/// that has no other way back in — and the person who lets them back in must
/// not be able to do it quietly.
#[test]
fn scenario_an_administrator_gives_an_operator_a_new_password() {
    isolated_home();
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("reset-password.sqlite3");
    enrolled_register(&database);

    // Given a distributor who has locked themselves out
    let mut app = YkDistApp::new(Some(database.clone()));
    for _ in 0..3 {
        app.sign_in.username = "bruno".into();
        app.sign_in.password = "not the one".into();
        app.sign_in_with_password();
    }
    assert_eq!(app.session.authority(), Authority::SignedOut);

    // And an administrator at the keyboard
    app.sign_in.username = "ana".into();
    app.sign_in.password = PASSWORD.into();
    app.sign_in_with_password();
    assert_eq!(
        app.session.authority(),
        Authority::SignedIn(Role::Administrator)
    );
    app.refresh_operators();
    let bruno = app
        .operator_panel
        .operators
        .iter()
        .find(|operator| operator.username == "bruno")
        .expect("bruno is on the list")
        .id;

    // When they choose a new password for them
    const REPLACEMENT: &str = "eleven quiet lanterns in march";
    app.operator_panel.resetting = Some(bruno);
    app.operator_panel.reset_password = REPLACEMENT.into();
    app.operator_panel.reset_password_again = REPLACEMENT.into();
    assert!(app.require_reverification(Action::ManageOperators));
    app.set_operator_password(bruno);

    // Then the change is on the trail, naming who made it and by what method,
    // the typed password is gone from the form, and the panel has closed
    let changed = entries(&app, "operator.credential.changed");
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert!(changed[0].contains("operator=bruno"), "{}", changed[0]);
    assert!(changed[0].contains("method=local"), "{}", changed[0]);
    assert!(changed[0].contains("by=ana"), "{}", changed[0]);
    assert!(app.operator_panel.reset_password.is_empty());
    assert!(app.operator_panel.resetting.is_none());

    // And bruno can sign in with it, lockout and all, while the old password
    // cannot
    app.sign_out("explicit");
    app.sign_in.username = "bruno".into();
    app.sign_in.password = OTHER_PASSWORD.into();
    app.sign_in_with_password();
    assert_eq!(app.session.authority(), Authority::SignedOut);
    app.sign_in.username = "bruno".into();
    app.sign_in.password = REPLACEMENT.into();
    app.sign_in_with_password();
    assert_eq!(
        app.session.authority(),
        Authority::SignedIn(Role::Distributor)
    );

    // And nothing anybody typed is anywhere on the trail
    let trail = whole_trail(&app);
    assert!(!trail.contains(REPLACEMENT));
    assert!(!trail.contains(OTHER_PASSWORD));
    assert!(!trail.contains(PASSWORD));
}

/// The two halves of removing somebody, in one register: an account enrolled by
/// mistake goes, and an account with history stays and is disabled instead.
#[test]
fn scenario_an_operator_enrolled_by_mistake_is_removed_and_one_with_history_is_not() {
    isolated_home();
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("remove-operator.sqlite3");
    enrolled_register(&database);

    // Given an administrator who has also enrolled somebody by mistake
    let mut app = YkDistApp::new(Some(database.clone()));
    app.sign_in.username = "ana".into();
    app.sign_in.password = PASSWORD.into();
    app.sign_in_with_password();
    app.operator_panel.new_username = "brunno".into();
    app.operator_panel.new_display_name = "Bruno Costa".into();
    app.operator_panel.new_role = Role::Distributor;
    app.operator_panel.new_password = OTHER_PASSWORD.into();
    app.operator_panel.new_password_again = OTHER_PASSWORD.into();
    assert!(app.require_reverification(Action::ManageOperators));
    app.enrol_operator();
    let id_of = |app: &YkDistApp, username: &str| {
        app.operator_panel
            .operators
            .iter()
            .find(|operator| operator.username == username)
            .map(|operator| operator.id)
    };
    let typo = id_of(&app, "brunno").expect("the mistyped account exists");

    // When it is removed
    app.operator_panel.removing = Some(typo);
    app.remove_operator(typo);

    // Then it is gone from the list, the removal is on the trail, and the panel
    // has closed
    assert!(
        id_of(&app, "brunno").is_none(),
        "the account is still there"
    );
    let removed = entries(&app, "operator.removed");
    assert_eq!(removed.len(), 1, "{removed:?}");
    assert!(removed[0].contains("operator=brunno"), "{}", removed[0]);
    assert!(removed[0].contains("by=ana"), "{}", removed[0]);
    assert!(app.operator_panel.removing.is_none());

    // And when the operator who has actually used this register is removed
    let bruno = id_of(&app, "bruno").expect("bruno is on the list");
    app.sign_out("explicit");
    app.sign_in.username = "bruno".into();
    app.sign_in.password = OTHER_PASSWORD.into();
    app.sign_in_with_password();
    app.sign_out("explicit");
    app.sign_in.username = "ana".into();
    app.sign_in.password = PASSWORD.into();
    app.sign_in_with_password();
    app.refresh_operators();
    app.remove_operator(bruno);

    // Then it is refused, the account is still there, and the refusal is on the
    // trail like every other one
    assert!(id_of(&app, "bruno").is_some(), "bruno was deleted");
    assert_eq!(entries(&app, "operator.removed").len(), 1);
    let refusals = entries(&app, "operator.authorisation.refused");
    assert!(!refusals.is_empty(), "the refusal was not recorded");

    // And disabling them works, which is what the screen says to do instead
    app.set_operator_active(bruno, false);
    assert_eq!(entries(&app, "operator.disabled").len(), 1);
}

/// Phase 5, end to end through the application: an export is refused once the
/// sign-in's own re-verification has lapsed, the prompt opens, and presenting
/// the credential again lets it through.
#[test]
fn scenario_an_export_asks_for_the_credential_again_and_then_goes_through() {
    isolated_home();
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("reverify.sqlite3");
    let out = dir.path().join("report.csv");
    enrolled_register(&database);

    let mut app = YkDistApp::new(Some(database.clone()));
    app.sign_in.username = "ana".into();
    app.sign_in.password = PASSWORD.into();
    app.sign_in_with_password();

    // Given a report on screen, and a session whose re-verification has lapsed —
    // which is what `act_as` models: a change of who is at the keyboard discards
    // one, and so does time
    app.generate_report();
    assert!(app.reports.current.is_some(), "{:?}", app.reports.error);
    app.store
        .as_ref()
        .expect("open")
        .act_as(Authority::SignedIn(Role::Administrator));
    if let Some(session) = app.session.session_mut() {
        session.reverified_at = None;
    }

    // When the export is asked for
    let written = app.write_report(&out);

    // Then nothing was written, and the operator is asked for the credential on
    // the screen that has the prompt rather than left with a status line
    assert!(!written, "a refused export must not write the file");
    assert!(!out.exists(), "the file was written anyway");
    assert_eq!(app.sign_in.reverifying, Some(Action::Export));
    assert_eq!(app.tab, yk_dist_manager::app::Tab::Operators);

    // And the refusal is on the trail, as a re-verification rather than a role
    // problem — the two call for different answers
    let refusals = entries(&app, "operator.authorisation.refused");
    assert_eq!(refusals.len(), 1, "{refusals:?}");
    assert!(
        refusals[0].contains("reason=reverification"),
        "{}",
        refusals[0]
    );
    assert!(refusals[0].contains("action=export"), "{}", refusals[0]);

    // When the credential is presented again
    app.sign_in.password = PASSWORD.into();
    app.complete_reverification();

    // Then it is audited, nothing typed is on the trail, and the export goes
    // through
    let reverified = entries(&app, "operator.reverified");
    assert_eq!(reverified.len(), 1, "{reverified:?}");
    assert!(reverified[0].contains("action=export"), "{}", reverified[0]);
    assert!(app.sign_in.reverifying.is_none());
    assert!(app.sign_in.password.is_empty());
    assert!(!whole_trail(&app).contains(PASSWORD));

    assert!(app.write_report(&out), "{:?}", app.reports.error);
    assert!(out.exists(), "the export should have been written");
    assert!(!entries(&app, "export.taken").is_empty());
}
