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

#[test]
fn scenario_a_register_holding_the_broken_procedure_is_offered_the_corrected_one() {
    // A real failure, seen on an installation: the database was seeded by a build
    // whose `org-standard v1` marked the key for a forced PIN change *before*
    // creating the resident credential — an ordering that cannot complete on
    // hardware, because the mark takes the PIN out of use. Seeding deliberately
    // never overwrites a stored (id, version), so correcting the constructor
    // alone left the broken procedure in place and the operator met the refusal
    // in the editor.
    let store = Store::open_in_memory().unwrap();

    // Given a register already holding the broken v1, as an older build wrote it
    let mut broken = BootstrapTemplate::org_standard();
    broken.version = "1".into();
    let marker = broken
        .steps
        .iter()
        .position(|s| s.kind == StepKind::Fido2ForcePinChange)
        .unwrap();
    let step = broken.steps.remove(marker);
    let credential = broken
        .steps
        .iter()
        .position(|s| s.kind == StepKind::Fido2Credential)
        .unwrap();
    broken.steps.insert(credential, step);
    assert!(
        broken.validate().is_err(),
        "the fixture has to be the broken ordering, or this proves nothing"
    );
    store.upsert_template(&broken).unwrap();

    // When the application seeds its built-ins, as it does on every open
    store.seed_builtin_templates().unwrap();

    // Then the broken version is still on record — a run may have recorded it,
    // and rewriting what a version *said* would rewrite what a key was told to
    // have applied to it
    let versions = store.template_versions("org-standard").unwrap();
    assert!(versions.contains(&"1".to_string()), "{versions:?}");

    // And the corrected version is there beside it
    assert!(versions.contains(&"2".to_string()), "{versions:?}");

    // And the wizard is offered the corrected one
    let offered = latest_per_id(&store.templates().unwrap());
    let standard = offered
        .iter()
        .find(|t| t.id == "org-standard")
        .expect("the standard procedure is offered");
    assert_eq!(standard.version, "2");
    assert!(
        standard.validate().is_ok(),
        "what the wizard offers must be a procedure that can actually complete"
    );

    let ordering: Vec<&str> = standard
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
        "the credential must be created before the PIN is taken out of use"
    );
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

    // When the operator withdraws the FIDO-only procedure
    store.retire_template("fido-only", "1").unwrap();

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

    let error = store
        .delete_template("fido-only", "1")
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
