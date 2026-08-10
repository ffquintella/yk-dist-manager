//! Turning a template into an **execution plan**.
//!
//! The plan is what the wizard shows the operator before anything touches the
//! key, and it is also what the executor runs. Secrets are represented as
//! [`Arg::Secret`] placeholders, so a plan can be displayed, logged and stored
//! without ever carrying a PIN.
//!
//! Every step carries **two** representations:
//!
//! * [`NativeOp`] — the pure-Rust call that performs the step in-process
//!   (`yubikey` over PC/SC, `ctap-hid-fido2` over HID, `hidapi` for OTP). This
//!   is the preferred path.
//! * a `ykman` argv — the fallback, used where no crate covers the operation
//!   yet, and as a cross-check while the native path is being validated.
//!
//! `ykman` flags follow version 5.9.2 (`ykman <cmd> --help`).

use crate::domain::StepKind;
use crate::template::{BootstrapTemplate, RenderContext, TemplateError, TemplateStep};

/// One argument of a planned command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    /// Safe to display and to persist.
    Literal(String),
    /// A secret supplied at execution time; only the label is ever shown.
    Secret(&'static str),
}

impl Arg {
    pub fn literal(value: impl Into<String>) -> Self {
        Arg::Literal(value.into())
    }

    /// Rendering used everywhere in the UI, in logs and in the audit trail.
    pub fn redacted(&self) -> String {
        match self {
            Arg::Literal(v) => v.clone(),
            Arg::Secret(label) => format!("<{label}>"),
        }
    }

    pub fn is_secret(&self) -> bool {
        matches!(self, Arg::Secret(_))
    }
}

/// The native, in-process way to perform a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeOp {
    /// Crate providing the operation.
    pub crate_name: &'static str,
    /// The call, as it appears in the code and in the docs.
    pub call: &'static str,
    /// Cargo feature that must be enabled for it.
    pub feature: &'static str,
    /// `false` when no Rust crate covers this operation yet, so `ykman` is
    /// still the only way to perform it.
    pub available: bool,
}

impl NativeOp {
    const fn new(
        crate_name: &'static str,
        call: &'static str,
        feature: &'static str,
        available: bool,
    ) -> Self {
        Self {
            crate_name,
            call,
            feature,
            available,
        }
    }

    pub fn describe(&self) -> String {
        format!("{}::{}", self.crate_name, self.call)
    }
}

/// Which transport will actually run a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// A native Rust call.
    Native,
    /// `ykman` subprocess, because nothing native covers it yet.
    Ykman,
    /// Neither: a human has to do something.
    Manual,
}

impl Transport {
    pub fn label(&self) -> &'static str {
        match self {
            Transport::Native => "native",
            Transport::Ykman => "ykman (fallback)",
            Transport::Manual => "manual",
        }
    }
}

/// A single step of the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCommand {
    pub step_id: String,
    pub kind: StepKind,
    /// Operator-facing description, already rendered.
    pub description: String,
    /// The preferred native call, when one exists.
    pub native: Option<NativeOp>,
    /// `ykman` fallback program name, when the step can be driven that way.
    pub program: Option<String>,
    pub args: Vec<Arg>,
    /// Anything the operator needs to know about how this step is performed.
    pub note: Option<String>,
}

impl PlannedCommand {
    /// The transport that will be used, given what is implemented today.
    pub fn transport(&self) -> Transport {
        match (&self.native, &self.program) {
            (Some(op), _) if op.available => Transport::Native,
            (_, Some(_)) => Transport::Ykman,
            _ => Transport::Manual,
        }
    }

    /// The `ykman` equivalent, with secrets replaced by placeholders. Shown as
    /// the reference command and used when [`Transport::Ykman`] applies.
    pub fn redacted_command(&self) -> String {
        match &self.program {
            None => format!("(manual) {}", self.description),
            Some(program) => {
                let mut parts = vec![program.clone()];
                parts.extend(self.args.iter().map(Arg::redacted));
                parts.join(" ")
            }
        }
    }

    /// One line describing how the step will run.
    pub fn transport_detail(&self) -> String {
        match self.transport() {
            Transport::Native => self
                .native
                .as_ref()
                .map(NativeOp::describe)
                .unwrap_or_default(),
            Transport::Ykman => self.redacted_command(),
            Transport::Manual => format!("(manual) {}", self.description),
        }
    }

    pub fn carries_secret(&self) -> bool {
        self.args.iter().any(Arg::is_secret)
    }
}

/// Build the full plan for a template bound to one key and holder.
pub fn plan(
    template: &BootstrapTemplate,
    ctx: &RenderContext,
) -> Result<Vec<PlannedCommand>, TemplateError> {
    template.validate()?;
    let mut commands = Vec::new();
    for step in template.enabled_steps() {
        commands.push(plan_step(step, ctx)?);
    }
    Ok(commands)
}

fn device_args(ctx: &RenderContext) -> Vec<Arg> {
    if ctx.key_serial.is_empty() {
        Vec::new()
    } else {
        vec![
            Arg::literal("--device"),
            Arg::literal(ctx.key_serial.clone()),
        ]
    }
}

fn ykman(ctx: &RenderContext, tail: Vec<Arg>) -> (Option<String>, Vec<Arg>) {
    let mut args = device_args(ctx);
    args.extend(tail);
    (Some("ykman".to_owned()), args)
}

/// The native operation for each step kind, and whether a crate covers it.
///
/// Keep this table and `docs/yubikey-reference.md` in step — it is the single
/// statement of what the app can do without a subprocess.
pub fn native_op(kind: StepKind) -> Option<NativeOp> {
    Some(match kind {
        StepKind::Fido2Pin => NativeOp::new(
            "ctap-hid-fido2",
            "FidoKeyHid::set_new_pin",
            "native-fido",
            true,
        ),
        StepKind::Fido2MinPinLength => NativeOp::new(
            "ctap-hid-fido2",
            "authenticatorConfig(setMinPINLength)",
            "native-fido",
            // CTAP 2.1 authenticatorConfig coverage still to be confirmed
            // against the crate; `ykman` carries this step meanwhile.
            false,
        ),
        StepKind::Fido2Credential => NativeOp::new(
            "ctap-hid-fido2",
            "make_credential(rk = true)",
            "native-fido",
            true,
        ),
        StepKind::OtpAccessCode | StepKind::OtpSlotConfig => NativeOp::new(
            "hidapi",
            "yubico OTP config frame (slot write)",
            "native-otp",
            // No crate implements the OTP configuration protocol; the frame
            // builder is ours to write. See features/step-otp-access-code.md.
            false,
        ),
        StepKind::PivPinPuk => NativeOp::new(
            "yubikey",
            "YubiKey::change_pin / change_puk",
            "native-piv",
            true,
        ),
        StepKind::PivManagementKey => {
            NativeOp::new("yubikey", "MgmKey::set_protected", "native-piv", true)
        }
        StepKind::PivKeygen => NativeOp::new("yubikey", "piv::generate", "native-piv", true),
        StepKind::PivCsr => NativeOp::new(
            "yubikey + x509-cert",
            "piv::sign_data over a CertReq we build",
            "native-piv",
            true,
        ),
        StepKind::PivCertImport => NativeOp::new(
            "yubikey",
            "certificate::Certificate::write",
            "native-piv",
            true,
        ),
        StepKind::Verify => NativeOp::new(
            "yubikey + ctap-hid-fido2",
            "YubiKey::piv_keys + get_info",
            "native-piv",
            true,
        ),
    })
}

fn plan_step(step: &TemplateStep, ctx: &RenderContext) -> Result<PlannedCommand, TemplateError> {
    let description = crate::template::render(&step.description, ctx)?;

    let (program, args, note) = match step.kind {
        StepKind::Fido2Pin => {
            let (p, a) = ykman(
                ctx,
                vec![
                    Arg::literal("fido"),
                    Arg::literal("access"),
                    Arg::literal("change-pin"),
                    Arg::literal("--new-pin"),
                    Arg::Secret("FIDO2-PIN"),
                ],
            );
            (
                p,
                a,
                Some(
                    "Native path sets the PIN over CTAP2 without ever putting it on a command \
                     line."
                        .into(),
                ),
            )
        }
        StepKind::Fido2MinPinLength => {
            let min = step.rendered_param("min_length", ctx)?;
            let (p, a) = ykman(
                ctx,
                vec![
                    Arg::literal("fido"),
                    Arg::literal("access"),
                    Arg::literal("set-min-length"),
                    Arg::literal(min),
                ],
            );
            (
                p,
                a,
                Some("Requires firmware 5.7 or newer; skipped automatically on older keys.".into()),
            )
        }
        StepKind::Fido2Credential => {
            let rp = step.rendered_param("rp_id", ctx)?;
            let user = step.rendered_param("user_name", ctx)?;
            (
                None,
                vec![
                    Arg::literal(format!("rp_id={rp}")),
                    Arg::literal(format!("user={user}")),
                ],
                Some(
                    "`ykman` cannot create credentials at all — it only lists and deletes them. \
                     This is the clearest case for the native path: a CTAP2 \
                     `authenticatorMakeCredential` with `rk=true` leaves the credential resident \
                     on the key. See features/step-fido2-credentials.md."
                        .into(),
                ),
            )
        }
        StepKind::OtpAccessCode => {
            let slot = step.rendered_param("slot", ctx)?;
            let (p, a) = ykman(
                ctx,
                vec![
                    Arg::literal("otp"),
                    Arg::literal("settings"),
                    Arg::literal(slot),
                    Arg::literal("--new-access-code"),
                    Arg::Secret("OTP-ACCESS-CODE"),
                    Arg::literal("--force"),
                ],
            );
            (
                p,
                a,
                Some(
                    "Access code is exactly 6 bytes, given as 12 hex characters. Stays on `ykman` \
                     until the HID config frame is implemented."
                        .into(),
                ),
            )
        }
        StepKind::OtpSlotConfig => {
            let slot = step.rendered_param("slot", ctx)?;
            let (p, a) = ykman(
                ctx,
                vec![
                    Arg::literal("otp"),
                    Arg::literal("chalresp"),
                    Arg::literal("--generate"),
                    Arg::literal(slot),
                    Arg::literal("--force"),
                ],
            );
            (p, a, None)
        }
        StepKind::PivPinPuk => {
            let (p, a) = ykman(
                ctx,
                vec![
                    Arg::literal("piv"),
                    Arg::literal("access"),
                    Arg::literal("change-pin"),
                    Arg::literal("--pin"),
                    Arg::Secret("CURRENT-PIV-PIN"),
                    Arg::literal("--new-pin"),
                    Arg::Secret("NEW-PIV-PIN"),
                ],
            );
            (
                p,
                a,
                Some(
                    "Followed by `change-puk`; the PUK must change too, or the factory default \
                     12345678 stays valid."
                        .into(),
                ),
            )
        }
        StepKind::PivManagementKey => {
            let algorithm = step.rendered_param("algorithm", ctx)?;
            let mut tail = vec![
                Arg::literal("piv"),
                Arg::literal("access"),
                Arg::literal("change-management-key"),
                Arg::literal("--algorithm"),
                Arg::literal(algorithm),
                Arg::literal("--pin"),
                Arg::Secret("PIV-PIN"),
            ];
            if step.param("protect").unwrap_or("false") == "true" {
                tail.push(Arg::literal("--protect"));
                tail.push(Arg::literal("--generate"));
            } else {
                tail.push(Arg::literal("--new-management-key"));
                tail.push(Arg::Secret("PIV-MGMT-KEY"));
            }
            tail.push(Arg::literal("--force"));
            let (p, a) = ykman(ctx, tail);
            (
                p,
                a,
                Some(
                    "A protected management key is generated at random and stored on the key \
                     itself, guarded by the PIN — nothing to hold in custody."
                        .into(),
                ),
            )
        }
        StepKind::PivKeygen => {
            let slot = step.rendered_param("slot", ctx)?;
            let algorithm = step.rendered_param("algorithm", ctx)?;
            let pin_policy = step.rendered_param("pin_policy", ctx)?;
            let touch_policy = step.rendered_param("touch_policy", ctx)?;
            let (p, a) = ykman(
                ctx,
                vec![
                    Arg::literal("piv"),
                    Arg::literal("keys"),
                    Arg::literal("generate"),
                    Arg::literal("--algorithm"),
                    Arg::literal(algorithm),
                    Arg::literal("--pin-policy"),
                    Arg::literal(pin_policy),
                    Arg::literal("--touch-policy"),
                    Arg::literal(touch_policy),
                    Arg::literal("--pin"),
                    Arg::Secret("PIV-PIN"),
                    Arg::literal(slot),
                    Arg::literal(format!("pubkey-{}.pem", ctx.key_serial)),
                ],
            );
            (
                p,
                a,
                Some(
                    "The private key never leaves the device; `piv::attest` proves that \
                     afterwards."
                        .into(),
                ),
            )
        }
        StepKind::PivCsr => {
            let slot = step.rendered_param("slot", ctx)?;
            let subject = step.rendered_param("subject", ctx)?;
            let hash = step.rendered_param("hash", ctx)?;
            let email = step.rendered_param("san_email", ctx)?;
            let (p, a) = ykman(
                ctx,
                vec![
                    Arg::literal("piv"),
                    Arg::literal("certificates"),
                    Arg::literal("request"),
                    Arg::literal("--subject"),
                    Arg::literal(subject),
                    Arg::literal("--hash-algorithm"),
                    Arg::literal(hash),
                    Arg::literal("--pin"),
                    Arg::Secret("PIV-PIN"),
                    Arg::literal(slot),
                    Arg::literal(format!("pubkey-{}.pem", ctx.key_serial)),
                    Arg::literal(format!("csr-{}.pem", ctx.key_serial)),
                ],
            );
            (
                p,
                a,
                Some(format!(
                    "`ykman` cannot put an e-mail in a SAN, so on the fallback path the CA has to \
                     add rfc822Name={email} from its profile. Building the CertReq ourselves and \
                     signing it with `piv::sign_data` puts the SAN in the request directly — the \
                     main reason this step goes native. See features/ca-integration.md."
                )),
            )
        }
        StepKind::PivCertImport => {
            let slot = step.rendered_param("slot", ctx)?;
            let mut tail = vec![
                Arg::literal("piv"),
                Arg::literal("certificates"),
                Arg::literal("import"),
                Arg::literal("--pin"),
                Arg::Secret("PIV-PIN"),
            ];
            if step.param("verify").unwrap_or("false") == "true" {
                tail.push(Arg::literal("--verify"));
            }
            tail.push(Arg::literal(slot));
            tail.push(Arg::literal(format!("cert-{}.pem", ctx.key_serial)));
            let (p, a) = ykman(ctx, tail);
            (p, a, None)
        }
        StepKind::Verify => {
            let (p, a) = ykman(ctx, vec![Arg::literal("piv"), Arg::literal("info")]);
            (
                p,
                a,
                Some(
                    "Also reads FIDO2 and OTP state; the results are stored on the run as \
                     evidence."
                        .into(),
                ),
            )
        }
    };

    Ok(PlannedCommand {
        step_id: step.id.clone(),
        kind: step.kind,
        description,
        native: native_op(step.kind),
        program,
        args,
        note,
    })
}
