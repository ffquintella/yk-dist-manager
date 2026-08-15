//! Behaviour: a box of keys in one sitting (`features/bulk-enrollment.md`).
//!
//! What no unit test covers: the batch is *persisted as it goes*, so a session
//! that ends on key 3 of 5 comes back and finishes the box rather than starting
//! it again — and the trail carries one entry per key, beside the run's own,
//! rather than one summary at the end.
//!
//! The runs themselves are recorded directly rather than driven through the
//! executor: what is under test here is the bookkeeping, and a batch that needed
//! a plugged-in key to test its counting would be a batch nobody could test.

use yk_dist_manager::batch::{Batch, EntryState, Outcome, Presented, Shape, pairing};
use yk_dist_manager::domain::{SerialSource, YubiKeyRecord};
use yk_dist_manager::store::{Store, StoreConfig};

fn register(path: &std::path::Path) -> Store {
    let store = Store::create_new(&StoreConfig::new(path).with_operator("felipe")).unwrap();
    for serial in [1_u32, 2, 3, 4, 5] {
        store
            .upsert_key(&YubiKeyRecord::from_serial(
                20_423_630 + serial,
                SerialSource::Device,
            ))
            .unwrap();
    }
    store
}

#[test]
fn scenario_a_batch_interrupted_on_key_three_comes_back_and_finishes_the_box() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("batch.sqlite3");

    // Given a stock batch of five keys, two of them done
    let id = {
        let store = register(&database);
        let mut batch = Batch::stock("org-standard", "2", "felipe", 5);
        store.insert_batch(&batch).unwrap();

        for (index, serial) in [20_423_631_u32, 20_423_632].into_iter().enumerate() {
            assert_eq!(batch.present(serial), Presented::Ready { position: index });
            batch.record(
                index,
                Outcome::Done {
                    run: uuid::Uuid::new_v4(),
                },
            );
            store
                .record_batch_entry(batch.id, &batch.entries[index])
                .unwrap();
        }
        assert!(!batch.is_complete());
        store.close();
        batch.id
    };

    // When the register is opened again — the laptop closed, the operator came
    // back after lunch
    let store = Store::open_existing(&StoreConfig::new(&database)).unwrap();
    let mut resumable = store.unfinished_batches().unwrap();
    assert_eq!(resumable.len(), 1, "an unfinished batch is offered");

    let mut batch = resumable.remove(0);
    assert_eq!(batch.id, id);
    assert_eq!(
        batch.template_version, "2",
        "it finishes with what it started"
    );

    // Then it resumes at key 3, and the two already done are not offered again
    assert_eq!(batch.tally().done, 2);
    assert_eq!(batch.next_pending().map(|e| e.position), Some(2));
    assert!(batch.resume_audit_detail().contains("from_key=3"));

    assert!(
        matches!(
            batch.present(20_423_631),
            Presented::Duplicate {
                position: 0,
                state: EntryState::Done
            }
        ),
        "a key already done is refused, not re-run"
    );

    // And the rest of the box goes through, with key 4 failing and the batch
    // carrying on rather than stopping dead
    assert_eq!(batch.present(20_423_633), Presented::Ready { position: 2 });
    batch.record(
        2,
        Outcome::Done {
            run: uuid::Uuid::new_v4(),
        },
    );
    assert_eq!(batch.present(20_423_634), Presented::Ready { position: 3 });
    batch.record(
        3,
        Outcome::Failed {
            run: None,
            reason: "the PIV applet did not answer".into(),
        },
    );
    assert_eq!(batch.present(20_423_635), Presented::Ready { position: 4 });
    batch.record(
        4,
        Outcome::Done {
            run: uuid::Uuid::new_v4(),
        },
    );

    for entry in &batch.entries {
        store.record_batch_entry(batch.id, entry).unwrap();
    }
    store.finish_batch(batch.id, chrono::Utc::now()).unwrap();

    let tally = batch.tally();
    assert_eq!(tally.audit_detail(), "succeeded=4 failed=1 skipped=0");
    assert_eq!(batch.needs_attention().len(), 1);

    // And what is on the register agrees, down to the reason the one key failed
    let reloaded = store
        .batches()
        .unwrap()
        .into_iter()
        .find(|b| b.id == id)
        .expect("the batch is on the register");
    assert!(reloaded.is_complete());
    assert!(reloaded.finished_at.is_some());
    assert_eq!(reloaded.tally().done, 4);
    let attention = reloaded.needs_attention();
    assert_eq!(attention.len(), 1);
    assert_eq!(attention[0].serial, Some(20_423_634));
    assert!(attention[0].detail.contains("PIV applet"));
    store.close();
}

#[test]
fn scenario_an_assigned_batch_keeps_each_key_with_its_person_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("assigned.sqlite3");

    let id = {
        let store = register(&database);
        let pairs = pairing::parse(
            "serial,email\n20423631,ana@example.org\n20423632,bruno@example.org\n",
            &std::collections::BTreeMap::new(),
        )
        .expect("a valid list");

        let batch = Batch::assigned("org-standard", "2", "felipe", &pairs);
        assert_eq!(batch.shape, Shape::AssignedEnrolment);
        store.insert_batch(&batch).unwrap();
        store.close();
        batch.id
    };

    // Reopened: the pairing survives, which is the point — a certificate carries
    // the holder's address, so losing which key was whose would mean issuing to
    // the wrong person
    let store = Store::open_existing(&StoreConfig::new(&database)).unwrap();
    let batch = store
        .batches()
        .unwrap()
        .into_iter()
        .find(|b| b.id == id)
        .expect("the batch is on the register");

    assert_eq!(batch.entries.len(), 2);
    assert_eq!(batch.entries[0].serial, Some(20_423_631));
    assert_eq!(batch.entries[0].holder_display, "ana@example.org");
    assert_eq!(batch.entries[1].serial, Some(20_423_632));
    assert_eq!(batch.entries[1].holder_display, "bruno@example.org");
    store.close();
}

#[test]
fn scenario_a_register_written_before_batches_existed_still_opens() {
    // Schema v8 adds two tables and alters nothing, so a v7 register migrates
    // forward with an empty batch list rather than a backfill.
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("old.sqlite3");

    let store = register(&database);
    store.close();

    let store = Store::open_existing(&StoreConfig::new(&database)).unwrap();
    assert!(store.batches().unwrap().is_empty());
    assert!(store.unfinished_batches().unwrap().is_empty());
    store.close();
}
