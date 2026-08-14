//! Behaviour test for what happens to a key after the hand-over
//! (`features/key-lifecycle-and-revocation.md` phases 2, 3, 4, 6, 7 and 8).
//!
//! One story, end to end, driven through `YkDistApp` the way the Inventory
//! screen's *Lifecycle…* panel drives it: a key that was bootstrapped and handed
//! over is reported stolen, and the register has to be able to say what was on it,
//! refuse to send it back out until it is clean, and produce the note somebody has
//! to send.
//!
//! **One test in this file, deliberately** — it drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared by
//! every test in a binary.
//!
//! Nothing here touches a key: the only hardware-facing thing in the story is the
//! sanitisation, and this scenario records the one an operator performed
//! elsewhere. The reset that records its own is covered in
//! `behaviour_key_reset.rs`, against the mock resetter.

use yk_dist_manager::YkDistApp;
use yk_dist_manager::device::reset::{Applet, Outcome, Status as ResetStatus};
use yk_dist_manager::domain::lifecycle::DependencyKind;
use yk_dist_manager::domain::{
    BootstrapRun, DeliveryMethod, DistributionRecord, Holder, IncidentKind, KeyStatus, RunStatus,
    SerialSource, StepKind, StepOutcome, StepStatus, YubiKeyRecord,
};
use yk_dist_manager::store::{Store, StoreConfig, StoreError};

const SERIAL: u32 = 20_423_633;
const REPLACEMENT: u32 = 20_423_634;
const CERTIFICATE: &str = "0A1B2C3D";
const CREDENTIAL: &str = "DEADBEEFCAFE";

fn step(kind: StepKind, detail: &str) -> StepOutcome {
    StepOutcome {
        step_id: kind.slug().into(),
        kind,
        status: StepStatus::Done,
        started_at: Some(chrono::Utc::now()),
        finished_at: Some(chrono::Utc::now()),
        detail: detail.into(),
    }
}

/// A run that did what the standard procedure does: a PIN, a resident credential
/// and a signing certificate. The details are the ones the real steps write, since
/// that is what the dependency list reads back.
fn completed_run(holder: &Holder) -> BootstrapRun {
    let mut run = BootstrapRun::new(
        SERIAL,
        Some(holder.id),
        "org-standard",
        "2",
        "felipe",
        vec![
            step(StepKind::Fido2Pin, "[native] transport PIN set"),
            step(
                StepKind::Fido2Credential,
                &format!(
                    "[native] resident credential registered — credential_id={CREDENTIAL} \
                     rp_id=idp.example algorithm=ES256 user_name=ana.silva@example.org"
                ),
            ),
            step(
                StepKind::PivCertImport,
                &format!(
                    "[native] certificate imported into slot 9c — subject=CN=Ana issuer=CN=Unit CA \
                     serial={CERTIFICATE} valid=2026-01-01..2027-01-01"
                ),
            ),
        ],
    );
    run.custody = "sealed-envelope".into();
    run.settle();
    assert_eq!(run.status, RunStatus::Completed);
    run
}

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

fn status_of(app: &YkDistApp, serial: u32) -> KeyStatus {
    app.store
        .as_ref()
        .expect("a register is open")
        .key_by_serial(serial)
        .expect("the key reads back")
        .expect("the key is in the inventory")
        .status
}

#[test]
fn scenario_a_stolen_key_is_reported_dealt_with_and_only_then_reissued() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
    }
    let database = home.path().join("keys.sqlite3");

    // Given: a key that was bootstrapped and handed to Ana, and a spare in stock
    let holder = Holder::new("Ana Silva", "ana.silva@example.org", "ESI", "1").unwrap();
    {
        let store = Store::create_new(&StoreConfig::new(&database)).unwrap();
        store
            .upsert_key(&YubiKeyRecord::from_serial(
                SERIAL,
                SerialSource::ManualEntry,
            ))
            .unwrap();
        store
            .upsert_key(&YubiKeyRecord::from_serial(
                REPLACEMENT,
                SerialSource::ManualEntry,
            ))
            .unwrap();
        store.insert_holder(&holder).unwrap();
        let run = completed_run(&holder);
        store.insert_run(&run).unwrap();
        store
            .set_key_status(SERIAL, KeyStatus::Bootstrapped)
            .unwrap();
        let record = DistributionRecord {
            id: uuid::Uuid::new_v4(),
            key_id: store.key_by_serial(SERIAL).unwrap().unwrap().id,
            key_serial: SERIAL,
            holder_id: holder.id,
            holder_display: holder.display(),
            distributed_at: chrono::Utc::now(),
            distributed_by: "felipe".into(),
            method: DeliveryMethod::InPerson,
            receipt_ref: "TERM-2026-001".into(),
            bootstrap_run_id: Some(run.id),
            returned_at: None,
            returned_to: None,
            notes: String::new(),
        };
        store.insert_distribution(&record).unwrap();
        store
            .set_key_status(SERIAL, KeyStatus::Distributed)
            .unwrap();
        let _ = store.close();
    }

    let mut app = YkDistApp::new(Some(database.clone()));
    assert!(app.store.is_some(), "{:?}", app.db_form.error);

    // When the operator opens the lifecycle panel for that key
    app.open_key_lifecycle(SERIAL);

    // Then it already knows who had it, and what was put on it — read off the run
    // rather than from a list somebody maintained
    assert_eq!(app.lifecycle.reported_by, holder.display());
    let subjects: Vec<(DependencyKind, String)> = app
        .lifecycle
        .dependencies
        .iter()
        .map(|d| (d.kind, d.subject.clone()))
        .collect();
    assert!(
        subjects.contains(&(DependencyKind::Certificate, CERTIFICATE.to_owned())),
        "the certificate in slot 9c is a dependency: {subjects:?}"
    );
    assert!(
        subjects.contains(&(DependencyKind::Credential, CREDENTIAL.to_owned())),
        "so is the resident credential: {subjects:?}"
    );
    assert!(
        subjects
            .iter()
            .any(|(kind, subject)| *kind == DependencyKind::Custody && subject == "sealed-envelope"),
        "and where the secrets went is stated, for information: {subjects:?}"
    );

    // When a report arrives with nobody's name on it
    app.lifecycle.report_kind = IncidentKind::Stolen;
    app.lifecycle.reported_by.clear();
    app.report_key_incident();

    // Then it is refused, and the key has not moved: a register does not assert a
    // loss on its own authority
    assert!(
        app.lifecycle
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("reported_by"),
        "{:?}",
        app.lifecycle.error
    );
    assert_eq!(status_of(&app, SERIAL), KeyStatus::Distributed);
    assert!(events(&app, "key.reported_lost").is_empty());

    // When the report is complete
    app.lifecycle.reported_by = "Ana Silva".into();
    app.lifecycle.report_date = "2026-08-13".into();
    app.lifecycle.circumstances = "bag taken on the bus".into();
    app.report_key_incident();

    // Then the key is lost, the report is on the register, and the trail names the
    // kind, the date and the reporter — but not the circumstances, which are text
    // an operator may need to correct and the trail cannot
    assert_eq!(app.lifecycle.error, None);
    assert_eq!(status_of(&app, SERIAL), KeyStatus::Lost);
    let reported = events(&app, "key.reported_lost");
    assert_eq!(reported.len(), 1, "{reported:?}");
    assert!(
        reported[0].contains("kind=stolen")
            && reported[0].contains("2026-08-13")
            && reported[0].contains("Ana Silva"),
        "{}",
        reported[0]
    );
    assert!(
        !reported[0].contains("bag taken"),
        "the circumstances are counted, not quoted: {}",
        reported[0]
    );
    let incident = app
        .open_incident()
        .cloned()
        .expect("the incident is open now");
    assert_eq!(incident.circumstances, "bag taken on the bus");

    // When somebody tries to put the key back into stock while it is still
    // carrying Ana's credentials — the key was recovered, say, or the report was
    // premature
    let refusal = app
        .store
        .as_ref()
        .unwrap()
        .set_key_status(SERIAL, KeyStatus::Returned)
        .and_then(|()| {
            app.store
                .as_ref()
                .unwrap()
                .set_key_status(SERIAL, KeyStatus::InStock)
        });

    // Then it is refused, by name, and the refusal says what to do about it
    match refusal {
        Err(StoreError::NotSanitised { serial, reason }) => {
            assert_eq!(serial, SERIAL);
            assert!(
                reason.contains("FIDO2") && reason.contains("PIV"),
                "{reason}"
            );
            assert!(reason.contains("factory default"), "{reason}");
        }
        other => panic!("an unsanitised key must not go back into stock: {other:?}"),
    }
    assert_eq!(status_of(&app, SERIAL), KeyStatus::Returned);

    // When the operator revokes the certificate at the CA and records it
    let certificate = app
        .lifecycle
        .dependencies
        .iter()
        .find(|d| d.kind == DependencyKind::Certificate)
        .cloned()
        .unwrap();
    app.settle_dependency(&certificate);
    app.lifecycle.reference = "CRL-2026-14".into();
    app.record_remediation();

    // Then it is on the record with its reason and reference, and the entry says
    // so — `keyCompromise`, because a key somebody else may be holding is one
    assert_eq!(app.lifecycle.error, None);
    let revoked = events(&app, "key.certificate_revoked");
    assert_eq!(revoked.len(), 1, "{revoked:?}");
    assert!(
        revoked[0].contains(CERTIFICATE)
            && revoked[0].contains("reason=keyCompromise")
            && revoked[0].contains("CRL-2026-14"),
        "{}",
        revoked[0]
    );

    // And the note produced now still says the credential is outstanding
    app.generate_incident_note(incident.id);
    let note = app
        .lifecycle
        .note
        .clone()
        .map(|(_, text)| text)
        .expect("a note was produced");
    assert!(
        note.contains(CERTIFICATE) && note.contains("dealt with on"),
        "{note}"
    );
    assert!(
        note.contains(CREDENTIAL) && note.contains("OUTSTANDING"),
        "{note}"
    );
    assert!(note.contains("Ana Silva"), "{note}");
    assert!(
        note.contains("Check by hand"),
        "the note names its own blind spot: {note}"
    );
    assert_eq!(events(&app, "key.incident_note").len(), 1);

    // When an incident is closed with something still owed and no explanation
    app.lifecycle.detail.clear();
    app.close_key_incident(incident.id);

    // Then it stays open: the gap has to be visible, either as work done or as a
    // reason it was not
    assert!(
        app.lifecycle
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("not been dealt with"),
        "{:?}",
        app.lifecycle.error
    );
    assert!(app.open_incident().is_some());

    // When the credential is removed at the relying party and recorded
    let credential = app
        .lifecycle
        .dependencies
        .iter()
        .find(|d| d.kind == DependencyKind::Credential)
        .cloned()
        .unwrap();
    app.settle_dependency(&credential);
    app.lifecycle.reference = "IDP-991".into();
    app.record_remediation();

    let removed = events(&app, "key.credential_removed");
    assert_eq!(removed.len(), 1, "{removed:?}");
    assert!(removed[0].contains(CREDENTIAL), "{}", removed[0]);
    assert!(
        yk_dist_manager::incident::is_settled(
            &app.lifecycle.dependencies,
            &app.lifecycle.remediations
        ),
        "nothing is outstanding once both are recorded"
    );

    // Then the incident closes without a note, because nothing is owed
    app.close_key_incident(incident.id);
    assert_eq!(app.lifecycle.error, None);
    assert!(app.open_incident().is_none());
    assert_eq!(events(&app, "key.incident_closed").len(), 1);

    // When the key turns up, and a factory reset from *Attached now* clears two of
    // its three applets — the PIV one refused, which is the case the outcomes exist
    // to distinguish from a reset that worked
    app.reset.outcomes = vec![
        Outcome {
            applet: Applet::Fido2,
            transport: "ykman",
            status: ResetStatus::Done,
            detail: "reset".into(),
        },
        Outcome {
            applet: Applet::Otp,
            transport: "ykman",
            status: ResetStatus::Skipped,
            detail: "both slots were already empty".into(),
        },
        Outcome {
            applet: Applet::Piv,
            transport: "native",
            status: ResetStatus::Failed,
            detail: "the card stopped answering".into(),
        },
    ];
    app.record_reset_sanitisation(SERIAL);

    // Then the reset records its own sanitisation — for the applets that answered,
    // and only those
    let from_reset = events(&app, "key.sanitised");
    assert_eq!(from_reset.len(), 1, "{from_reset:?}");
    assert!(
        from_reset[0].contains("fido2+otp") && from_reset[0].contains("source=reset"),
        "the refused applet is not recorded as clean: {}",
        from_reset[0]
    );
    assert_eq!(
        app.lifecycle.sanitisation.outstanding,
        vec![Applet::Piv],
        "and the gate stays closed for the one that refused"
    );

    // When the PIV applet is reset on a bench with `ykman`, and the operator
    // records that
    app.lifecycle.sanitised_applets.clear();
    app.toggle_sanitised_applet(Applet::Piv, true);
    app.lifecycle.reference = "reset with ykman on the bench".into();
    app.record_manual_sanitisation();

    // Then the sanitisation is recorded, saying it was the operator's word rather
    // than this tool's own reset
    assert_eq!(app.lifecycle.error, None);
    let sanitised = events(&app, "key.sanitised");
    assert_eq!(sanitised.len(), 2, "{sanitised:?}");
    assert!(
        sanitised
            .iter()
            .any(|detail| detail.contains("subject=piv ") && detail.contains("source=operator")),
        "{sanitised:?}"
    );
    assert!(app.lifecycle.sanitisation.is_clear());

    // And the key may now go back into stock — the same call that was refused
    app.store
        .as_ref()
        .unwrap()
        .set_key_status(SERIAL, KeyStatus::InStock)
        .expect("a sanitised key may be reissued");
    assert_eq!(status_of(&app, SERIAL), KeyStatus::InStock);

    // When the key turns out to be faulty and goes back to the supplier
    app.lifecycle.rma_reference.clear();
    app.send_key_for_rma();
    assert!(
        app.lifecycle
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("reference"),
        "an RMA nobody can quote is an RMA nobody can chase: {:?}",
        app.lifecycle.error
    );

    app.lifecycle.rma_reference = "RMA-4412".into();
    app.lifecycle.rma_fault = "will not enumerate on any port".into();
    app.send_key_for_rma();
    assert_eq!(app.lifecycle.error, None);
    assert_eq!(events(&app, "key.rma.sent").len(), 1);
    let case = app.lifecycle.rma.first().cloned().expect("a case is open");

    // Then a replacement nobody has recorded cannot be linked…
    app.lifecycle.rma_replacement = "99999999".into();
    app.record_rma_replacement(case.id);
    assert!(
        app.lifecycle
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("not in the inventory"),
        "{:?}",
        app.lifecycle.error
    );

    // …and the one that was is, keeping its own row and its own history
    app.lifecycle.rma_replacement = REPLACEMENT.to_string();
    app.record_rma_replacement(case.id);
    assert_eq!(app.lifecycle.error, None);
    let replaced = events(&app, "key.rma.replaced");
    assert_eq!(replaced.len(), 1, "{replaced:?}");
    assert!(
        replaced[0].contains("RMA-4412") && replaced[0].contains(&REPLACEMENT.to_string()),
        "{}",
        replaced[0]
    );
    assert_eq!(
        app.lifecycle.rma[0].replacement_serial,
        Some(REPLACEMENT),
        "the case says which key replaced this one"
    );
    assert_eq!(
        status_of(&app, REPLACEMENT),
        KeyStatus::InStock,
        "the replacement is an ordinary key in stock, not a copy of the old row"
    );

    // And the whole trail verifies: every entry above is chained, and none of them
    // could be edited or removed on the way
    assert!(
        app.store.as_ref().unwrap().verify_audit().unwrap() > 0,
        "the chain verifies from the genesis entry"
    );
}
