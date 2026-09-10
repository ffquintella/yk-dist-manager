//! Authorisation as the **store** enforces it
//! (`features/operator-auth-and-roles.md` phase 1).
//!
//! The specification asks for the refusal to exist below the screen, so that a UI
//! bug cannot bypass it. These tests never build a `YkDistApp`: they take a
//! `Store`, tell it which role it is acting as, and try the write directly. If a
//! refusal here can be got round by calling the method, it can be got round by a
//! button that should have been hidden.
//!
//! Two different refusals are asserted, because there are two layers:
//!
//! * `StoreError::NotAuthorised` — SQLite's own, raised while the statement was
//!   being prepared, which is what covers a mutation nobody wrote a check for.
//! * `StoreError::Forbidden` / `NeedsReverification` — the explicit check, for
//!   what is not a table write: a factory reset, a re-keyed file, an export.

use chrono::{Duration, Utc};
use yk_dist_manager::domain::{SerialSource, YubiKeyRecord};
use yk_dist_manager::operator::{Action, Authority, NewOperator, Role};
use yk_dist_manager::store::{Store, StoreError};

const PASSWORD: &str = "correct horse battery staple";

fn register() -> Store {
    Store::open_in_memory().expect("an in-memory register")
}

fn key(serial: u32) -> YubiKeyRecord {
    YubiKeyRecord::from_serial(serial, SerialSource::Device)
}

/// A register with an administrator in it, acting as that administrator with a
/// live re-verification — the state most of these tests start from.
fn enrolled() -> Store {
    let store = register();
    store
        .enrol_first_administrator("ana", "Ana Silva", PASSWORD, "felipe")
        .expect("the first administrator is created");
    store.act_as(Authority::SignedIn(Role::Administrator));
    store.mark_reverified(Utc::now());
    store
}

// ------------------------------------------------------------ the first run

/// The promise that makes this feature safe to ship: a register written before
/// it has no operators, so it refuses nothing and opens exactly as it did.
#[test]
fn a_register_with_no_operators_refuses_nothing() {
    // Given a register nobody has enrolled into
    let store = register();

    // When it is asked what authority to open under
    let authority = store.opening_authority().expect("the count reads back");

    // Then it is unenrolled, and every write still works
    assert_eq!(authority, Authority::Unenrolled);
    assert_eq!(store.operator_count().unwrap(), 0);
    store.upsert_key(&key(20_423_631)).expect("a key is added");
    store
        .require(Action::ManageTemplates)
        .expect("a procedure can still be edited");
    store
        .require(Action::ResetApplet)
        .expect("an applet can still be reset");
}

#[test]
fn the_first_administrator_is_created_once_and_then_never_again() {
    // Given an unenrolled register
    let store = register();

    // When the first administrator is created
    let ana = store
        .enrol_first_administrator("Ana", "Ana Silva", PASSWORD, "felipe")
        .expect("the first administrator is created");

    // Then the account exists, lower-cased, as an administrator
    assert_eq!(ana.username, "ana");
    assert_eq!(ana.role, Role::Administrator);
    assert!(ana.has_password);
    assert!(ana.can_sign_in());

    // And the register now expects somebody to sign in
    assert_eq!(
        store.opening_authority().unwrap(),
        Authority::SignedOut,
        "one operator is enough to switch authorisation on"
    );

    // And the first-run path is closed, so it cannot be a way round the
    // administrator's role
    let second = store.enrol_first_administrator("bruno", "Bruno Costa", PASSWORD, "felipe");
    assert!(
        matches!(second, Err(StoreError::Forbidden { .. })),
        "{second:?}"
    );
}

#[test]
fn creating_the_first_administrator_is_audited_as_the_moment_authorisation_began() {
    let store = register();
    store
        .enrol_first_administrator("ana", "Ana Silva", PASSWORD, "felipe")
        .unwrap();

    let entry = store
        .audit_entries(20)
        .unwrap()
        .into_iter()
        .find(|entry| entry.event == "operator.enrolled")
        .expect("the enrolment is on the trail");
    assert_eq!(entry.target, "operator:ana");
    assert!(entry.details.contains("role=administrator"));
    assert!(entry.details.contains("first=true"));
    assert!(!entry.details.contains(PASSWORD));
}

// ----------------------------------------------------- the role matrix, below the UI

/// The specification's test, at the layer it names: a distributor cannot edit a
/// template, and the refusal comes from the database rather than from a check.
#[test]
fn a_distributor_cannot_write_a_template_and_sqlite_is_what_refuses() {
    // Given a register whose session is a distributor
    let store = enrolled();
    let template = yk_dist_manager::template::BootstrapTemplate::org_standard();
    store.act_as(Authority::SignedIn(Role::Distributor));

    // When they try to store a procedure
    let refused = store.upsert_template(&template);

    // Then the statement never ran
    assert!(
        matches!(refused, Err(StoreError::NotAuthorised)),
        "{refused:?}"
    );

    // And the work a distributor *is* for still goes through
    store
        .upsert_key(&key(20_423_632))
        .expect("a distributor keeps the inventory");
}

#[test]
fn an_auditor_cannot_write_anything_operational() {
    // Given a register whose session is an auditor
    let store = enrolled();
    store.act_as(Authority::SignedIn(Role::Auditor));

    // When they try to change the register
    let key_refused = store.upsert_key(&key(20_423_633));
    let template_refused =
        store.upsert_template(&yk_dist_manager::template::BootstrapTemplate::org_standard());

    // Then both are refused by the database itself
    assert!(
        matches!(key_refused, Err(StoreError::NotAuthorised)),
        "{key_refused:?}"
    );
    assert!(
        matches!(template_refused, Err(StoreError::NotAuthorised)),
        "{template_refused:?}"
    );

    // And reading, which is what an auditor is for, is untouched
    store.keys().expect("an auditor reads everything");
    store.audit_entries(10).expect("and verifies the chain");
}

/// An auditor's own sign-in has to be recordable, or the norm's "login always
/// audited" cannot hold for the one role that only ever reads.
#[test]
fn an_auditor_may_still_append_to_the_audit_trail() {
    let store = enrolled();
    store.act_as(Authority::SignedIn(Role::Auditor));
    store
        .append_audit("bruno", "operator.login", "operator:bruno", "method=local")
        .expect("an auditor's own login is recordable");
}

#[test]
fn the_operations_that_are_not_a_table_write_are_refused_by_the_explicit_check() {
    let store = enrolled();
    store.act_as(Authority::SignedIn(Role::Distributor));

    // A factory reset writes to hardware, not to a table, so no authorizer could
    // catch it.
    for action in [
        Action::ResetApplet,
        Action::ChangeDatabasePassword,
        Action::ManageOperators,
        Action::ChangeSecuritySettings,
        Action::ManageTemplates,
    ] {
        let refused = store.require(action);
        assert!(
            matches!(refused, Err(StoreError::Forbidden { .. })),
            "{} should be refused to a distributor, got {refused:?}",
            action.slug()
        );
    }

    // And the ones a distributor is for are not.
    store.mark_reverified(Utc::now());
    for action in [
        Action::Read,
        Action::RunBootstrap,
        Action::RecordDistribution,
        Action::ManageHolders,
        Action::ManageInventory,
        Action::Export,
    ] {
        store
            .require(action)
            .unwrap_or_else(|e| panic!("a distributor may {}: {e}", action.slug()));
    }
}

#[test]
fn a_refusal_names_the_role_and_the_action_without_naming_a_person() {
    let store = enrolled();
    store.act_as(Authority::SignedIn(Role::Auditor));
    let Err(StoreError::Forbidden { who, what }) = store.require(Action::ResetApplet) else {
        panic!("an auditor may not reset an applet");
    };
    // Authorisation is by role, never per user — so the refusal names the role.
    assert_eq!(who, "Auditor");
    assert!(what.contains("factory-reset"));
}

// ------------------------------------------------------------ re-verification

#[test]
fn a_sensitive_operation_is_refused_without_a_fresh_re_verification() {
    // Given an administrator who signed in a long time ago
    let store = enrolled();
    store.act_as(Authority::SignedIn(Role::Administrator));

    // When they ask to edit a procedure with no re-verification behind them
    let refused = store.require(Action::ManageTemplates);

    // Then it is refused, and the message says why rather than "forbidden"
    let Err(StoreError::NeedsReverification { what }) = refused else {
        panic!("expected a re-verification refusal, got {refused:?}");
    };
    assert!(what.contains("procedure"));

    // And the ordinary work is not gated on it
    store
        .require(Action::RunBootstrap)
        .expect("a run is not sensitive");

    // When the credential is presented again
    store.mark_reverified(Utc::now());

    // Then the same operation goes through
    store.require(Action::ManageTemplates).expect("re-verified");
}

#[test]
fn a_re_verification_expires_and_a_change_of_authority_discards_it() {
    let store = enrolled();
    store.act_as(Authority::SignedIn(Role::Administrator));

    store.mark_reverified(Utc::now() - Duration::minutes(5));
    assert!(matches!(
        store.require(Action::ManageOperators),
        Err(StoreError::NeedsReverification { .. })
    ));

    // Signing somebody else in must never carry the last person's verification.
    store.mark_reverified(Utc::now());
    store.act_as(Authority::SignedIn(Role::Administrator));
    assert!(matches!(
        store.require(Action::ManageOperators),
        Err(StoreError::NeedsReverification { .. })
    ));
}

// ---------------------------------------------------------------- enrolment

#[test]
fn only_an_administrator_may_enrol_and_the_enrolment_is_audited() {
    let store = enrolled();

    let bruno = store
        .enrol_operator(
            &NewOperator {
                username: "bruno".into(),
                display_name: "Bruno Costa".into(),
                role: Role::Distributor,
            },
            Some(PASSWORD),
            "ana",
        )
        .expect("an administrator enrols");
    assert_eq!(bruno.role, Role::Distributor);

    // A distributor cannot, and is refused before the row is prepared.
    store.act_as(Authority::SignedIn(Role::Distributor));
    store.mark_reverified(Utc::now());
    let refused = store.enrol_operator(
        &NewOperator {
            username: "carla".into(),
            display_name: "Carla Dias".into(),
            role: Role::Administrator,
        },
        Some(PASSWORD),
        "bruno",
    );
    assert!(
        matches!(refused, Err(StoreError::Forbidden { .. })),
        "{refused:?}"
    );
    assert_eq!(store.operator_count().unwrap(), 2);
}

#[test]
fn a_role_change_is_audited_with_both_roles_and_who_made_it() {
    let store = enrolled();
    let bruno = store
        .enrol_operator(
            &NewOperator {
                username: "bruno".into(),
                display_name: "Bruno Costa".into(),
                role: Role::Distributor,
            },
            Some(PASSWORD),
            "ana",
        )
        .unwrap();

    store
        .set_operator_role(bruno.id, Role::Auditor, "ana")
        .unwrap();

    let entry = store
        .audit_entries(40)
        .unwrap()
        .into_iter()
        .find(|entry| entry.event == "operator.role.changed")
        .expect("the change is on the trail");
    assert!(entry.details.contains("operator=bruno"));
    assert!(entry.details.contains("from=distributor"));
    assert!(entry.details.contains("to=auditor"));
    assert!(entry.details.contains("by=ana"));
}

/// A register that lost its last administrator is a register nobody can enrol
/// into, change a procedure in, or reset a key with — the lockout this feature
/// exists not to cause, by a longer route.
#[test]
fn the_last_administrator_cannot_be_demoted_or_disabled() {
    let store = enrolled();
    let ana = store.operator_by_username("ana").unwrap().unwrap();

    let demoted = store.set_operator_role(ana.id, Role::Distributor, "ana");
    assert!(
        matches!(demoted, Err(StoreError::Forbidden { .. })),
        "{demoted:?}"
    );
    let disabled = store.set_operator_active(ana.id, false, "ana");
    assert!(
        matches!(disabled, Err(StoreError::Forbidden { .. })),
        "{disabled:?}"
    );

    // With a second administrator in place, both are allowed.
    let bruno = store
        .enrol_operator(
            &NewOperator {
                username: "bruno".into(),
                display_name: "Bruno Costa".into(),
                role: Role::Administrator,
            },
            Some(PASSWORD),
            "ana",
        )
        .unwrap();
    store.set_operator_active(bruno.id, false, "ana").unwrap();
    assert!(!store.operator_by_id(bruno.id).unwrap().unwrap().active);
}

/// An account enrolled by mistake — a mistyped username, somebody who did not
/// join — is the case disabling answers badly: the list carries a name nothing
/// stands behind for the life of the register.
#[test]
fn an_operator_who_wrote_nothing_can_be_removed_outright() {
    // Given an operator who has never signed in
    let store = enrolled();
    let bruno = store
        .enrol_operator(
            &NewOperator {
                username: "bruno".into(),
                display_name: "Bruno Costa".into(),
                role: Role::Distributor,
            },
            Some(PASSWORD),
            "ana",
        )
        .unwrap();

    // When an administrator removes them
    store.remove_operator(bruno.id, "ana").unwrap();

    // Then the account is gone, the removal is on the trail, and the username is
    // free again
    assert!(store.operator_by_id(bruno.id).unwrap().is_none());
    let entry = store
        .audit_entries(40)
        .unwrap()
        .into_iter()
        .find(|entry| entry.event == "operator.removed")
        .expect("the removal is on the trail");
    assert!(entry.details.contains("operator=bruno"), "{entry:?}");
    assert!(entry.details.contains("role=distributor"), "{entry:?}");
    assert!(entry.details.contains("by=ana"), "{entry:?}");
    store
        .enrol_operator(
            &NewOperator {
                username: "bruno".into(),
                display_name: "Bruno Nunes".into(),
                role: Role::Auditor,
            },
            Some(PASSWORD),
            "ana",
        )
        .expect("the username is free again");
}

/// The line this feature is drawn on: an entry whose actor the register cannot
/// name is an entry nobody can act on, so history is what makes an account
/// permanent.
#[test]
fn an_operator_who_wrote_an_audit_entry_can_only_be_disabled() {
    // Given an operator who has signed in, which is one audit entry
    let store = enrolled();
    let bruno = store
        .enrol_operator(
            &NewOperator {
                username: "bruno".into(),
                display_name: "Bruno Costa".into(),
                role: Role::Distributor,
            },
            Some(PASSWORD),
            "ana",
        )
        .unwrap();
    store
        .sign_in_with_password("bruno", PASSWORD, Utc::now())
        .expect("bruno signs in");
    store.act_as(Authority::SignedIn(Role::Administrator));
    store.mark_reverified(Utc::now());

    // When an administrator tries to remove them
    let refused = store.remove_operator(bruno.id, "ana");

    // Then it is refused, in words that name the alternative, and the account is
    // still there to disable
    let Err(StoreError::Forbidden { what, .. }) = &refused else {
        panic!("{refused:?}");
    };
    assert!(what.contains("disable"), "{what}");
    assert!(store.operator_by_id(bruno.id).unwrap().is_some());
    store.set_operator_active(bruno.id, false, "ana").unwrap();
}

/// A failed sign-in is recorded against `(not signed in)` rather than against
/// the username that was typed, so an account that never got in is still
/// removable — and a typo at the sign-in box cannot make one permanent.
#[test]
fn a_failed_sign_in_does_not_make_an_account_permanent() {
    let store = enrolled();
    let bruno = store
        .enrol_operator(
            &NewOperator {
                username: "bruno".into(),
                display_name: "Bruno Costa".into(),
                role: Role::Distributor,
            },
            Some(PASSWORD),
            "ana",
        )
        .unwrap();
    let refused = store.sign_in_with_password("bruno", "the wrong one", Utc::now());
    assert!(refused.is_err());
    store.act_as(Authority::SignedIn(Role::Administrator));
    store.mark_reverified(Utc::now());

    store
        .remove_operator(bruno.id, "ana")
        .expect("a failed attempt is not history of their own");
}

#[test]
fn the_last_administrator_and_the_signed_in_account_cannot_be_removed() {
    let store = enrolled();
    let ana = store.operator_by_username("ana").unwrap().unwrap();

    // The last administrator: the same lockout `set_operator_active` refuses.
    let last = store.remove_operator(ana.id, "felipe");
    assert!(
        matches!(last, Err(StoreError::Forbidden { .. })),
        "{last:?}"
    );

    // And with a second administrator enrolled, ana still cannot remove herself:
    // an administrator who deletes their own account mid-session leaves a
    // session signed in as nobody.
    store
        .enrol_operator(
            &NewOperator {
                username: "bruno".into(),
                display_name: "Bruno Costa".into(),
                role: Role::Administrator,
            },
            Some(PASSWORD),
            "ana",
        )
        .unwrap();
    let herself = store.remove_operator(ana.id, "ana");
    assert!(
        matches!(herself, Err(StoreError::Forbidden { .. })),
        "{herself:?}"
    );
}

#[test]
fn a_distributor_cannot_remove_an_operator() {
    let store = enrolled();
    let bruno = store
        .enrol_operator(
            &NewOperator {
                username: "bruno".into(),
                display_name: "Bruno Costa".into(),
                role: Role::Distributor,
            },
            Some(PASSWORD),
            "ana",
        )
        .unwrap();
    store.act_as(Authority::SignedIn(Role::Distributor));
    store.mark_reverified(Utc::now());

    let refused = store.remove_operator(bruno.id, "bruno");
    assert!(
        matches!(refused, Err(StoreError::Forbidden { .. })),
        "{refused:?}"
    );
    assert!(store.operator_by_id(bruno.id).unwrap().is_some());
}

/// An operator who forgot their password is otherwise locked out of a register
/// that has no other way back in.
#[test]
fn an_administrator_can_replace_another_operators_password() {
    // Given an operator whose password nobody remembers, and a lockout on top
    let store = enrolled();
    let bruno = store
        .enrol_operator(
            &NewOperator {
                username: "bruno".into(),
                display_name: "Bruno Costa".into(),
                role: Role::Distributor,
            },
            Some(PASSWORD),
            "ana",
        )
        .unwrap();
    for _ in 0..3 {
        let _ = store.sign_in_with_password("bruno", "not it", Utc::now());
    }
    store.act_as(Authority::SignedIn(Role::Administrator));
    store.mark_reverified(Utc::now());
    assert!(store.lockout_for("bruno").unwrap().failures > 0);

    // When an administrator sets a new one
    const REPLACEMENT: &str = "a different long enough passphrase";
    store
        .set_operator_password(bruno.id, REPLACEMENT, "ana")
        .unwrap();

    // Then the new password works, the old one does not, the failure history is
    // gone with the credential it counted failures against, and the entry says a
    // credential changed without saying anything about the password
    store.act_as(Authority::SignedOut);
    store
        .sign_in_with_password("bruno", REPLACEMENT, Utc::now())
        .expect("bruno signs in with the new password");
    assert_eq!(store.lockout_for("bruno").unwrap().failures, 0);
    let entry = store
        .audit_entries(40)
        .unwrap()
        .into_iter()
        .find(|entry| entry.event == "operator.credential.changed")
        .expect("the change is on the trail");
    assert!(entry.details.contains("operator=bruno"), "{entry:?}");
    assert!(entry.details.contains("method=local"), "{entry:?}");
    assert!(entry.details.contains("by=ana"), "{entry:?}");
    assert!(!entry.details.contains(REPLACEMENT), "{entry:?}");
    assert!(!entry.details.contains(PASSWORD), "{entry:?}");
    assert!(!entry.details.contains("length"), "{entry:?}");
}

#[test]
fn a_replacement_password_is_held_to_the_same_floor_and_a_distributor_cannot_set_one() {
    let store = enrolled();
    let bruno = store
        .enrol_operator(
            &NewOperator {
                username: "bruno".into(),
                display_name: "Bruno Costa".into(),
                role: Role::Distributor,
            },
            Some(PASSWORD),
            "ana",
        )
        .unwrap();

    let weak = store.set_operator_password(bruno.id, "short", "ana");
    assert!(matches!(weak, Err(StoreError::WeakPassword(_))), "{weak:?}");

    store.act_as(Authority::SignedIn(Role::Distributor));
    store.mark_reverified(Utc::now());
    let refused = store.set_operator_password(bruno.id, "another long enough passphrase", "bruno");
    assert!(
        matches!(refused, Err(StoreError::Forbidden { .. })),
        "{refused:?}"
    );
}

/// Both new writes are sensitive operations, and both are refused on a session
/// whose credential is no longer live (`features/operator-auth-and-roles.md`
/// phase 5).
#[test]
fn removing_an_operator_and_setting_a_password_need_the_credential_again() {
    let store = enrolled();
    let bruno = store
        .enrol_operator(
            &NewOperator {
                username: "bruno".into(),
                display_name: "Bruno Costa".into(),
                role: Role::Distributor,
            },
            Some(PASSWORD),
            "ana",
        )
        .unwrap();
    // The re-verification window closes.
    store.mark_reverified(Utc::now() - Duration::hours(1));

    let removal = store.remove_operator(bruno.id, "ana");
    assert!(
        matches!(removal, Err(StoreError::NeedsReverification { .. })),
        "{removal:?}"
    );
    let password = store.set_operator_password(bruno.id, "another long enough passphrase", "ana");
    assert!(
        matches!(password, Err(StoreError::NeedsReverification { .. })),
        "{password:?}"
    );
}

#[test]
fn an_operator_password_is_held_to_the_same_floor_as_the_database_password() {
    let store = enrolled();
    let refused = store.enrol_operator(
        &NewOperator {
            username: "bruno".into(),
            display_name: "Bruno Costa".into(),
            role: Role::Distributor,
        },
        Some("short"),
        "ana",
    );
    assert!(
        matches!(refused, Err(StoreError::WeakPassword(_))),
        "{refused:?}"
    );
    assert_eq!(store.operator_count().unwrap(), 1);
}

// ------------------------------------------------------------------ sign-in

#[test]
fn a_correct_password_signs_in_and_the_login_is_audited() {
    let store = enrolled();
    store.act_as(Authority::SignedOut);

    let session = store
        .sign_in_with_password("ana", PASSWORD, Utc::now())
        .expect("the password is right");
    assert_eq!(session.username, "ana");
    assert_eq!(session.role, Role::Administrator);

    let entry = store
        .audit_entries(40)
        .unwrap()
        .into_iter()
        .find(|entry| entry.event == "operator.login")
        .expect("the login is on the trail");
    assert!(entry.details.contains("method=local"));
    assert!(entry.details.contains("role=administrator"));
    assert!(!entry.details.contains(PASSWORD));
}

/// The specification's test: a failed login is audited without any password
/// material in the entry.
#[test]
fn a_failed_sign_in_is_audited_with_the_reason_and_the_count_and_nothing_typed() {
    let store = enrolled();
    store.act_as(Authority::SignedOut);

    let refused = store.sign_in_with_password("ana", "not the password at all", Utc::now());
    assert!(
        matches!(refused, Err(StoreError::Forbidden { .. })),
        "{refused:?}"
    );

    let entry = store
        .audit_entries(40)
        .unwrap()
        .into_iter()
        .find(|entry| entry.event == "operator.login.failed")
        .expect("the failure is on the trail");
    assert_eq!(entry.target, "operator:ana");
    assert!(entry.details.contains("method=local"));
    assert!(entry.details.contains("reason=bad-credential"));
    assert!(entry.details.contains("attempt=1"));
    assert!(entry.details.contains("lockout=0"));
    // Not the password, and not a length either.
    assert!(!entry.details.contains("not the password"));
    assert!(!entry.details.to_lowercase().contains("password="));
    assert!(!entry.details.contains("length"));
    assert_eq!(entry.actor, "(not signed in)");
}

/// Telling "no such account" apart from "wrong password" on screen tells an
/// attacker which usernames exist.
#[test]
fn an_unknown_username_is_refused_in_the_same_words_as_a_wrong_password() {
    let store = enrolled();
    store.act_as(Authority::SignedOut);

    let Err(StoreError::Forbidden { what: unknown, .. }) =
        store.sign_in_with_password("nobody", PASSWORD, Utc::now())
    else {
        panic!("an unknown username is refused");
    };
    let Err(StoreError::Forbidden { what: wrong, .. }) =
        store.sign_in_with_password("ana", "the wrong one entirely", Utc::now())
    else {
        panic!("a wrong password is refused");
    };
    assert_eq!(unknown, wrong);
}

#[test]
fn three_failures_lock_the_account_and_the_lockout_survives_in_the_register() {
    let store = enrolled();
    store.act_as(Authority::SignedOut);
    let now = Utc::now();

    for attempt in 1..=3 {
        let _ = store.sign_in_with_password("ana", "wrong", now);
        assert_eq!(store.lockout_for("ana").unwrap().failures, attempt);
    }
    assert!(store.lockout_for("ana").unwrap().is_locked_at(now));

    // Even the *right* password is refused while the lockout runs — otherwise
    // the lockout is advice.
    let refused = store.sign_in_with_password("ana", PASSWORD, now);
    assert!(
        matches!(refused, Err(StoreError::Forbidden { .. })),
        "{refused:?}"
    );

    // And it expires on its own.
    store
        .sign_in_with_password("ana", PASSWORD, now + Duration::seconds(61))
        .expect("a minute later the account is usable again");
    assert_eq!(
        store.lockout_for("ana").unwrap().failures,
        0,
        "a success clears the history"
    );
}

/// Counting failures only against accounts that exist would turn the lockout
/// into an oracle for who is on the register.
#[test]
fn failures_against_a_username_that_does_not_exist_are_counted_too() {
    let store = enrolled();
    store.act_as(Authority::SignedOut);
    let now = Utc::now();

    for _ in 0..3 {
        let _ = store.sign_in_with_password("nobody", "guess", now);
    }
    let lockout = store.lockout_for("nobody").unwrap();
    assert_eq!(lockout.failures, 3);
    assert!(lockout.is_locked_at(now));
}

#[test]
fn a_disabled_operator_cannot_sign_in() {
    let store = enrolled();
    store
        .enrol_operator(
            &NewOperator {
                username: "bruno".into(),
                display_name: "Bruno Costa".into(),
                role: Role::Distributor,
            },
            Some(PASSWORD),
            "ana",
        )
        .unwrap();
    let bruno = store.operator_by_username("bruno").unwrap().unwrap();
    store.set_operator_active(bruno.id, false, "ana").unwrap();

    store.act_as(Authority::SignedOut);
    let refused = store.sign_in_with_password("bruno", PASSWORD, Utc::now());
    assert!(
        matches!(refused, Err(StoreError::Forbidden { .. })),
        "{refused:?}"
    );
}

#[test]
fn an_administrator_can_lift_a_lockout_and_the_lift_is_audited() {
    let store = enrolled();
    let now = Utc::now();
    store.act_as(Authority::SignedOut);
    for _ in 0..3 {
        let _ = store.sign_in_with_password("ana", "wrong", now);
    }
    assert!(store.lockout_for("ana").unwrap().is_locked_at(now));

    store.act_as(Authority::SignedIn(Role::Administrator));
    store.mark_reverified(now);
    store.clear_operator_lockout("ana", "felipe").unwrap();

    assert!(!store.lockout_for("ana").unwrap().is_locked_at(now));
    assert!(
        store
            .audit_entries(60)
            .unwrap()
            .iter()
            .any(|entry| entry.event == "operator.lockout.cleared")
    );
}

#[test]
fn a_refusal_is_itself_recorded() {
    let store = enrolled();
    store.act_as(Authority::SignedIn(Role::Auditor));
    store
        .record_refusal("bruno", Action::ResetApplet, "role")
        .unwrap();

    let entry = store
        .audit_entries(40)
        .unwrap()
        .into_iter()
        .find(|entry| entry.event == "operator.authorisation.refused")
        .expect("a refusal is a security event in its own right");
    assert!(entry.details.contains("action=reset-applet"));
    assert!(entry.details.contains("authority=Auditor"));
}

// ------------------------------------- the guards are wired, not merely present

/// The gap this closes: every one of the checks above passed while **no caller
/// used them**. `Store::require` was reachable only from operator management, so
/// a template edit, a factory reset, a re-key and an export all went through with
/// a session hours old. A check nothing calls is documentation.
#[test]
fn a_procedure_edit_asks_for_the_credential_again_at_the_write_itself() {
    // Given an administrator whose re-verification has lapsed
    let store = enrolled();
    store.act_as(Authority::SignedIn(Role::Administrator));
    let template = yk_dist_manager::template::BootstrapTemplate::org_standard();

    // When they store a procedure without presenting the credential again
    let refused = store.upsert_template(&template);

    // Then the write itself refuses — not a screen, and not a check nobody calls
    assert!(
        matches!(refused, Err(StoreError::NeedsReverification { .. })),
        "{refused:?}"
    );

    // And the same is true of a term, which is what a holder signs
    let term = yk_dist_manager::term::TermTemplate::builtin()
        .into_iter()
        .next()
        .expect("this build ships terms");
    assert!(
        matches!(
            store.upsert_term_template(&term),
            Err(StoreError::NeedsReverification { .. })
        ),
        "a term is as sensitive as a procedure"
    );

    // When the credential is presented again, the write goes through
    store.mark_reverified(Utc::now());
    store.upsert_template(&template).expect("re-verified");
    store.upsert_term_template(&term).expect("re-verified");
}

/// Retiring, reinstating and deleting a version are the three ways to change what
/// the wizard offers without writing a new one, and each is as sensitive as the
/// edit.
#[test]
fn withdrawing_a_procedure_is_as_sensitive_as_writing_one() {
    let store = enrolled();
    let template = yk_dist_manager::template::BootstrapTemplate::org_standard();
    store
        .upsert_template(&template)
        .expect("stored while re-verified");
    let (id, version) = (template.id.clone(), template.version.clone());

    // Given the re-verification has lapsed
    store.act_as(Authority::SignedIn(Role::Administrator));

    for refused in [
        store.retire_template(&id, &version),
        store.reinstate_template(&id, &version),
        store.delete_template(&id, &version),
    ] {
        assert!(
            matches!(refused, Err(StoreError::NeedsReverification { .. })),
            "{refused:?}"
        );
    }
}

/// The role half of a template write is left to SQLite on purpose, and that is a
/// property worth pinning: the explicit check must not start answering first, or
/// the authorizer stops being exercised by anything.
#[test]
fn the_role_refusal_on_a_template_still_comes_from_the_database() {
    let store = enrolled();
    store.act_as(Authority::SignedIn(Role::Distributor));
    // Re-verified, so the only thing left to refuse is the role.
    store.mark_reverified(Utc::now());

    let refused =
        store.upsert_template(&yk_dist_manager::template::BootstrapTemplate::org_standard());
    assert!(
        matches!(refused, Err(StoreError::NotAuthorised)),
        "the authorizer is the layer that refuses a role, got {refused:?}"
    );
}

/// The seeds run before anybody has signed in, and must not be caught by the
/// guard on the method they go through — a register that could not seed its own
/// built-in procedures would open half-configured.
#[test]
fn opening_a_register_still_seeds_its_built_in_procedures() {
    // Given a freshly opened register, which is `Unenrolled` however many
    // operators the file holds
    let store = register();
    assert_eq!(store.authority(), Authority::Unenrolled);

    // When it seeds
    let templates = store.seed_builtin_templates().expect("the seeds run");
    let terms = store.seed_builtin_terms().expect("the seeds run");

    // Then they went in
    assert!(templates > 0, "this build ships procedures");
    assert!(terms > 0, "this build ships terms");
}
