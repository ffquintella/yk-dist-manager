//! Unit tests for template rendering and command planning.
//!
//! The key invariant: **no secret ever appears in a rendered plan**.

use yk_dist_manager::domain::{Holder, StepKind};
use yk_dist_manager::template::{
    Arg, BootstrapTemplate, RenderContext, TemplateError, Transport, native_op, plan, render,
};

fn ctx() -> RenderContext {
    let holder = Holder::new("Ana Silva", "ana.silva@fgv.br", "ESI", "12345").unwrap();
    let mut ctx = RenderContext::for_holder(&holder, 20_423_633, "felipe", "FGV");
    ctx.date = "2026-08-10".into();
    ctx
}

#[test]
fn renders_known_variables() {
    let out = render("subject for {{holder.name}} <{{holder.email}}>", &ctx()).unwrap();
    assert_eq!(out, "subject for Ana Silva <ana.silva@fgv.br>");
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
    let mut template = BootstrapTemplate::default_fgv();
    let first = template.steps[0].clone();
    template.steps.push(first);
    assert!(matches!(
        template.validate().unwrap_err(),
        TemplateError::DuplicateStepId(_)
    ));
}

#[test]
fn template_with_no_enabled_step_is_rejected() {
    let mut template = BootstrapTemplate::default_fgv();
    for step in &mut template.steps {
        step.enabled = false;
    }
    assert_eq!(template.validate().unwrap_err(), TemplateError::Empty);
}

#[test]
fn plan_covers_every_enabled_step_in_order() {
    let template = BootstrapTemplate::default_fgv();
    let commands = plan(&template, &ctx()).unwrap();
    assert_eq!(commands.len(), template.steps.len());
    for (command, step) in commands.iter().zip(&template.steps) {
        assert_eq!(command.step_id, step.id);
    }
}

#[test]
fn no_plan_output_can_leak_a_secret() {
    let commands = plan(&BootstrapTemplate::default_fgv(), &ctx()).unwrap();
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
    let commands = plan(&BootstrapTemplate::default_fgv(), &ctx()).unwrap();
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
    let commands = plan(&BootstrapTemplate::default_fgv(), &ctx()).unwrap();
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
            .contains("ana.silva@fgv.br"),
        "the SAN e-mail must be stated on the step"
    );
}

#[test]
fn the_key_serial_selects_the_device_on_every_command() {
    let commands = plan(&BootstrapTemplate::default_fgv(), &ctx()).unwrap();
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
fn credential_registration_is_native_because_ykman_cannot_do_it() {
    let commands = plan(&BootstrapTemplate::default_fgv(), &ctx()).unwrap();
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
    let commands = plan(&BootstrapTemplate::default_fgv(), &ctx()).unwrap();
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
    let mut template = BootstrapTemplate::default_fgv();
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
