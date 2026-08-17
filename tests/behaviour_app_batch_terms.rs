//! Behaviour: the terms a box of keys owes, in one action
//! (`features/bulk-enrollment.md` phase 7, `features/receipts-and-terms.md` phase 7).
//!
//! What no unit test covers: `batch::documents` decides *which* positions owe a
//! document and what each is called, and that is tested next to the code. What
//! happens here is the rest of it — the term is actually rendered from the stored
//! template, the file lands on disk, the trail carries one `term.generated` per
//! holder rather than one line for the box, and a stock batch is refused with a
//! sentence that says where to go instead.

use yk_dist_manager::YkDistApp;
use yk_dist_manager::batch::{Batch, Outcome, Presented, pairing::Pair};
use yk_dist_manager::domain::{Holder, SerialSource, YubiKeyRecord};
use yk_dist_manager::store::{Store, StoreConfig};

const FIRST: u32 = 20_423_631;
const SECOND: u32 = 20_423_632;
const THIRD: u32 = 20_423_633;

fn events(app: &YkDistApp, event: &str) -> Vec<String> {
    app.store
        .as_ref()
        .expect("a register is open")
        .audit_entries(400)
        .expect("the trail reads back")
        .into_iter()
        .filter(|entry| entry.event == event)
        .map(|entry| entry.details)
        .collect()
}

#[test]
fn scenario_an_assigned_batch_writes_one_term_per_finished_key() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("batch-terms.sqlite3");
    let into = dir.path().join("out");
    std::fs::create_dir_all(&into).unwrap();

    // Given three people, three keys, and an assigned batch where two keys are
    // done and the third never ran
    let batch_id = {
        let store =
            Store::create_new(&StoreConfig::new(&database).with_operator("felipe")).unwrap();
        let mut pairs = Vec::new();
        for (index, serial) in [FIRST, SECOND, THIRD].into_iter().enumerate() {
            store
                .upsert_key(&YubiKeyRecord::from_serial(serial, SerialSource::Device))
                .unwrap();
            let holder = Holder::new(
                &format!("Pessoa {index}"),
                &format!("pessoa.{index}@example.org"),
                "ESI",
                &format!("{index}"),
            )
            .unwrap();
            store.insert_holder(&holder).unwrap();
            pairs.push(Pair {
                serial: None,
                email: holder.email.clone(),
                holder_id: Some(holder.id),
                line: index + 2,
            });
        }

        let mut batch = Batch::assigned("org-standard", "2", "felipe", &pairs);
        store.insert_batch(&batch).unwrap();
        for (position, serial) in [FIRST, SECOND].into_iter().enumerate() {
            assert_eq!(batch.present(serial), Presented::Ready { position });
            batch.record(
                position,
                Outcome::Done {
                    run: uuid::Uuid::new_v4(),
                },
            );
            store
                .record_batch_entry(batch.id, &batch.entries[position])
                .unwrap();
        }
        let id = batch.id;
        let _ = store.close();
        id
    };

    let mut app = YkDistApp::new(Some(database.clone()));
    assert!(app.store.is_some(), "{:?}", app.db_form.error);
    app.reload_batches();
    app.resume_batch(batch_id);

    // When the operator asks for the hand-over documents with the box half done
    let directory = app
        .generate_batch_terms(&into)
        .expect("the batch owes documents");

    // Then there is one file per finished key, named for the serial and nothing
    // else — a folder listing is not a staff list
    let mut written: Vec<String> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    written.sort();
    assert_eq!(
        written,
        vec![format!("termo-{FIRST}.pdf"), format!("termo-{SECOND}.pdf")],
        "one term per finished key, and none for the key that never ran"
    );
    for name in &written {
        let bytes = std::fs::read(directory.join(name)).unwrap();
        assert!(
            bytes.starts_with(b"%PDF-"),
            "{name} is a PDF somebody can print and sign"
        );
    }

    // And the position that produced nothing is reported rather than passed over
    // in silence
    let notice = app
        .batch
        .notice
        .clone()
        .expect("the batch says what it did");
    assert!(
        notice.contains("2 term(s)") && notice.contains("1 position(s) produced nothing"),
        "the notice counts both what was written and what was not: {notice}"
    );
    assert!(app.batch.error.is_none(), "{:?}", app.batch.error);

    // And the trail carries one entry per holder, not one for the box
    let generated = events(&app, "term.generated");
    assert_eq!(generated.len(), 2, "one entry per term: {generated:?}");
    assert!(
        generated
            .iter()
            .any(|d| d.contains("pessoa.0@example.org") && d.contains("consignment@")),
        "each entry names the holder and the template version that produced it: {generated:?}"
    );
    assert!(
        generated.iter().all(|d| d.contains(&batch_id.to_string())),
        "and the batch it came from: {generated:?}"
    );

    let summary = events(&app, "batch.terms");
    assert_eq!(summary.len(), 1);
    assert!(
        summary[0].contains("documents=2") && summary[0].contains("skipped=1"),
        "the summary counts the set: {}",
        summary[0]
    );
    assert!(
        summary[0].contains("refused=0"),
        "and says nothing was refused: {}",
        summary[0]
    );
}

#[test]
fn scenario_a_stock_batch_is_refused_and_told_where_to_go_instead() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("stock-terms.sqlite3");
    let into = dir.path().join("out");
    std::fs::create_dir_all(&into).unwrap();

    // Given a stock-preparation batch — keys with nobody's name on them yet
    let batch_id = {
        let store =
            Store::create_new(&StoreConfig::new(&database).with_operator("felipe")).unwrap();
        store
            .upsert_key(&YubiKeyRecord::from_serial(FIRST, SerialSource::Device))
            .unwrap();
        let mut batch = Batch::stock("org-standard", "2", "felipe", 2);
        store.insert_batch(&batch).unwrap();
        assert_eq!(batch.present(FIRST), Presented::Ready { position: 0 });
        batch.record(
            0,
            Outcome::Done {
                run: uuid::Uuid::new_v4(),
            },
        );
        store
            .record_batch_entry(batch.id, &batch.entries[0])
            .unwrap();
        let id = batch.id;
        let _ = store.close();
        id
    };

    let mut app = YkDistApp::new(Some(database.clone()));
    app.reload_batches();
    app.resume_batch(batch_id);

    // When the operator asks for hand-over documents anyway
    let result = app.generate_batch_terms(&into);

    // Then nothing is written — a consignment term with no name on it is a form,
    // and a form that comes out of the register looks like a record
    assert!(result.is_none(), "a stock batch produces no terms");
    assert_eq!(
        std::fs::read_dir(&into).unwrap().count(),
        0,
        "not even an empty folder"
    );
    let refusal = app.batch.error.clone().expect("the refusal is on screen");
    assert!(
        refusal.contains("no holders") && refusal.contains("Distribution"),
        "the refusal says why, and where to go instead: {refusal}"
    );
    assert!(
        events(&app, "term.generated").is_empty(),
        "and the trail claims no term"
    );
    assert!(events(&app, "batch.terms").is_empty());
}
