//! Behaviour tests for the CA round trip: the request out, the certificate back.
//!
//! The issuer is the operator (`features/ca-integration.md` phase 1, decided
//! 2026-08-13). There is no CA endpoint to stand in for, and that is the point of
//! these scenarios: the certificate is the **one input to a run that arrives by
//! hand**, so every check on it happens here, before the write.
//!
//! No test writes to a real key: the executor only talks to the traits in
//! `device::write`, and the only implementation linked into a test binary is
//! `MockWriter`.

use yk_dist_manager::bootstrap::{
    Confirmation, ExecutionRequest, Executor, RunRecorder, Transports, certificate_request,
};
use yk_dist_manager::device::write::MockWriter;
use yk_dist_manager::domain::{BootstrapRun, RunStatus, StepKind, StepStatus};
use yk_dist_manager::secret::{Secret, SecretKind};
use yk_dist_manager::template::plan::{PlannedCommand, plan};
use yk_dist_manager::template::{BootstrapTemplate, RenderContext};

const SERIAL: u32 = 20_423_633;

/// The holder this run is for, matching the SAN in the fixture certificate.
const HOLDER_EMAIL: &str = "ana.silva@example.org";

/// A real certificate: self-signed P-256, `CN=Ana Silva`, with
/// `rfc822Name=ana.silva@example.org`. The same fixture `device::certificate`
/// parses, so these scenarios exercise the checks against a document `openssl`
/// produced rather than one this crate did.
const CERTIFICATE: &str = include_str!("fixtures/certificate_with_email_san.pem");

#[derive(Default)]
struct Recording {
    audit: Vec<(String, String, String)>,
}

impl RunRecorder for Recording {
    fn run_updated(&mut self, _: &BootstrapRun) -> Result<(), String> {
        Ok(())
    }
    fn audit(&mut self, event: &str, target: &str, detail: &str) -> Result<(), String> {
        self.audit
            .push((event.into(), target.into(), detail.into()));
        Ok(())
    }
}

impl Recording {
    fn detail_for(&self, event: &str) -> Option<&str> {
        self.audit
            .iter()
            .find(|(e, _, _)| e == event)
            .map(|(_, _, d)| d.as_str())
    }
}

fn template() -> BootstrapTemplate {
    BootstrapTemplate::builtin()
        .into_iter()
        .find(|t| t.id == "org-standard")
        .expect("the standard procedure ships with the build")
}

fn commands(template: &BootstrapTemplate) -> Vec<PlannedCommand> {
    let mut ctx = RenderContext::sample();
    ctx.key_serial = SERIAL.to_string();
    plan(template, &ctx).expect("the built-in template plans")
}

fn request<'a>(
    template: &'a BootstrapTemplate,
    commands: &'a [PlannedCommand],
    certificate: Option<&str>,
) -> ExecutionRequest<'a> {
    ExecutionRequest {
        template,
        commands,
        serial: SERIAL,
        holder_id: None,
        operator: "felipe".into(),
        relying_party: "example.org".into(),
        certificate_subject: "CN=Ana Silva,OU=ESI".into(),
        certificate_email: HOLDER_EMAIL.into(),
        holder_display: "Ana Silva".into(),
        certificate_pem: certificate.map(str::to_owned),
    }
}

fn step(run: &BootstrapRun, kind: StepKind) -> &yk_dist_manager::domain::StepOutcome {
    run.steps
        .iter()
        .find(|step| step.kind == kind)
        .expect("the standard procedure has this step")
}

/// The PIV PIN the operator wrote down when the first run generated it, typed back
/// in for the resume. Nothing is retained, so this is the only way the applet can
/// be authenticated to a second time.
fn transport_pin() -> Secret {
    Secret::from_operator_input(SecretKind::PivPin, "471102").expect("a six-digit PIN is usable")
}

/// A first run, with no certificate in hand yet.
fn first_run(key: &mut MockWriter, recording: &mut Recording) -> BootstrapRun {
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands, None);
    let confirmation = Confirmation::given(SERIAL, commands.len());
    let mut executor = Executor::new(Transports { backend: key });
    executor
        .run(&request, &confirmation, recording)
        .expect("the run itself is recordable")
}

#[test]
fn scenario_a_first_run_produces_a_request_and_keeps_it() {
    // Given a factory-fresh key
    let mut key = MockWriter::factory_fresh(SERIAL);
    let mut recording = Recording::default();

    // When the procedure runs with no certificate available yet
    let run = first_run(&mut key, &mut recording);

    // Then the request it produced is retrievable from the record, not just its
    // size. Without this the operator has nothing to take to the CA, and the only
    // way out is generating a second key and abandoning the first.
    let csr = certificate_request(&run).expect("the run kept its certification request");
    assert!(csr.starts_with("-----BEGIN CERTIFICATE REQUEST-----"));
    assert!(csr.ends_with("-----END CERTIFICATE REQUEST-----"));

    // And the import waited, saying what to do next
    let import = step(&run, StepKind::PivCertImport);
    assert_eq!(import.status, StepStatus::Skipped);
    assert!(
        import.detail.contains("resume"),
        "the operator has to be told how this finishes: {}",
        import.detail
    );
    assert!(!key.was_called("piv.import_certificate"));

    // And the key is not claimed as ready
    assert_ne!(
        run.status,
        RunStatus::Completed,
        "a key with no signing certificate is not a completed bootstrap"
    );
}

#[test]
fn scenario_the_operator_brings_the_certificate_back_and_the_run_finishes() {
    // Given a run that stopped with the import pending
    let mut key = MockWriter::factory_fresh(SERIAL);
    let mut recording = Recording::default();
    let run = first_run(&mut key, &mut recording);
    assert_eq!(
        step(&run, StepKind::PivCertImport).status,
        StepStatus::Skipped
    );

    // When the operator returns with the issued certificate and resumes
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands, Some(CERTIFICATE));
    let mut recording = Recording::default();
    let resumed = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor.supply(transport_pin());
        executor
            .resume(&request, run, &mut recording)
            .expect("a resume is recordable")
    };

    // Then the certificate is on the key
    assert!(key.was_called("piv.import_certificate"));
    let import = step(&resumed, StepKind::PivCertImport);
    assert_eq!(import.status, StepStatus::Done, "detail: {}", import.detail);

    // And the record says whose certificate it was — the evidence an audit asks
    // for is "which certificate went onto which key", and it is all non-secret.
    assert!(
        import.detail.contains("Ana Silva"),
        "the subject belongs in the record: {}",
        import.detail
    );
    assert!(
        import.detail.contains(HOLDER_EMAIL),
        "and the address the certificate carries: {}",
        import.detail
    );

    // And the resume is audited as a resume rather than as a fresh run
    assert!(recording.detail_for("bootstrap.resumed").is_some());

    // And nothing that had already succeeded was written a second time. Re-running
    // a PIN a holder may have changed is the failure rule 4 exists for.
    let repeats = key
        .operations()
        .iter()
        .filter(|op| **op == "piv.set_pin_and_puk")
        .count();
    assert_eq!(repeats, 1, "the PIN was set once, by the first run");
}

#[test]
fn scenario_a_certificate_for_another_holder_is_refused_and_nothing_is_written() {
    // The mix-up this check exists for: two holders' certificates come back from
    // the CA together and the wrong one is pasted. It would import cleanly and
    // then fail every signature the holder makes.
    let mut key = MockWriter::factory_fresh(SERIAL);
    let mut recording = Recording::default();
    let run = first_run(&mut key, &mut recording);

    let template = template();
    let commands = commands(&template);
    let mut request = request(&template, &commands, Some(CERTIFICATE));
    request.certificate_email = "bruno.costa@example.org".into();

    let mut recording = Recording::default();
    let resumed = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor.supply(transport_pin());
        executor
            .resume(&request, run, &mut recording)
            .expect("the refusal is recorded, not raised")
    };

    let import = step(&resumed, StepKind::PivCertImport);
    assert_eq!(import.status, StepStatus::Failed);
    assert!(
        import.detail.contains("bruno.costa@example.org"),
        "the refusal has to name what was expected: {}",
        import.detail
    );
    assert!(
        !key.was_called("piv.import_certificate"),
        "and nothing may reach the key"
    );
    assert_ne!(resumed.status, RunStatus::Completed);
}

#[test]
fn scenario_pasting_the_request_back_instead_of_the_certificate_is_refused() {
    // Also realistic: the operator copies the document the tool gave them rather
    // than the one the CA returned.
    let mut key = MockWriter::factory_fresh(SERIAL);
    let mut recording = Recording::default();
    let run = first_run(&mut key, &mut recording);
    let csr = certificate_request(&run)
        .expect("there is a request")
        .to_owned();

    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands, Some(&csr));

    let mut recording = Recording::default();
    let resumed = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor.supply(transport_pin());
        executor
            .resume(&request, run, &mut recording)
            .expect("the refusal is recorded, not raised")
    };

    let import = step(&resumed, StepKind::PivCertImport);
    assert_eq!(import.status, StepStatus::Failed);
    assert!(
        import.detail.contains("request"),
        "the message has to name the mix-up: {}",
        import.detail
    );
    assert!(!key.was_called("piv.import_certificate"));
}

#[test]
fn scenario_a_pin_the_operator_typed_is_not_offered_back_as_a_new_secret() {
    // The show-once panel says *write these down, nothing keeps a copy*. A PIN the
    // operator has just typed in does not belong under that sentence: it would tell
    // them to record a value they already hold, and audit it as newly shown.
    let mut key = MockWriter::factory_fresh(SERIAL);
    let mut recording = Recording::default();
    let run = first_run(&mut key, &mut recording);

    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands, Some(CERTIFICATE));
    let mut recording = Recording::default();
    let mut executor = Executor::new(Transports { backend: &mut key });
    executor.supply(transport_pin());
    executor
        .resume(&request, run, &mut recording)
        .expect("a resume is recordable");

    assert!(
        executor.take_secrets().is_empty(),
        "a resume that generated nothing has nothing to show"
    );
}

#[test]
fn scenario_the_certificate_never_reaches_the_audit_trail_as_a_secret() {
    // A certificate is a public document and is *meant* to be in the record. What
    // must not be there is a secret, and this run generates several — so the trail
    // is swept for them, the same way `behaviour_executor` sweeps a plan.
    let mut key = MockWriter::factory_fresh(SERIAL);
    let mut recording = Recording::default();
    let run = first_run(&mut key, &mut recording);

    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands, Some(CERTIFICATE));
    let mut recording = Recording::default();
    let resumed = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor.supply(transport_pin());
        executor.resume(&request, run, &mut recording).unwrap()
    };

    let everything = recording
        .audit
        .iter()
        .map(|(e, t, d)| format!("{e} {t} {d}"))
        .chain(resumed.steps.iter().map(|s| s.detail.clone()))
        .collect::<Vec<_>>()
        .join("\n");

    for factory_default in ["123456", "12345678", "010203040506"] {
        assert!(
            !everything.contains(factory_default),
            "a factory default reached the record: {factory_default}"
        );
    }
    assert!(!everything.contains("PRIVATE KEY"));
}
