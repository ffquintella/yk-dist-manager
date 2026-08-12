//! Behaviour tests for **managing bootstrap templates**: a unit adds its own
//! procedure, changes one, withdraws one, and removes one it typed by mistake.
//!
//! The invariant behind all of them: what a bootstrap run recorded stays
//! explainable. An edit never overwrites the version a run applied, and a version
//! a run refers to cannot be deleted — only retired.
//!
//! See `features/bootstrap-templates.md`.

use yk_dist_manager::domain::{BootstrapRun, StepKind, StepOutcome};
use yk_dist_manager::store::{Store, StoreError};
use yk_dist_manager::template::{
    BootstrapTemplate, TemplateDraft, TemplateStep, edit_audit_entry, latest_of, latest_per_id,
};

/// A procedure of a unit's own: two steps, nothing shipped by this build.
fn unit_template() -> BootstrapTemplate {
    let mut draft = TemplateDraft::blank();
    draft.id = "contractor".into();
    draft.name = "Contractor keys".into();
    draft.description = "FIDO2 PIN and the on-key credential for {{holder.email}}. No PIV.".into();
    draft.add_step(StepKind::Fido2Pin).unwrap();
    draft.add_step(StepKind::Fido2Credential).unwrap();
    draft.to_template().unwrap()
}

fn run_of(template: &BootstrapTemplate) -> BootstrapRun {
    BootstrapRun::new(
        20_423_633,
        None,
        template.id.clone(),
        template.version.clone(),
        "felipe",
        vec![StepOutcome::planned(
            "fido2-pin",
            StepKind::Fido2Pin,
            "[manual] planned",
        )],
    )
}

/// A built-in as an older build wrote it: version 1, with the forced PIN change
/// ahead of the credential — an ordering that cannot complete on hardware, because
/// the mark takes the PIN out of use.
fn broken_v1(mut template: BootstrapTemplate) -> BootstrapTemplate {
    template.version = "1".into();
    let marker = template
        .steps
        .iter()
        .position(|s| s.kind == StepKind::Fido2ForcePinChange)
        .unwrap();
    let step = template.steps.remove(marker);
    let credential = template
        .steps
        .iter()
        .position(|s| s.kind == StepKind::Fido2Credential)
        .unwrap();
    template.steps.insert(credential, step);
    assert!(
        template.validate().is_err(),
        "the fixture has to be the broken ordering, or this proves nothing"
    );
    template
}

#[test]
fn scenario_a_register_holding_the_broken_procedure_is_offered_the_corrected_one() {
    // A real failure, seen on an installation: the database was seeded by a build
    // whose `org-standard v1` marked the key for a forced PIN change *before*
    // creating the resident credential. Seeding deliberately never overwrites a
    // stored (id, version), so correcting the constructor alone left the broken
    // procedure in place and the operator met the refusal in the editor.
    //
    // Every built-in that sets a FIDO2 PIN is checked, not just the standard one.
    // The first pass of this fix bumped `org-standard` alone, and `fido-only` —
    // whose steps are a filtered view of it, so its ordering was corrected in the
    // code for free — stayed at v1 and kept the broken ordering in every register
    // already seeded. Same bug, second id, and it was met in the editor the same
    // way.
    for builtin in BootstrapTemplate::builtin() {
        let store = Store::open_in_memory().unwrap();
        let id = builtin.id.clone();
        let corrected = builtin.version.clone();

        // Given a register already holding the broken v1
        store.upsert_template(&broken_v1(builtin)).unwrap();

        // When the application seeds its built-ins, as it does on every open
        store.seed_builtin_templates().unwrap();

        // Then the broken version is still on record — a run may have recorded it,
        // and rewriting what a version *said* would rewrite what a key was told to
        // have applied to it
        let versions = store.template_versions(&id).unwrap();
        assert!(versions.contains(&"1".to_string()), "{id}: {versions:?}");

        // And the corrected version is there beside it
        assert!(versions.contains(&corrected), "{id}: {versions:?}");

        // And the wizard is offered the corrected one
        let offered = latest_per_id(&store.templates().unwrap());
        let latest = offered
            .iter()
            .find(|t| t.id == id)
            .unwrap_or_else(|| panic!("{id} is offered"));
        assert_eq!(latest.version, corrected, "{id}");
        assert!(
            latest.validate().is_ok(),
            "{id}: what the wizard offers must be a procedure that can actually complete"
        );

        let ordering: Vec<&str> = latest
            .steps
            .iter()
            .filter(|s| {
                matches!(
                    s.kind,
                    StepKind::Fido2Credential | StepKind::Fido2ForcePinChange
                )
            })
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(
            ordering,
            vec!["fido2-credential", "fido2-force-pin-change"],
            "{id}: the credential must be created before the PIN is taken out of use"
        );
    }
}

#[test]
fn scenario_a_unit_adds_its_own_template_and_the_wizard_offers_it() {
    // Given a database with the templates this build ships
    let store = Store::open_in_memory().unwrap();
    store.seed_builtin_templates().unwrap();

    // When the operator stores a procedure of their own
    let stored = store.save_template_version(&unit_template()).unwrap();

    // Then it is version 1, and the wizard offers it alongside the built-ins
    assert_eq!(stored.version, "1");
    let offered = latest_per_id(&store.templates().unwrap());
    assert!(
        offered.iter().any(|t| t.id == "contractor"),
        "the new template must be offered: {:?}",
        offered.iter().map(|t| &t.id).collect::<Vec<_>>()
    );
    assert_eq!(offered.len(), 3, "two built-ins plus the new one");
}

#[test]
fn scenario_an_edit_is_stored_as_a_new_version_and_the_old_one_stays_readable() {
    // Given a stored template that a bootstrap run has already applied
    let store = Store::open_in_memory().unwrap();
    let first = store.save_template_version(&unit_template()).unwrap();
    store.insert_run(&run_of(&first)).unwrap();

    // When the operator adds a step and saves
    let mut draft = TemplateDraft::from_template(&first, true);
    draft.add_step(StepKind::Verify).unwrap();
    let second = store
        .save_template_version(&draft.to_template().unwrap())
        .unwrap();

    // Then the new version is the one new runs get…
    assert_eq!(second.version, "2");
    assert_eq!(
        latest_of(&store.templates().unwrap(), "contractor")
            .unwrap()
            .version,
        "2"
    );
    // …and the version the run recorded is still there, unchanged
    let catalogue = store.template_catalogue().unwrap();
    let applied = catalogue
        .iter()
        .find(|s| s.template.id == "contractor" && s.template.version == "1")
        .expect("the version a run applied stays on record");
    assert_eq!(applied.template.steps.len(), first.steps.len());
    assert_eq!(applied.runs, 1, "the catalogue says what refers to it");
}

#[test]
fn scenario_the_version_number_comes_from_the_database_not_from_the_draft() {
    // Two operators editing the same template must not both produce "version 2".
    let store = Store::open_in_memory().unwrap();
    store.save_template_version(&unit_template()).unwrap();

    let mut stale = unit_template();
    stale.version = "1".into(); // what the screen still shows
    let stored = store.save_template_version(&stale).unwrap();
    assert_eq!(stored.version, "2");

    let stored = store.save_template_version(&stale).unwrap();
    assert_eq!(stored.version, "3");
    assert_eq!(store.template_versions("contractor").unwrap().len(), 3);
}

#[test]
fn scenario_a_template_typed_by_mistake_is_removed() {
    // Given a template nothing refers to
    let store = Store::open_in_memory().unwrap();
    let mut typo = unit_template();
    typo.id = "contarctor".into();
    let stored = store.save_template_version(&typo).unwrap();

    // When it is removed
    let removed = store
        .delete_template(&stored.id, &stored.version)
        .expect("a template nothing refers to can be removed");

    // Then the row is gone and the caller learns what it was, for the audit entry
    assert_eq!(removed.runs, 0);
    assert!(removed.audit_detail().contains("id=contarctor"));
    assert!(
        store
            .template_catalogue()
            .unwrap()
            .iter()
            .all(|s| s.template.id != "contarctor")
    );
}

#[test]
fn scenario_a_template_a_run_recorded_cannot_be_removed() {
    // Given a template a bootstrap run applied
    let store = Store::open_in_memory().unwrap();
    let stored = store.save_template_version(&unit_template()).unwrap();
    store.insert_run(&run_of(&stored)).unwrap();

    // When the operator tries to delete that version
    let error = store
        .delete_template(&stored.id, &stored.version)
        .expect_err("a run refers to it");

    // Then it is refused, and the refusal names retirement — a run saying it
    // applied `contractor v1` with no `contractor v1` to look up is not a record
    match error {
        StoreError::TemplateInUse {
            id,
            version,
            reason,
        } => {
            assert_eq!(id, "contractor");
            assert_eq!(version, "1");
            assert!(reason.contains("retire"), "got: {reason}");
        }
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(store.template_catalogue().unwrap().len(), 1, "still there");
}

#[test]
fn scenario_a_retired_template_is_no_longer_offered_but_stays_on_record() {
    // Given a template a run applied
    let store = Store::open_in_memory().unwrap();
    let stored = store.save_template_version(&unit_template()).unwrap();
    store.insert_run(&run_of(&stored)).unwrap();

    // When it is retired instead of removed
    let retired = store.retire_template(&stored.id, &stored.version).unwrap();
    assert_eq!(retired.runs, 1);

    // Then the wizard is not offered it…
    assert!(
        store.templates().unwrap().is_empty(),
        "a retired template is not offered"
    );
    // …and the record still explains what the run applied
    let catalogue = store.template_catalogue().unwrap();
    assert_eq!(catalogue.len(), 1);
    assert!(catalogue[0].is_retired());
    assert_eq!(catalogue[0].template.steps.len(), stored.steps.len());
}

#[test]
fn scenario_retiring_a_builtin_survives_the_next_time_the_database_is_opened() {
    // Given a database with the built-ins seeded
    let store = Store::open_in_memory().unwrap();
    store.seed_builtin_templates().unwrap();

    // When the operator withdraws the FIDO-only procedure. The version comes from
    // the built-in itself, so a future bump does not quietly turn this into a
    // retirement of a version nobody is offered.
    let fido = BootstrapTemplate::fido_only();
    store.retire_template(&fido.id, &fido.version).unwrap();

    // Then re-seeding — which happens on every open — does not bring it back
    assert_eq!(store.seed_builtin_templates().unwrap(), 0);
    assert!(
        store
            .templates()
            .unwrap()
            .iter()
            .all(|t| t.id != "fido-only"),
        "a retirement the application undoes on the next launch is not a retirement"
    );
    assert!(
        store
            .template_catalogue()
            .unwrap()
            .iter()
            .any(|s| s.template.id == "fido-only" && s.is_retired())
    );
}

#[test]
fn scenario_a_builtin_cannot_be_deleted_because_seeding_would_bring_it_back() {
    let store = Store::open_in_memory().unwrap();
    store.seed_builtin_templates().unwrap();

    let fido = BootstrapTemplate::fido_only();
    let error = store
        .delete_template(&fido.id, &fido.version)
        .expect_err("deleting a built-in would only look like it worked");
    assert!(
        error.to_string().contains("retire"),
        "the refusal must name the operation that lasts: {error}"
    );
}

#[test]
fn scenario_a_retired_template_can_be_put_back_in_use() {
    let store = Store::open_in_memory().unwrap();
    let stored = store.save_template_version(&unit_template()).unwrap();
    store.retire_template(&stored.id, &stored.version).unwrap();
    assert!(store.templates().unwrap().is_empty());

    store
        .reinstate_template(&stored.id, &stored.version)
        .unwrap();
    assert_eq!(store.templates().unwrap().len(), 1);
    assert!(!store.template_catalogue().unwrap()[0].is_retired());
}

#[test]
fn scenario_a_template_that_cannot_be_planned_never_reaches_the_database() {
    // Given a draft whose certificate subject uses a variable nothing supplies
    let store = Store::open_in_memory().unwrap();
    let mut draft = unit_template();
    draft.steps.push(
        TemplateStep::for_kind(StepKind::PivCsr, "piv-csr")
            .with_param("subject", "CN={{holder.shoe_size}}"),
    );

    // When the operator saves it
    let error = store
        .save_template_version(&draft)
        .expect_err("an unknown variable is refused at the desk");

    // Then nothing was stored, and the message names the variable
    assert!(
        error.to_string().contains("holder.shoe_size"),
        "got: {error}"
    );
    assert!(store.template_catalogue().unwrap().is_empty());
}

#[test]
fn scenario_a_template_with_no_steps_is_refused() {
    let store = Store::open_in_memory().unwrap();
    let mut empty = unit_template();
    empty.steps.clear();
    assert!(store.save_template_version(&empty).is_err());
    assert!(store.template_catalogue().unwrap().is_empty());
}

#[test]
fn scenario_retiring_something_that_is_not_there_is_reported_not_ignored() {
    let store = Store::open_in_memory().unwrap();
    assert!(matches!(
        store.retire_template("contractor", "1"),
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store.delete_template("contractor", "1"),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn scenario_every_change_to_a_template_is_audited() {
    // Given a database with the built-ins seeded
    let store = Store::open_in_memory().unwrap();
    store.seed_builtin_templates().unwrap();

    // When a template is added, edited, retired, reinstated and removed — each
    // audited the way the Templates screen does it
    let added = store.save_template_version(&unit_template()).unwrap();
    let (event, target, details) = edit_audit_entry(&added, None);
    assert_eq!(event, "template.created");
    store
        .append_audit("felipe", event, &target, &details)
        .unwrap();

    let mut draft = TemplateDraft::from_template(&added, true);
    draft.add_step(StepKind::Verify).unwrap();
    let edited = store
        .save_template_version(&draft.to_template().unwrap())
        .unwrap();
    let (event, target, details) = edit_audit_entry(&edited, Some(&added.version));
    assert_eq!(event, "template.changed");
    store
        .append_audit("felipe", event, &target, &details)
        .unwrap();

    let retired = store.retire_template(&added.id, &added.version).unwrap();
    store
        .append_audit(
            "felipe",
            "template.retired",
            &format!("template:{}", added.id),
            &retired.audit_detail(),
        )
        .unwrap();
    let reinstated = store.reinstate_template(&added.id, &added.version).unwrap();
    store
        .append_audit(
            "felipe",
            "template.reinstated",
            &format!("template:{}", added.id),
            &reinstated.audit_detail(),
        )
        .unwrap();
    let removed = store.delete_template(&edited.id, &edited.version).unwrap();
    store
        .append_audit(
            "felipe",
            "template.removed",
            &format!("template:{}", edited.id),
            &removed.audit_detail(),
        )
        .unwrap();

    // Then all five events are in the chain, the chain verifies, and no entry
    // carries the procedure itself — only its shape, because an audit entry can
    // never be corrected
    let entries = store.audit_entries(20).unwrap();
    for expected in [
        "template.created",
        "template.changed",
        "template.retired",
        "template.reinstated",
        "template.removed",
    ] {
        let entry = entries
            .iter()
            .find(|e| e.event == expected)
            .unwrap_or_else(|| panic!("{expected} must be audited"));
        assert_eq!(entry.target, "template:contractor");
        assert_eq!(entry.actor, "felipe");
        assert!(
            !entry.details.contains("{{"),
            "{expected} quoted template text into the immutable record: {}",
            entry.details
        );
    }
    store.verify_audit().unwrap();
}

// --------------------------------------------------- sharing a procedure (4)

/// A procedure signed by a key a test holds. Not a credential: the private half
/// exists for the length of this call and protects nothing.
fn signed_by(template: &BootstrapTemplate, key_id: &str, seed: [u8; 32]) -> BootstrapTemplate {
    use yk_dist_manager::template::TemplateSignature;
    use yk_dist_manager::template::signing::{ALGORITHM, canonical_bytes};

    let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
    let signature = ed25519_dalek::Signer::sign(&signing, &canonical_bytes(template));
    let mut signed = template.clone();
    signed.signature = Some(TemplateSignature {
        key_id: key_id.into(),
        algorithm: ALGORITHM.into(),
        signature: hex::encode(signature.to_bytes()),
    });
    signed
}

fn trusted(key_id: &str, seed: [u8; 32]) -> Vec<yk_dist_manager::template::TemplateKey> {
    let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
    vec![yk_dist_manager::template::TemplateKey {
        id: key_id.into(),
        public_key: hex::encode(signing.verifying_key().to_bytes()),
        comment: "the test's key".into(),
    }]
}

#[test]
fn scenario_a_procedure_crosses_between_two_registers_through_a_file() {
    use yk_dist_manager::store::TemplateImport;
    use yk_dist_manager::template::TemplateFile;

    // Given a unit that has written its own procedure and edited it once, so its
    // register holds two versions
    let source = Store::open_in_memory().unwrap();
    let v1 = source.save_template_version(&unit_template()).unwrap();
    let mut edited = TemplateDraft::from_template(&v1, true);
    edited.steps[0].params_text = "min_length = 8\nsource = operator-entered".into();
    let v2 = source
        .save_template_version(&edited.to_template().unwrap())
        .unwrap();
    assert_eq!(v2.version, "2");

    // When it exports the newest version and another unit imports the file
    let exported = TemplateFile::of(&v2, chrono::Utc::now()).to_json();
    let target = Store::open_in_memory().unwrap();
    let read = TemplateFile::from_json(&exported).expect("the file reads");
    let outcome = target.import_template(&read.template).unwrap();

    // Then the receiving register holds the procedure exactly — step for step,
    // parameter for parameter — under a version *it* assigned
    let TemplateImport::Stored { template, previous } = outcome else {
        panic!("a register that did not have it must store it");
    };
    assert_eq!(previous, None, "this register had no version of that id");
    assert_eq!(template.version, "1", "the receiver numbers it, from 1");
    assert_eq!(template.id, v2.id);
    assert_eq!(template.steps, v2.steps);
    assert_eq!(
        yk_dist_manager::template::signing::fingerprint(&template),
        yk_dist_manager::template::signing::fingerprint(&v2),
        "the same procedure has the same fingerprint on both sides"
    );

    // And importing the same file again stores nothing: an operator will do this,
    // from the mail and then from the share, and two identical versions would be
    // the catalogue growing a row per attempt
    let again = target.import_template(&read.template).unwrap();
    assert!(matches!(
        again,
        TemplateImport::AlreadyPresent { version, .. } if version == "1"
    ));
    assert_eq!(target.template_versions(&template.id).unwrap().len(), 1);

    // And an edit on top of an imported procedure is a new version here too, so
    // the imported one stays exactly as it arrived
    let mut local = TemplateDraft::from_template(&template, true);
    local.description = "Ours, with a local note".into();
    let v2_here = target
        .save_template_version(&local.to_template().unwrap())
        .unwrap();
    assert_eq!(v2_here.version, "2");
    assert_eq!(
        latest_of(&target.templates().unwrap(), &template.id)
            .unwrap()
            .description,
        "Ours, with a local note"
    );
}

#[test]
fn scenario_an_imported_procedure_that_differs_becomes_a_new_version_not_an_overwrite() {
    use yk_dist_manager::store::TemplateImport;

    // Given a register with a procedure a run has already recorded
    let store = Store::open_in_memory().unwrap();
    let stored = store.save_template_version(&unit_template()).unwrap();
    store.insert_run(&run_of(&stored)).unwrap();

    // When a file arrives carrying the same id with a different procedure —
    // another unit's edit of the same template
    let mut theirs = stored.clone();
    theirs.steps[0]
        .params
        .insert("min_length".into(), "8".into());
    let outcome = store.import_template(&theirs).unwrap();

    // Then it is stored beside the version the run recorded, never over it
    let TemplateImport::Stored { template, previous } = outcome else {
        panic!("a different procedure is a new version");
    };
    assert_eq!(previous.as_deref(), Some("1"));
    assert_eq!(template.version, "2");

    let v1 = store.stored_template(&stored.id, "1").unwrap();
    assert_eq!(
        v1.template.steps[0]
            .params
            .get("min_length")
            .map(String::as_str),
        Some("6"),
        "the version the run recorded must be untouched"
    );
    assert_eq!(v1.runs, 1);
}

#[test]
fn scenario_a_file_that_cannot_be_planned_never_reaches_the_database() {
    use yk_dist_manager::template::{PortableError, TemplateFile};

    // A file is untrusted input: hand-edited, mailed, or written by something
    // else entirely. The gate that keeps an unplannable procedure out of the
    // register has to apply to it exactly as it applies to the editor.
    let store = Store::open_in_memory().unwrap();
    let mut broken = unit_template();
    broken
        .steps
        .iter_mut()
        .find(|s| s.kind == StepKind::Fido2Credential)
        .unwrap()
        .params
        .insert("rp_id".into(), "{{org.department}}".into());

    let raw = TemplateFile::of(&broken, chrono::Utc::now()).to_json();
    assert!(matches!(
        TemplateFile::from_json(&raw),
        Err(PortableError::Refused(_))
    ));

    // And the store refuses it too, so neither door depends on the other
    // remembering to check.
    assert!(store.import_template(&broken).is_err());
    assert!(store.template_catalogue().unwrap().is_empty());
}

// ------------------------------------------- signing and verification (5)

#[test]
fn scenario_a_signed_procedure_survives_the_journey_and_a_tampered_one_does_not() {
    use yk_dist_manager::template::TemplateFile;
    use yk_dist_manager::template::signing::{Trust, verify};

    let keys = trusted("esi-templates-2026", [42u8; 32]);

    // Given a procedure signed by the organisation's key, in a file
    let signed = signed_by(&unit_template(), "esi-templates-2026", [42u8; 32]);
    let file = TemplateFile::of(&signed, chrono::Utc::now()).to_json();

    // When a unit imports it
    let store = Store::open_in_memory().unwrap();
    let read = TemplateFile::from_json(&file).unwrap();
    let outcome = store.import_template(&read.template).unwrap();
    let stored = match outcome {
        yk_dist_manager::store::TemplateImport::Stored { template, .. } => template,
        other => panic!("expected a store, got {other:?}"),
    };

    // Then the signature still verifies *after* the register renumbered it, which
    // is the whole reason the version is not part of what is signed
    assert_eq!(stored.version, "1");
    assert_eq!(
        verify(&stored, &keys),
        Trust::Signed {
            key_id: "esi-templates-2026".into()
        }
    );

    // And it still verifies when read back out of the database, so the signature
    // survives serialisation as well as transport
    let reloaded = latest_of(&store.templates().unwrap(), &stored.id)
        .cloned()
        .unwrap();
    assert!(verify(&reloaded, &keys).is_verified());

    // When somebody changes one parameter of the stored procedure — the attack
    // this feature exists for: a template decides what is written to a key
    let mut tampered = reloaded.clone();
    tampered
        .steps
        .iter_mut()
        .find(|s| s.kind == StepKind::Fido2Pin)
        .unwrap()
        .params
        .insert("min_length".into(), "4".into());
    store.upsert_template(&tampered).unwrap();

    // Then it no longer verifies, and the verdict says it was altered rather than
    // that it was never signed
    let altered = latest_of(&store.templates().unwrap(), &stored.id)
        .cloned()
        .unwrap();
    match verify(&altered, &keys) {
        Trust::Invalid { key_id, .. } => assert_eq!(key_id, "esi-templates-2026"),
        other => panic!("a tampered procedure must not verify: {other:?}"),
    }
}

#[test]
fn scenario_editing_a_signed_procedure_produces_an_unsigned_version() {
    use yk_dist_manager::template::signing::verify;

    // The cost of the feature, made explicit: a unit that edits a signed procedure
    // has an unsigned one until whoever holds the key signs it again. Anything else
    // would mean the application could sign, which would mean it held the key.
    let keys = trusted("esi", [7u8; 32]);
    let store = Store::open_in_memory().unwrap();
    let signed = signed_by(&unit_template(), "esi", [7u8; 32]);
    let stored = match store.import_template(&signed).unwrap() {
        yk_dist_manager::store::TemplateImport::Stored { template, .. } => template,
        other => panic!("{other:?}"),
    };
    assert!(verify(&stored, &keys).is_verified());

    // When the operator opens it and changes a step
    let mut draft = TemplateDraft::from_template(&stored, true);
    draft.steps[0].params_text = "min_length = 8\nsource = operator-entered".into();
    // The draft is dirty — and only because of the edit, not because loading a
    // signed template looks like one.
    assert!(draft.is_dirty(Some(&stored)));
    assert!(
        !TemplateDraft::from_template(&stored, true).is_dirty(Some(&stored)),
        "opening a signed template must not read as an edit"
    );
    let saved = store
        .save_template_version(&draft.to_template().unwrap())
        .unwrap();

    // Then the new version is unsigned, and the signed one is still there
    assert_eq!(saved.version, "2");
    assert_eq!(saved.signature, None);
    assert_eq!(
        verify(&saved, &keys),
        yk_dist_manager::template::Trust::Unsigned
    );
    assert!(
        verify(
            &store.stored_template(&stored.id, "1").unwrap().template,
            &keys
        )
        .is_verified()
    );
}

#[test]
fn scenario_a_duplicated_procedure_does_not_inherit_the_signature() {
    // A duplicate is a different procedure — the id is part of what is signed —
    // so carrying the signature over would produce something that reads as
    // *altered*, which is a lie about a template nobody has tampered with.
    let signed = signed_by(&unit_template(), "esi", [3u8; 32]);
    let copy = signed.duplicated_as("contractor-copy", "Contractor keys (copy)");
    assert_eq!(copy.signature, None);
    assert_eq!(
        yk_dist_manager::template::signing::verify(&copy, &trusted("esi", [3u8; 32])),
        yk_dist_manager::template::Trust::Unsigned
    );
}

// ------------------------------------------------ comparing two versions (6)

#[test]
fn scenario_an_operator_asks_what_changed_since_the_batch_they_shipped() {
    use yk_dist_manager::template::diff::{Change, diff};

    // Given a register where the procedure has moved on twice since a run
    let store = Store::open_in_memory().unwrap();
    let v1 = store.save_template_version(&unit_template()).unwrap();
    store.insert_run(&run_of(&v1)).unwrap();

    let mut draft = TemplateDraft::from_template(&v1, true);
    draft.steps[0].params_text = "min_length = 8\nsource = operator-entered".into();
    draft.add_step(StepKind::Verify).unwrap();
    draft.move_step(2, true);
    let v2 = store
        .save_template_version(&draft.to_template().unwrap())
        .unwrap();

    // When the operator compares the version that run recorded with the newest
    let d = diff(&v1, &v2);

    // Then every kind of change is named, with the step and both values
    assert!(!d.is_identical());
    assert_eq!(d.from, "1");
    assert_eq!(d.to, "2");
    assert_eq!(d.count(Change::Added), 1, "{:#?}", d.lines);
    assert!(d.count(Change::Moved) >= 1, "{:#?}", d.lines);

    let changed = d
        .changes()
        .find(|l| l.what.contains("min_length"))
        .expect("the PIN length changed");
    assert_eq!(changed.before, "6");
    assert_eq!(changed.after, "8");

    // And the summary is one line somebody can put in a ticket
    assert!(d.summary().starts_with("1 -> 2:"), "{}", d.summary());
}
