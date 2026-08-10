//! Bootstrap **templates**: the declarative description of what a bootstrap
//! does, plus the `{{variable}}` rendering that binds a template to one holder
//! and one key.
//!
//! Templates are data, not code: an operator can add or reorder steps without
//! touching Rust. See `features/bootstrap-templates.md` for the format spec and
//! `docs/bootstrap-procedure.md` for the procedure itself.

pub mod plan;

pub use plan::{Arg, NativeOp, PlannedCommand, Transport, native_op, plan};

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::{Holder, StepKind};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("unknown variable `{0}` — see RenderContext for the available names")]
    UnknownVariable(String),
    #[error("unterminated `{{{{` in template text")]
    Unterminated,
    #[error("step `{step}` is missing required parameter `{param}`")]
    MissingParam { step: String, param: String },
    #[error("template has no enabled steps")]
    Empty,
    #[error("duplicate step id `{0}`")]
    DuplicateStepId(String),
}

/// Values a template can interpolate. Everything here is non-secret.
#[derive(Debug, Clone, Default)]
pub struct RenderContext {
    pub holder_name: String,
    pub holder_email: String,
    pub holder_unit: String,
    pub key_serial: String,
    pub key_model: String,
    pub operator: String,
    pub org: String,
    pub org_unit: String,
    /// `YYYY-MM-DD`, so rendered subjects are reproducible in tests.
    pub date: String,
}

impl RenderContext {
    pub fn for_holder(holder: &Holder, key_serial: u32, operator: &str, org: &str) -> Self {
        Self {
            holder_name: holder.full_name.clone(),
            holder_email: holder.email.clone(),
            holder_unit: holder.unit.clone(),
            key_serial: key_serial.to_string(),
            key_model: String::new(),
            operator: operator.to_owned(),
            org: org.to_owned(),
            org_unit: holder.unit.clone(),
            date: chrono::Utc::now().format("%Y-%m-%d").to_string(),
        }
    }

    fn lookup(&self, name: &str) -> Option<&str> {
        Some(match name {
            "holder.name" => &self.holder_name,
            "holder.email" => &self.holder_email,
            "holder.unit" => &self.holder_unit,
            "key.serial" => &self.key_serial,
            "key.model" => &self.key_model,
            "operator" => &self.operator,
            "org" => &self.org,
            "org.unit" => &self.org_unit,
            "date" => &self.date,
            _ => return None,
        })
    }

    /// Names accepted by [`render`], for the template editor's help panel.
    pub const VARIABLES: [&'static str; 9] = [
        "holder.name",
        "holder.email",
        "holder.unit",
        "key.serial",
        "key.model",
        "operator",
        "org",
        "org.unit",
        "date",
    ];
}

/// Substitute every `{{ name }}` occurrence. An unknown name is an error, never
/// an empty string — a silently blank certificate subject is worse than a
/// refused bootstrap.
pub fn render(text: &str, ctx: &RenderContext) -> Result<String, TemplateError> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(TemplateError::Unterminated);
        };
        let name = after[..end].trim();
        match ctx.lookup(name) {
            Some(value) => out.push_str(value),
            None => return Err(TemplateError::UnknownVariable(name.to_owned())),
        }
        rest = &after[end + 2..];
    }

    out.push_str(rest);
    Ok(out)
}

/// One step of a template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateStep {
    /// Stable id, referenced by [`crate::domain::StepOutcome`].
    pub id: String,
    pub kind: StepKind,
    /// Operator-facing description; may contain `{{variables}}`.
    pub description: String,
    pub enabled: bool,
    /// A failure here aborts the run; otherwise the run continues and the step
    /// is recorded as failed.
    pub required: bool,
    /// Step parameters; values may contain `{{variables}}`.
    pub params: BTreeMap<String, String>,
}

impl TemplateStep {
    pub fn new(id: &str, kind: StepKind, description: &str) -> Self {
        Self {
            id: id.to_owned(),
            kind,
            description: description.to_owned(),
            enabled: true,
            required: true,
            params: BTreeMap::new(),
        }
    }

    pub fn with_param(mut self, key: &str, value: &str) -> Self {
        self.params.insert(key.to_owned(), value.to_owned());
        self
    }

    pub fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    pub fn param(&self, key: &str) -> Result<&str, TemplateError> {
        self.params
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| TemplateError::MissingParam {
                step: self.id.clone(),
                param: key.to_owned(),
            })
    }

    /// Render one parameter against the context.
    pub fn rendered_param(&self, key: &str, ctx: &RenderContext) -> Result<String, TemplateError> {
        render(self.param(key)?, ctx)
    }
}

/// A named, versioned bootstrap procedure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapTemplate {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub steps: Vec<TemplateStep>,
}

impl BootstrapTemplate {
    /// Structural checks run before a template can be selected in the wizard.
    pub fn validate(&self) -> Result<(), TemplateError> {
        let mut seen = std::collections::BTreeSet::new();
        for step in &self.steps {
            if !seen.insert(step.id.as_str()) {
                return Err(TemplateError::DuplicateStepId(step.id.clone()));
            }
        }
        if !self.steps.iter().any(|s| s.enabled) {
            return Err(TemplateError::Empty);
        }
        Ok(())
    }

    pub fn enabled_steps(&self) -> impl Iterator<Item = &TemplateStep> {
        self.steps.iter().filter(|s| s.enabled)
    }

    /// The default procedure: FIDO2 PIN + OTP access code + on-key FIDO2
    /// credential + PIV signing certificate bound to the holder's e-mail.
    ///
    /// This mirrors `docs/bootstrap-procedure.md`; keep the two in step.
    pub fn default_fgv() -> Self {
        Self {
            id: "fgv-standard".into(),
            name: "FGV standard bootstrap".into(),
            version: "1".into(),
            description: "FIDO2 PIN, OTP slot access code, initial on-key FIDO2 credential, \
                          and a PIV signing certificate carrying the holder's e-mail."
                .into(),
            steps: vec![
                TemplateStep::new(
                    "fido2-pin",
                    StepKind::Fido2Pin,
                    "Set the FIDO2 PIN for {{holder.name}}",
                )
                .with_param("min_length", "6")
                .with_param("source", "operator-entered"),
                TemplateStep::new(
                    "fido2-min-pin-length",
                    StepKind::Fido2MinPinLength,
                    "Raise the minimum FIDO2 PIN length (firmware 5.7+ only)",
                )
                .with_param("min_length", "6")
                .optional(),
                TemplateStep::new(
                    "otp-access-code",
                    StepKind::OtpAccessCode,
                    "Write-protect OTP slot 1 with a 6-byte access code",
                )
                .with_param("slot", "1")
                .with_param("source", "generated"),
                TemplateStep::new(
                    "fido2-credential",
                    StepKind::Fido2Credential,
                    "Register the initial discoverable credential on the key for {{holder.email}}",
                )
                .with_param("rp_id", "{{org}}")
                .with_param("user_name", "{{holder.email}}")
                .with_param("resident", "true"),
                TemplateStep::new(
                    "piv-pin-puk",
                    StepKind::PivPinPuk,
                    "Set the PIV PIN and PUK",
                )
                .with_param("source", "operator-entered"),
                TemplateStep::new(
                    "piv-management-key",
                    StepKind::PivManagementKey,
                    "Replace the default PIV management key (stored on-key, PIN-protected)",
                )
                .with_param("algorithm", "aes256")
                .with_param("protect", "true"),
                TemplateStep::new(
                    "piv-keygen",
                    StepKind::PivKeygen,
                    "Generate the signing key on the device, slot 9c",
                )
                .with_param("slot", "9c")
                .with_param("algorithm", "eccp256")
                .with_param("pin_policy", "once")
                .with_param("touch_policy", "cached"),
                TemplateStep::new(
                    "piv-csr",
                    StepKind::PivCsr,
                    "Request a signing certificate for {{holder.email}}",
                )
                .with_param("slot", "9c")
                .with_param("subject", "CN={{holder.name}},OU={{org.unit}},O={{org}}")
                .with_param("san_email", "{{holder.email}}")
                .with_param("hash", "sha256"),
                TemplateStep::new(
                    "piv-cert-import",
                    StepKind::PivCertImport,
                    "Import the issued certificate into slot 9c",
                )
                .with_param("slot", "9c")
                .with_param("verify", "true"),
                TemplateStep::new(
                    "verify",
                    StepKind::Verify,
                    "Read the key back and confirm the applied state",
                )
                .with_param("expect_fido_pin", "true")
                .with_param("expect_piv_slot", "9c"),
            ],
        }
    }

    /// A minimal template for keys that only need WebAuthn.
    pub fn fido_only() -> Self {
        let full = Self::default_fgv();
        Self {
            id: "fido-only".into(),
            name: "FIDO2 only".into(),
            version: "1".into(),
            description: "FIDO2 PIN plus the initial on-key credential. No PIV, no OTP.".into(),
            steps: full
                .steps
                .into_iter()
                .filter(|s| {
                    matches!(
                        s.kind,
                        StepKind::Fido2Pin
                            | StepKind::Fido2MinPinLength
                            | StepKind::Fido2Credential
                            | StepKind::Verify
                    )
                })
                .collect(),
        }
    }

    /// Templates shipped with the app.
    pub fn builtin() -> Vec<Self> {
        vec![Self::default_fgv(), Self::fido_only()]
    }
}
