//! Unit tests for template rendering and command planning.
//!
//! The key invariant: **no secret ever appears in a rendered plan**.

use yk_dist_manager::domain::{Holder, StepKind};
use yk_dist_manager::template::{
    Arg, BootstrapTemplate, MAX_STEPS, RenderContext, StoredTemplate, TemplateDraft, TemplateError,
    TemplateStep, Transport, check_id, edit_audit_entry, latest_of, latest_per_id, native_op,
    parse_params, plan, render, unique_id, versions_of,
};

fn ctx() -> RenderContext {
    let holder = Holder::new("Ana Silva", "ana.silva@example.org", "ESI", "12345").unwrap();
    let mut ctx = RenderContext::for_holder(&holder, 20_423_633, "felipe", "Example Organisation");
    ctx.date = "2026-08-10".into();
    ctx
}

#[test]
fn renders_known_variables() {
    let out = render("subject for {{holder.name}} <{{holder.email}}>", &ctx()).unwrap();
    assert_eq!(out, "subject for Ana Silva <ana.silva@example.org>");
}

#[test]
fn whitespace_inside_braces_is_tolerated() {
    assert_eq!(render("{{  holder.unit  }}", &ctx()).unwrap(), "ESI");
}

#[test]
fn unknown_variable_is_an_error_not_an_empty_string() {
    let err = render("{{holder.phone}}", &ctx()).unwrap_err();
    assert_eq!(err, TemplateError::UnknownVariable("holder.phone".into()));
}

#[test]
fn unterminated_placeholder_is_an_error() {
    assert_eq!(
        render("{{holder.name", &ctx()).unwrap_err(),
        TemplateError::Unterminated
    );
}

#[test]
fn text_without_placeholders_passes_through() {
    assert_eq!(render("plain text", &ctx()).unwrap(), "plain text");
}

#[test]
fn every_documented_variable_resolves() {
    for name in RenderContext::VARIABLES {
        let template = format!("{{{{{name}}}}}");
        assert!(
            render(&template, &ctx()).is_ok(),
            "documented variable `{name}` does not resolve"
        );
    }
}

#[test]
fn builtin_templates_validate() {
    for template in BootstrapTemplate::builtin() {
        template
            .validate()
            .unwrap_or_else(|e| panic!("template {} is invalid: {e}", template.id));
    }
}

#[test]
fn duplicate_step_ids_are_rejected() {
    let mut template = BootstrapTemplate::org_standard();
    let first = template.steps[0].clone();
    template.steps.push(first);
    assert!(matches!(
        template.validate().unwrap_err(),
        TemplateError::DuplicateStepId(_)
    ));
}

#[test]
fn template_with_no_enabled_step_is_rejected() {
    let mut template = BootstrapTemplate::org_standard();
    for step in &mut template.steps {
        step.enabled = false;
    }
    assert_eq!(template.validate().unwrap_err(), TemplateError::Empty);
}

#[test]
fn plan_covers_every_enabled_step_in_order() {
    let template = BootstrapTemplate::org_standard();
    let commands = plan(&template, &ctx()).unwrap();
    assert_eq!(commands.len(), template.steps.len());
    for (command, step) in commands.iter().zip(&template.steps) {
        assert_eq!(command.step_id, step.id);
    }
}

#[test]
fn no_plan_output_can_leak_a_secret() {
    let commands = plan(&BootstrapTemplate::org_standard(), &ctx()).unwrap();
    for command in &commands {
        let rendered = format!(
            "{} {} {}",
            command.redacted_command(),
            command.transport_detail(),
            command.description
        );
        for forbidden in ["123456", "12345678", "010203040506"] {
            assert!(
                !rendered.contains(forbidden),
                "step {} leaked a secret-looking literal: {rendered}",
                command.step_id
            );
        }
        for arg in &command.args {
            if arg.is_secret() {
                let shown = arg.redacted();
                assert!(
                    shown.starts_with('<') && shown.ends_with('>'),
                    "secret arg rendered as `{shown}`"
                );
            }
        }
    }
}

#[test]
fn pin_carrying_steps_use_a_secret_placeholder() {
    let commands = plan(&BootstrapTemplate::org_standard(), &ctx()).unwrap();
    let fido_pin = commands
        .iter()
        .find(|c| c.kind == StepKind::Fido2Pin)
        .expect("FIDO2 PIN step is planned");
    assert!(fido_pin.carries_secret());
    assert!(
        fido_pin.args.contains(&Arg::Secret("FIDO2-PIN")),
        "expected a FIDO2-PIN placeholder, got {:?}",
        fido_pin.args
    );
}

#[test]
fn the_certificate_subject_is_bound_to_the_holder() {
    let commands = plan(&BootstrapTemplate::org_standard(), &ctx()).unwrap();
    let csr = commands
        .iter()
        .find(|c| c.kind == StepKind::PivCsr)
        .expect("CSR step is planned");
    let rendered = csr.redacted_command();
    assert!(rendered.contains("CN=Ana Silva"), "got: {rendered}");
    assert!(rendered.contains("OU=ESI"), "got: {rendered}");
    // The e-mail must be carried as a SAN, so it shows up in the note, not the DN.
    assert!(
        csr.note
            .as_deref()
            .unwrap_or("")
            .contains("ana.silva@example.org"),
        "the SAN e-mail must be stated on the step"
    );
}

#[test]
fn the_key_serial_selects_the_device_on_every_command() {
    let commands = plan(&BootstrapTemplate::org_standard(), &ctx()).unwrap();
    for command in commands.iter().filter(|c| c.program.is_some()) {
        let rendered = command.redacted_command();
        assert!(
            rendered.contains("--device 20423633"),
            "step {} does not pin the device: {rendered}",
            command.step_id
        );
    }
}

#[test]
fn fido_only_template_omits_piv_and_otp() {
    let commands = plan(&BootstrapTemplate::fido_only(), &ctx()).unwrap();
    assert!(commands.iter().all(|c| !matches!(
        c.kind,
        StepKind::PivKeygen | StepKind::PivCsr | StepKind::OtpAccessCode
    )));
    assert!(commands.iter().any(|c| c.kind == StepKind::Fido2Credential));
}

#[test]
fn the_standard_template_forces_the_holder_to_change_the_transport_pin() {
    // Custody model B: the operator's PIN is a transport PIN, so the template
    // must mark the key for a mandatory change.
    let commands = plan(&BootstrapTemplate::org_standard(), &ctx()).unwrap();
    let forced = commands
        .iter()
        .find(|c| c.kind == StepKind::Fido2ForcePinChange)
        .expect("the forced-change step is planned");

    assert!(
        forced
            .redacted_command()
            .contains("fido access force-change"),
        "got: {}",
        forced.redacted_command()
    );
    assert!(
        !forced.carries_secret(),
        "marking the PIN for change needs no secret"
    );
    let note = forced.note.as_deref().unwrap_or("");
    assert!(
        note.contains("5.7"),
        "the firmware gate must be stated: {note}"
    );
    assert!(
        note.contains("hand-over term"),
        "below 5.7 the fallback is procedural, and the note must say so: {note}"
    );
}

#[test]
fn the_fido_only_template_keeps_the_forced_change() {
    let commands = plan(&BootstrapTemplate::fido_only(), &ctx()).unwrap();
    assert!(
        commands
            .iter()
            .any(|c| c.kind == StepKind::Fido2ForcePinChange),
        "custody model B applies to every template that sets a FIDO2 PIN"
    );
}

#[test]
fn credential_registration_is_native_because_ykman_cannot_do_it() {
    let commands = plan(&BootstrapTemplate::org_standard(), &ctx()).unwrap();
    let credential = commands
        .iter()
        .find(|c| c.kind == StepKind::Fido2Credential)
        .unwrap();
    assert_eq!(credential.transport(), Transport::Native);
    assert!(credential.program.is_none(), "no ykman fallback exists");
    assert_eq!(
        credential.native.as_ref().unwrap().crate_name,
        "ctap-hid-fido2"
    );
}

#[test]
fn otp_steps_still_fall_back_to_ykman() {
    let commands = plan(&BootstrapTemplate::org_standard(), &ctx()).unwrap();
    let otp = commands
        .iter()
        .find(|c| c.kind == StepKind::OtpAccessCode)
        .unwrap();
    assert_eq!(otp.transport(), Transport::Ykman);
    assert!(!otp.native.as_ref().unwrap().available);
}

#[test]
fn every_step_kind_declares_a_native_operation() {
    for kind in [
        StepKind::Fido2Pin,
        StepKind::Fido2MinPinLength,
        StepKind::Fido2ForcePinChange,
        StepKind::Fido2Credential,
        StepKind::OtpAccessCode,
        StepKind::OtpSlotConfig,
        StepKind::PivPinPuk,
        StepKind::PivManagementKey,
        StepKind::PivKeygen,
        StepKind::PivCsr,
        StepKind::PivCertImport,
        StepKind::Verify,
    ] {
        let op = native_op(kind).unwrap_or_else(|| panic!("{kind:?} has no native mapping"));
        assert!(!op.crate_name.is_empty());
        assert!(!op.call.is_empty());
    }
}

#[test]
fn a_missing_parameter_is_reported_with_the_step_id() {
    let mut template = BootstrapTemplate::org_standard();
    let step = template
        .steps
        .iter_mut()
        .find(|s| s.kind == StepKind::PivKeygen)
        .unwrap();
    step.params.remove("algorithm");

    match plan(&template, &ctx()).unwrap_err() {
        TemplateError::MissingParam { step, param } => {
            assert_eq!(step, "piv-keygen");
            assert_eq!(param, "algorithm");
        }
        other => panic!("unexpected error: {other}"),
    }
}

// ---------------------------------------------------------------------------
// Editing a template: the draft model, the pre-save gate, and the catalogue
// helpers the Templates screen is built on (features/bootstrap-templates.md).
// ---------------------------------------------------------------------------

#[test]
fn the_sample_context_resolves_every_documented_variable() {
    // The draft check plans against this context, so a variable it cannot supply
    // would make a valid template unsaveable.
    let sample = RenderContext::sample();
    for name in RenderContext::VARIABLES {
        let rendered = render(&format!("{{{{{name}}}}}"), &sample)
            .unwrap_or_else(|e| panic!("sample context cannot supply `{name}`: {e}"));
        assert!(
            !rendered.trim().is_empty(),
            "`{name}` renders empty in the sample context"
        );
    }
}

#[test]
fn the_builtin_templates_pass_the_pre_save_gate() {
    for template in BootstrapTemplate::builtin() {
        template
            .check()
            .unwrap_or_else(|e| panic!("built-in {} would be refused: {e}", template.id));
    }
}

#[test]
fn parameters_round_trip_through_the_editor_text() {
    let step = TemplateStep::for_kind(StepKind::PivCsr, "piv-csr");
    let text = step.params_text();
    assert!(text.contains("slot = 9c"), "got: {text}");
    assert_eq!(parse_params("piv-csr", &text).unwrap(), step.params);
}

#[test]
fn a_parameter_line_that_is_not_a_pair_is_refused_naming_the_step() {
    match parse_params("piv-csr", "slot 9c").unwrap_err() {
        TemplateError::BadParam { step, line } => {
            assert_eq!(step, "piv-csr");
            assert_eq!(line, "slot 9c");
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn blank_lines_and_comments_are_ignored_in_parameters() {
    let params = parse_params("verify", "\n# what the run must confirm\nslot = 9c\n").unwrap();
    assert_eq!(params.len(), 1);
    assert_eq!(params.get("slot").unwrap(), "9c");
}

#[test]
fn a_parameter_name_must_be_lower_case_and_undecorated() {
    assert!(parse_params("s", "Slot = 9c").is_err());
    assert!(parse_params("s", "slot-9 = x").is_err());
    assert!(parse_params("s", "pin_policy = once").is_ok());
}

#[test]
fn a_value_may_be_empty_but_the_name_may_not() {
    assert_eq!(
        parse_params("s", "batch =").unwrap().get("batch").unwrap(),
        ""
    );
    assert!(parse_params("s", "= 9c").is_err());
}

#[test]
fn a_step_added_by_hand_carries_the_parameters_its_kind_reads() {
    // Every kind must be addable and immediately plannable: a step whose
    // parameters the operator was never told about would be refused for a reason
    // only `plan` knows.
    for kind in StepKind::ALL {
        let mut draft = TemplateDraft::blank();
        draft.id = "one-step".into();
        draft.name = "One step".into();
        draft.description = "A template with a single step.".into();
        draft.add_step(kind).unwrap();
        draft
            .check()
            .unwrap_or_else(|e| panic!("a fresh {kind:?} step does not plan: {e}"));
    }
}

#[test]
fn an_id_must_be_lower_case_hyphenated() {
    for good in ["org-standard", "fido-only", "piv2"] {
        assert!(check_id(good).is_ok(), "`{good}` should be a usable id");
    }
    for bad in [
        "",
        "Example Organisation",
        "2fa",
        "org--standard",
        "org-",
        "org standard",
        "org_standard",
    ] {
        assert!(check_id(bad).is_err(), "`{bad}` should be refused");
    }
}

#[test]
fn a_template_needs_a_name_a_description_and_a_step() {
    let mut template = BootstrapTemplate::blank("fido-only");
    assert!(matches!(
        template.check().unwrap_err(),
        TemplateError::Missing("a name")
    ));
    template.name = "FIDO2 only".into();
    assert!(matches!(
        template.check().unwrap_err(),
        TemplateError::Missing(_)
    ));
    template.description = "FIDO2 PIN only.".into();
    assert!(matches!(
        template.check().unwrap_err(),
        TemplateError::Missing("at least one step")
    ));
    template
        .steps
        .push(TemplateStep::for_kind(StepKind::Fido2Pin, "fido2-pin"));
    template.check().unwrap();
}

#[test]
fn the_gate_refuses_a_template_that_cannot_be_planned() {
    let mut template = BootstrapTemplate::org_standard();
    template
        .steps
        .iter_mut()
        .find(|s| s.kind == StepKind::PivKeygen)
        .unwrap()
        .params
        .remove("slot");
    assert!(matches!(
        template.check().unwrap_err(),
        TemplateError::MissingParam { .. }
    ));
}

#[test]
fn the_gate_checks_steps_that_arrive_disabled_too() {
    // The wizard can enable an optional step on any run, so a step that only
    // breaks when somebody ticks it must not reach the database.
    let mut template = BootstrapTemplate::org_standard();
    let step = template
        .steps
        .iter_mut()
        .find(|s| s.kind == StepKind::Fido2MinPinLength)
        .unwrap();
    step.enabled = false;
    step.params
        .insert("min_length".into(), "{{holder.shoe_size}}".into());
    assert!(matches!(
        template.check().unwrap_err(),
        TemplateError::UnknownVariable(_)
    ));
}

#[test]
fn a_step_needs_a_description() {
    let mut template = BootstrapTemplate::org_standard();
    template.steps[0].description.clear();
    assert!(matches!(
        template.check().unwrap_err(),
        TemplateError::Missing(_)
    ));
}

#[test]
fn a_second_step_of_the_same_kind_gets_its_own_id() {
    let mut draft = TemplateDraft::from_template(&BootstrapTemplate::org_standard(), true);
    draft.add_step(StepKind::OtpAccessCode).unwrap();
    let ids: Vec<&str> = draft.steps.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"otp-access-code-2"), "got: {ids:?}");
    // …and the template still validates, which is what the id is for.
    draft.check().unwrap();
}

#[test]
fn a_template_cannot_grow_past_the_step_bound() {
    let mut draft = TemplateDraft::blank();
    for _ in 0..MAX_STEPS {
        draft.add_step(StepKind::Verify).unwrap();
    }
    assert_eq!(
        draft.add_step(StepKind::Verify).unwrap_err(),
        TemplateError::TooManySteps(MAX_STEPS)
    );
}

#[test]
fn moving_a_step_changes_the_order_of_execution() {
    let mut draft = TemplateDraft::from_template(&BootstrapTemplate::org_standard(), true);
    let first = draft.steps[0].id.clone();
    let second = draft.steps[1].id.clone();

    draft.move_step(1, true);
    assert_eq!(draft.steps[0].id, second);
    assert_eq!(draft.steps[1].id, first);

    // The ends hold: nothing moves off either edge, and nothing panics.
    draft.move_step(0, true);
    assert_eq!(draft.steps[0].id, second);
    let last = draft.steps.len() - 1;
    draft.move_step(last, false);
    draft.move_step(last + 5, true);
    assert_eq!(
        draft.steps.len(),
        BootstrapTemplate::org_standard().steps.len()
    );
}

#[test]
fn removing_a_step_out_of_range_is_ignored_rather_than_fatal() {
    // The index comes from a table painted a frame earlier.
    let mut draft = TemplateDraft::from_template(&BootstrapTemplate::fido_only(), true);
    let before = draft.steps.len();
    draft.remove_step(99);
    assert_eq!(draft.steps.len(), before);
    draft.remove_step(0);
    assert_eq!(draft.steps.len(), before - 1);
}

#[test]
fn a_draft_is_dirty_only_when_the_stored_form_differs() {
    let stored = BootstrapTemplate::org_standard();
    let mut draft = TemplateDraft::from_template(&stored, true);
    assert!(!draft.is_dirty(Some(&stored)));

    // Re-spacing a parameter is not an edit — warning about it would train the
    // operator to ignore the warning.
    draft.steps[0].params_text = draft.steps[0].params_text.replace(" = ", "=");
    assert!(!draft.is_dirty(Some(&stored)));

    draft.steps[0].params_text = "min_length = 8\nsource = operator-entered".into();
    assert!(draft.is_dirty(Some(&stored)));
}

#[test]
fn an_unparseable_draft_counts_as_edited() {
    let stored = BootstrapTemplate::fido_only();
    let mut draft = TemplateDraft::from_template(&stored, true);
    draft.steps[0].params_text = "this is not a parameter".into();
    assert!(draft.is_dirty(Some(&stored)));
}

#[test]
fn a_new_template_is_dirty_as_soon_as_anything_is_typed() {
    let mut draft = TemplateDraft::blank();
    assert!(!draft.is_dirty(None));
    draft.name = "Contractor keys".into();
    assert!(draft.is_dirty(None));
}

#[test]
fn a_duplicate_keeps_the_steps_and_takes_a_new_identity() {
    let source = BootstrapTemplate::org_standard();
    let copy = source.duplicated_as("contractor", "Contractor keys");
    assert_eq!(copy.id, "contractor");
    assert_eq!(copy.name, "Contractor keys");
    assert_eq!(copy.steps, source.steps);
    copy.check().unwrap();
}

#[test]
fn a_free_id_is_taken_as_is_and_a_taken_one_is_numbered() {
    let taken = vec!["org-standard".to_owned(), "org-standard-copy".to_owned()];
    assert_eq!(unique_id(&taken, "fido-only"), "fido-only");
    assert_eq!(
        unique_id(&taken, "org-standard-copy"),
        "org-standard-copy-2"
    );
}

#[test]
fn the_wizard_is_offered_the_newest_version_of_each_template() {
    let mut v1 = BootstrapTemplate::fido_only();
    v1.version = "1".into();
    let mut v2 = BootstrapTemplate::fido_only();
    v2.version = "2".into();
    v2.name = "FIDO2 only (revised)".into();
    let standard = BootstrapTemplate::org_standard();

    let offered = latest_per_id(&[v1, v2, standard]);
    assert_eq!(offered.len(), 2, "one entry per id: {offered:?}");
    let fido = offered.iter().find(|t| t.id == "fido-only").unwrap();
    assert_eq!(fido.version, "2");
    assert_eq!(latest_of(&offered, "fido-only").unwrap().version, "2");
    assert_eq!(versions_of(&offered, "fido-only"), vec!["2".to_owned()]);
}

#[test]
fn versions_sort_numerically_not_alphabetically() {
    let mut nine = BootstrapTemplate::fido_only();
    nine.version = "9".into();
    let mut ten = BootstrapTemplate::fido_only();
    ten.version = "10".into();
    assert_eq!(latest_of(&[nine, ten], "fido-only").unwrap().version, "10");
}

#[test]
fn the_audit_entry_distinguishes_a_new_template_from_an_edit() {
    let stored = BootstrapTemplate::fido_only();

    let (event, target, details) = edit_audit_entry(&stored, None);
    assert_eq!(event, "template.created");
    assert_eq!(target, "template:fido-only");
    assert!(details.contains("previous=none"), "got: {details}");

    let (event, _, details) = edit_audit_entry(&stored, Some("1"));
    assert_eq!(event, "template.changed");
    assert!(details.contains("previous=1"), "got: {details}");
    assert!(details.contains("steps="), "the shape, not the procedure");
}

fn stored(template: BootstrapTemplate, runs: usize, retired: bool) -> StoredTemplate {
    StoredTemplate {
        template,
        retired_at: retired.then(|| "2026-08-11T10:00:00+00:00".to_owned()),
        runs,
        updated_at: "2026-08-11T10:00:00+00:00".to_owned(),
    }
}

#[test]
fn a_version_a_run_recorded_cannot_be_removed_and_the_refusal_names_retirement() {
    let mut own = BootstrapTemplate::org_standard();
    own.id = "unit-standard".into();
    own.version = "3".into();

    let refusal = stored(own, 4, false)
        .removal_refusal()
        .expect("a used version is refused");
    assert!(refusal.contains("4 bootstrap run"), "got: {refusal}");
    assert!(refusal.contains("retire"), "the alternative must be named");
}

#[test]
fn a_builtin_version_cannot_be_removed_because_it_would_come_back() {
    let refusal = stored(BootstrapTemplate::org_standard(), 0, false)
        .removal_refusal()
        .expect("a built-in is refused");
    assert!(refusal.contains("re-created"), "got: {refusal}");
    assert!(refusal.contains("retire"), "the alternative must be named");
}

#[test]
fn a_template_nobody_used_can_be_removed() {
    let mut own = BootstrapTemplate::org_standard();
    own.id = "typo-templte".into();
    let entry = stored(own, 0, false);
    assert!(entry.removal_refusal().is_none());
    assert!(!entry.is_retired());
    assert!(entry.audit_detail().contains("runs=0"));
}

#[test]
fn forcing_the_pin_change_before_a_step_that_needs_the_pin_is_refused() {
    // Found on a real 5.7.4 key, not here: a key marked `forcePINChange` refuses
    // its PIN for everything except changing it, so the credential step that
    // followed the mark could never have succeeded. The shipped procedure had
    // exactly that ordering. This is the guard that stops it coming back — in a
    // hand-built template as well as in the built-in one.
    use yk_dist_manager::domain::StepKind;
    use yk_dist_manager::template::{TemplateError, TemplateStep};

    let mut broken = BootstrapTemplate::org_standard();
    let marker = broken
        .steps
        .iter()
        .position(|s| s.kind == StepKind::Fido2ForcePinChange)
        .expect("the standard procedure marks the key");
    let step = broken.steps.remove(marker);
    // Put it back where it used to be: before the credential.
    let credential = broken
        .steps
        .iter()
        .position(|s| s.kind == StepKind::Fido2Credential)
        .expect("the standard procedure creates a credential");
    broken.steps.insert(credential, step);

    match broken.validate() {
        Err(TemplateError::PinLockedBeforeUse { marker, later }) => {
            assert_eq!(marker, "fido2-force-pin-change");
            assert_eq!(later, "fido2-credential");
        }
        other => panic!("the ordering must be refused, got: {other:?}"),
    }

    // And the shipped ordering is the right way round.
    assert!(BootstrapTemplate::org_standard().validate().is_ok());

    // A forced change with no later PIN step is fine — that is the whole point of
    // putting it last.
    let mut fine = BootstrapTemplate::org_standard();
    fine.steps
        .push(TemplateStep::new("verify-again", StepKind::Verify, "Read the key back").optional());
    assert!(fine.validate().is_ok());
}

#[test]
fn an_edited_builtin_version_is_no_longer_the_builtin() {
    // Version 2 of `org-standard` is the unit's own procedure: seeding will not
    // re-create it, so it is removable like any other.
    let mut edited = BootstrapTemplate::org_standard();
    edited.version = "2".into();
    assert!(!edited.is_builtin());
    assert!(stored(edited, 0, false).removal_refusal().is_none());
}
