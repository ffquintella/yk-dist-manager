//! Behaviour: two operators in the same register
//! (`features/storage-sqlite-single-file.md` phase 4,
//! `features/database-selection.md` phases 6 and 8).
//!
//! The three things a share makes possible that a single desk does not, each
//! checked against two real connections to one file rather than against a mock:
//!
//! 1. **They can see each other.** Not a lock — on a share SQLite serialises the
//!    writes perfectly well — but a name, because the operator about to start a
//!    hand-over is the one who needs to know somebody else is working out of the
//!    same box of keys.
//! 2. **The second save loses, loudly.** Two screens painted minutes apart, both
//!    with the same key on them: without the optimistic check the first
//!    operator's observation vanishes and neither of them ever finds out.
//! 3. **A session that goes quiet stops being shown**, so a closed laptop does
//!    not leave a name on somebody's screen for the rest of the week.

use yk_dist_manager::domain::{SerialSource, YubiKeyRecord};
use yk_dist_manager::store::{Store, StoreConfig, StoreError};

fn config(path: &std::path::Path, operator: &str) -> StoreConfig {
    // `without_lease` because this is the *share* case: a local path never takes
    // the cloud-sync lock anyway, and being explicit says which mechanism is
    // under test.
    StoreConfig::new(path)
        .with_operator(operator)
        .without_lease()
}

#[test]
fn scenario_two_operators_see_each_other_and_the_second_save_does_not_win() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("shared.sqlite3");

    // Given a register on the unit's share with one key on it
    let ana = Store::create_new(&config(&database, "Ana")).unwrap();
    ana.upsert_key(&YubiKeyRecord::from_serial(
        20_423_633,
        SerialSource::Device,
    ))
    .unwrap();

    // Ana is alone, and the screen says nothing at all rather than "0 others"
    assert!(ana.presence().unwrap().is_empty());
    assert_eq!(ana.presence().unwrap().describe(chrono::Utc::now()), None);

    // When Bruno opens the same file from the next desk
    let bruno = Store::open_existing(&config(&database, "Bruno")).unwrap();

    // Then each of them can see the other, by name, and neither sees themselves
    let seen_by_ana = ana.presence().unwrap();
    assert_eq!(seen_by_ana.others.len(), 1, "{seen_by_ana:?}");
    assert_eq!(seen_by_ana.others[0].operator, "Bruno");
    let line = seen_by_ana
        .describe(chrono::Utc::now())
        .expect("something to say");
    assert!(line.contains("Bruno"), "{line}");

    let seen_by_bruno = bruno.presence().unwrap();
    assert_eq!(seen_by_bruno.others.len(), 1, "{seen_by_bruno:?}");
    assert_eq!(seen_by_bruno.others[0].operator, "Ana");

    // Given both of them have the key on screen
    let ana_sees = ana.key_by_serial(20_423_633).unwrap().unwrap();
    let bruno_sees = bruno.key_by_serial(20_423_633).unwrap().unwrap();

    // When Ana records an observation first
    ana.set_key_notes(
        20_423_633,
        "connector bent — do not hand out",
        ana_sees.updated_at,
    )
    .unwrap();

    // Then Bruno's save is refused, and the refusal says when the record moved
    // under him rather than only that it did
    match bruno.set_key_notes(20_423_633, "spare", bruno_sees.updated_at) {
        Err(StoreError::Conflict { what, theirs }) => {
            assert!(what.contains("20423633"), "{what}");
            assert!(!theirs.is_empty(), "the refusal has to name the moment");
        }
        other => panic!("expected a conflict, got {other:?}"),
    }

    // And what is on the register is Ana's warning, not Bruno's overwrite — the
    // whole point: the key that must not be handed out still says so
    assert_eq!(
        bruno.key_by_serial(20_423_633).unwrap().unwrap().notes,
        "connector bent — do not hand out"
    );

    // When Bruno reloads and tries again, his edit goes in
    let reloaded = bruno.key_by_serial(20_423_633).unwrap().unwrap();
    bruno
        .set_key_notes(
            20_423_633,
            "connector bent; RMA raised",
            reloaded.updated_at,
        )
        .unwrap();
    assert_eq!(
        ana.key_by_serial(20_423_633).unwrap().unwrap().notes,
        "connector bent; RMA raised"
    );

    // When Bruno goes home
    bruno.close();

    // Then his name leaves Ana's screen at once, rather than ageing out
    assert!(
        ana.presence().unwrap().is_empty(),
        "a closed session releases its row"
    );
    ana.close();
}

#[test]
fn scenario_a_session_that_crashed_ages_out_instead_of_haunting_the_banner() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("crashed.sqlite3");

    // A session that ended without releasing: the laptop was closed, or the
    // process was killed. There is no polite way to produce one — the row is only
    // left behind when nothing runs `close` — so it is aged by hand.
    let bruno = Store::create_new(&config(&database, "Bruno")).unwrap();
    bruno
        .age_sessions_for_tests(chrono::Duration::hours(8))
        .unwrap();
    // Deliberately *not* closed: this is the crash, and a `close` here would
    // remove the very row the test is about.
    std::mem::forget(bruno);

    let ana = Store::open_existing(&config(&database, "Ana")).unwrap();

    // It is not shown: a row is a claim about a moment, not a fact about now
    assert!(
        ana.presence().unwrap().is_empty(),
        "a session silent for eight hours is not somebody at a desk"
    );

    // And it was cleared away by this open rather than left to accumulate — the
    // pruning happens on the write, so a read-only session never has to write to
    // read.
    assert_eq!(
        ana.session_count_for_tests().unwrap(),
        1,
        "only this session's own row is left"
    );
    ana.close();
}
