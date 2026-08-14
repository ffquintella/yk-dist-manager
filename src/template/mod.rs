//! Bootstrap **templates**: the declarative description of what a bootstrap
//! does, plus the `{{variable}}` rendering that binds a template to one holder
//! and one key.
//!
//! Templates are data, not code: an operator can add, reorder or remove steps
//! without touching Rust, from the Templates screen. See
//! `features/bootstrap-templates.md` for the format spec and
//! `docs/bootstrap-procedure.md` for the procedure itself.
//!
//! Three rules the editor is built around:
//!
//! * **An edit is a new version.** `(id, version)` is the primary key and a
//!   bootstrap run records the version it applied, so the version on record is
//!   never overwritten — [`crate::versioning::next_version`] numbers the next one
//!   from what the database holds.
//! * **A draft is checked before it is stored.** [`BootstrapTemplate::check`]
//!   plans the template against [`RenderContext::sample`], so an unknown variable
//!   or a step missing a parameter is refused at the desk and not in front of a
//!   key.
//! * **Nothing that ran is deleted.** A template a run refers to can be
//!   *retired* (withdrawn from the wizard, kept on record); only a version
//!   nothing refers to can be removed outright. See [`StoredTemplate`].

pub mod applicability;
pub mod diff;
pub mod draft;
pub mod plan;
pub mod portable;
pub mod signing;

pub use applicability::{Applicability, Verdict};
pub use diff::{Change, DiffLine, TemplateDiff};
pub use draft::{StepDraft, TemplateDraft};
pub use plan::{Arg, NativeOp, PlannedCommand, Transport, native_op, plan};
pub use portable::{PortableError, TemplateFile};
pub use signing::{TemplateKey, TemplateSignature, Trust};

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::{Holder, StepKind};
use crate::versioning::version_order;

/// Most steps one template may carry. The standard procedure has eleven; the
/// bound exists because every input in this application has one (NRM §5.3.5).
pub const MAX_STEPS: usize = 40;

/// Longest a template or step id may be. Ids reach a bootstrap run record and,
/// later, an exported file name.
pub const MAX_ID: usize = 64;

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
    #[error("the template needs {0}")]
    Missing(&'static str),
    #[error("{field} is longer than {max} characters")]
    TooLong { field: &'static str, max: usize },
    #[error(
        "`{0}` is not a usable id — use lower-case letters, digits and hyphens, starting with a \
         letter (e.g. `fido-only`)"
    )]
    BadId(String),
    #[error("step `{step}`: `{line}` is not a `name = value` parameter")]
    BadParam { step: String, line: String },
    #[error("a template cannot have more than {0} steps")]
    TooManySteps(usize),
    #[error(
        "step `{later}` needs the FIDO2 PIN, but `{marker}` marks the key for a forced PIN \
         change before it. A key marked that way refuses its PIN for everything except changing \
         it, so `{later}` could never succeed — move the forced change after it"
    )]
    PinLockedBeforeUse { marker: String, later: String },
    #[error(
        "`{value}` is not a firmware version for `{field}` — write it as three numbers, e.g. \
         `5.7.0`. A bound this cannot read would refuse every key without saying why"
    )]
    BadVersionBound { field: &'static str, value: String },
    #[error("the firmware range is impossible: the lowest allowed is newer than the highest")]
    ImpossibleVersionRange,
    #[error(
        "`{0}` is not an application a YubiKey has — use one of: Yubico OTP, FIDO U2F, FIDO2, \
         OATH, PIV, OpenPGP, YubiHSM Auth"
    )]
    UnknownApplication(String),
    #[error("step `{step}`: {attempts} attempts is more than the {max} this allows")]
    TooManyAttempts { step: String, attempts: u8, max: u8 },
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

    /// A fictitious holder and key, for checking a draft template.
    ///
    /// The date is fixed rather than read from the clock: this context exists so
    /// [`BootstrapTemplate::check`] can plan a draft, and a check that depends on
    /// today's date is a check that cannot be tested. Nothing here is a real
    /// person — the same convention as [`crate::term::TermContext::sample`].
    pub fn sample() -> Self {
        Self {
            holder_name: "Ana Silva".into(),
            holder_email: "ana.silva@example.org".into(),
            holder_unit: "ESI".into(),
            key_serial: "20423633".into(),
            key_model: "YubiKey 5 NFC".into(),
            operator: "operator".into(),
            org: "Example Organisation".into(),
            org_unit: "ESI".into(),
            date: "2026-01-01".into(),
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateStep {
    /// Stable id, referenced by [`crate::domain::StepOutcome`].
    pub id: String,
    pub kind: StepKind,
    /// Operator-facing description; may contain `{{variables}}`.
    pub description: String,
    pub enabled: bool,
    /// A failure here aborts the run; otherwise the run continues and the step
    /// is recorded as failed.
    ///
    /// This is the **continue-on-failure** half of
    /// `features/bootstrap-templates.md` phase 7, and it has existed since phase 1;
    /// [`Self::attempts`] is the retry half.
    pub required: bool,
    /// How many times the executor attempts this step before recording it failed.
    ///
    /// `1` — one attempt — is the behaviour every template had before this field
    /// existed, and is what the encoding writes as nothing at all, so a stored
    /// template's bytes, its fingerprint and any signature over it are unchanged
    /// until somebody raises it.
    ///
    /// **Not every failure is retried, whatever this says.** A wrong PIN is not
    /// (retrying burns the applet's counter towards a lock), a detached key is not
    /// (there is nothing to retry against), an unsupported operation is not (it
    /// will be unsupported the second time too). Only a transport-level failure is
    /// — see [`crate::device::write::WriteError::is_worth_retrying`], which is
    /// where the policy lives, because it is a property of the error and not of
    /// the procedure.
    #[serde(default = "one_attempt", skip_serializing_if = "is_one_attempt")]
    pub attempts: u8,
    /// Step parameters; values may contain `{{variables}}`.
    pub params: BTreeMap<String, String>,
}

/// Most attempts a step may be given.
///
/// A bound because every input here has one, and a low one because retrying is
/// for a transport that dropped a frame, not for a key that is refusing. A step
/// that needs six goes needs an operator, not a loop.
pub const MAX_ATTEMPTS: u8 = 5;

fn one_attempt() -> u8 {
    1
}

fn is_one_attempt(attempts: &u8) -> bool {
    *attempts <= 1
}

impl TemplateStep {
    pub fn new(id: &str, kind: StepKind, description: &str) -> Self {
        Self {
            id: id.to_owned(),
            kind,
            description: description.to_owned(),
            enabled: true,
            required: true,
            attempts: 1,
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

    /// Give this step more than one go at a transport-level failure.
    pub fn with_attempts(mut self, attempts: u8) -> Self {
        self.attempts = attempts;
        self
    }

    /// The attempts this step actually gets, whatever the stored value says.
    ///
    /// Clamped rather than trusted: the field is `u8` and reaches the executor
    /// from a JSON body a hand edit could have set to 200, which would be a loop
    /// against a key rather than a retry.
    pub fn attempt_budget(&self) -> u8 {
        self.attempts.clamp(1, MAX_ATTEMPTS)
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

    /// A ready-to-edit step of one kind: the parameters that kind reads, filled
    /// with the values the standard procedure uses.
    ///
    /// This is what "Add step" inserts. The defaults come from the same place the
    /// built-in template does, so a step added by hand plans on the first try
    /// instead of being refused for a parameter the operator was never told
    /// about — the parameters each kind reads are otherwise only visible in
    /// [`plan`].
    pub fn for_kind(kind: StepKind, id: &str) -> Self {
        let (description, params): (&str, &[(&str, &str)]) = match kind {
            StepKind::Fido2Pin => (
                "Set the FIDO2 PIN for {{holder.name}}",
                &[("min_length", "6"), ("source", "operator-entered")],
            ),
            StepKind::Fido2MinPinLength => (
                "Raise the minimum FIDO2 PIN length (firmware 5.7+ only)",
                &[("min_length", "6")],
            ),
            StepKind::Fido2ForcePinChange => (
                "Require {{holder.name}} to change the transport PIN before first use",
                &[("enforcement", "firmware-if-available")],
            ),
            StepKind::Fido2Credential => (
                "Register the initial discoverable credential on the key for {{holder.email}}",
                &[
                    ("rp_id", "{{org}}"),
                    ("user_name", "{{holder.email}}"),
                    ("resident", "true"),
                ],
            ),
            StepKind::OtpAccessCode => (
                "Write-protect OTP slot 1 with a 6-byte access code",
                &[("slot", "1"), ("source", "generated")],
            ),
            StepKind::OtpSlotConfig => (
                "Program OTP slot 2 for challenge-response",
                &[("slot", "2")],
            ),
            StepKind::PivPinPuk => ("Set the PIV PIN and PUK", &[("source", "operator-entered")]),
            StepKind::PivManagementKey => (
                "Replace the default PIV management key (stored on-key, PIN-protected)",
                &[("algorithm", "aes256"), ("protect", "true")],
            ),
            StepKind::PivKeygen => (
                "Generate the signing key on the device, slot 9c",
                &[
                    ("slot", "9c"),
                    ("algorithm", "eccp256"),
                    ("pin_policy", "once"),
                    ("touch_policy", "cached"),
                ],
            ),
            StepKind::PivCsr => (
                "Request a signing certificate for {{holder.email}}",
                &[
                    ("slot", "9c"),
                    ("subject", "CN={{holder.name}},OU={{org.unit}},O={{org}}"),
                    ("san_email", "{{holder.email}}"),
                    ("hash", "sha256"),
                ],
            ),
            StepKind::PivCertImport => (
                "Import the issued certificate into slot 9c",
                &[("slot", "9c"), ("verify", "true")],
            ),
            StepKind::Verify => (
                "Read the key back and confirm the applied state",
                &[("expect_fido_pin", "true"), ("expect_piv_slot", "9c")],
            ),
        };

        let mut step = Self::new(id, kind, description);
        for (key, value) in params {
            step.params.insert((*key).to_owned(), (*value).to_owned());
        }
        step
    }

    /// The parameters as `name = value` lines, which is how the editor shows
    /// them: ordered (the map is a `BTreeMap`), diffable, and needing no schema
    /// per step kind.
    pub fn params_text(&self) -> String {
        self.params
            .iter()
            .map(|(key, value)| format!("{key} = {value}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Everything that must hold before this step is stored.
    fn check(&self) -> Result<(), TemplateError> {
        check_id(&self.id)?;
        if self.attempts > MAX_ATTEMPTS {
            return Err(TemplateError::TooManyAttempts {
                step: self.id.clone(),
                attempts: self.attempts,
                max: MAX_ATTEMPTS,
            });
        }
        if self.description.trim().is_empty() {
            return Err(TemplateError::Missing("a description on every step"));
        }
        bound(
            "a step description",
            &self.description,
            crate::domain::MAX_NOTE,
        )?;
        for (key, value) in &self.params {
            if !is_param_name(key) {
                return Err(TemplateError::BadParam {
                    step: self.id.clone(),
                    line: key.clone(),
                });
            }
            bound("a step parameter", value, crate::domain::MAX_TEXT)?;
        }
        Ok(())
    }
}

/// Parse the editor's `name = value` lines back into parameters.
///
/// Blank lines and `#` comments are ignored, the first `=` separates, and both
/// sides are trimmed. A line that is neither blank nor a `name = value` pair is a
/// typed error naming the step — silently dropping it would mean a template that
/// looks right on screen and plans differently.
pub fn parse_params(step_id: &str, text: &str) -> Result<BTreeMap<String, String>, TemplateError> {
    let mut params = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            return Err(TemplateError::BadParam {
                step: step_id.to_owned(),
                line: line.to_owned(),
            });
        };
        let name = name.trim();
        if !is_param_name(name) {
            return Err(TemplateError::BadParam {
                step: step_id.to_owned(),
                line: line.to_owned(),
            });
        }
        params.insert(name.to_owned(), value.trim().to_owned());
    }
    Ok(params)
}

/// `slot`, `min_length`, `pin_policy` — lower-case, digits and underscores.
fn is_param_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_ID
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Ids reach bootstrap run records, so they are restricted to what reads
/// unambiguously in a report years later: `org-standard`, `piv-csr`.
pub fn check_id(id: &str) -> Result<(), TemplateError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(TemplateError::Missing("an id"));
    }
    let ok = id.len() <= MAX_ID
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.contains("--")
        && !id.ends_with('-');
    if ok {
        Ok(())
    } else {
        Err(TemplateError::BadId(id.to_owned()))
    }
}

fn bound(field: &'static str, value: &str, max: usize) -> Result<(), TemplateError> {
    if value.chars().count() > max {
        return Err(TemplateError::TooLong { field, max });
    }
    Ok(())
}

/// `base`, or `base-2`, `base-3`… — the first that nothing in `existing` uses.
///
/// Used for both ids that have to be unique: a step id inside a template, and a
/// template id inside the database.
pub fn unique_id(existing: &[String], base: &str) -> String {
    if !existing.iter().any(|id| id == base) {
        return base.to_owned();
    }
    for n in 2.. {
        let candidate = format!("{base}-{n}");
        if !existing.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("the loop returns on the first free candidate")
}

/// An id for a new step of `kind` that no step in `existing` already uses.
///
/// The kind's own slug first (`piv-csr`), then `-2`, `-3`… — a template may
/// legitimately carry the same kind twice (two OTP slots, two credentials), and a
/// duplicate id is refused by [`BootstrapTemplate::validate`].
pub fn unique_step_id(existing: &[String], kind: StepKind) -> String {
    unique_id(existing, kind.slug())
}

/// A named, versioned bootstrap procedure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapTemplate {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub steps: Vec<TemplateStep>,
    /// Which keys this procedure may be applied to
    /// (`features/bootstrap-templates.md` phase 3).
    ///
    /// Default is *unrestricted*, and an unrestricted rule serialises to nothing,
    /// so a template stored before this field existed reads back byte-for-byte
    /// unchanged — the same contract [`Self::signature`] has, and for the sharper
    /// reason: the canonical bytes a signature is made over would otherwise
    /// change under every template in the field at once.
    #[serde(default, skip_serializing_if = "Applicability::is_unrestricted")]
    pub applicability: Applicability,
    /// Who signed this procedure, when anybody has
    /// ([`signing`], `features/bootstrap-templates.md` phase 5).
    ///
    /// It lives *inside* the template rather than in a column of its own for one
    /// reason that decides it: the signature has to travel wherever the procedure
    /// travels — into the wizard, into an exported file, into the pre-flight check
    /// before a run — and a field on the model does that everywhere at once, where
    /// a column would have to be threaded through every path by hand and would be
    /// dropped by the first one somebody forgot.
    ///
    /// It is skipped when absent, so a body written by a build before this field
    /// existed reads back unchanged and an unsigned template's stored JSON is
    /// byte-for-byte what it always was.
    ///
    /// The signature does not cover itself: [`signing::canonical_bytes`] ignores
    /// this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<TemplateSignature>,
}

impl BootstrapTemplate {
    /// An empty template for the editor to start from. Version `1`: nothing is
    /// stored until it is saved, and the store assigns the number then.
    pub fn blank(id: &str) -> Self {
        Self {
            id: id.trim().to_owned(),
            name: String::new(),
            version: "1".into(),
            description: String::new(),
            steps: Vec::new(),
            applicability: Applicability::default(),
            signature: None,
        }
    }

    /// The same template under a new id and name, for "duplicate".
    ///
    /// Duplication is how a variant is made — a FIDO-only key for a contractor
    /// derived from the standard procedure — so it copies the steps and leaves
    /// the version to the store.
    pub fn duplicated_as(&self, id: &str, name: &str) -> Self {
        Self {
            id: id.trim().to_owned(),
            name: name.trim().to_owned(),
            version: "1".into(),
            description: self.description.clone(),
            steps: self.steps.clone(),
            // The rule travels with the procedure: a variant cut from a template
            // that is only for firmware 5.7 is, until somebody says otherwise,
            // also only for firmware 5.7.
            applicability: self.applicability.clone(),
            // A duplicate is a *different* procedure: the id is part of what a
            // signature covers, so carrying the signature across would produce a
            // template that fails verification and looks tampered with. Dropping
            // it says the truth — this copy has not been signed by anybody.
            signature: None,
        }
    }

    /// The same template under a given version, trimmed as it will be stored.
    pub fn as_version(&self, version: &str) -> Self {
        Self {
            id: self.id.trim().to_owned(),
            name: self.name.trim().to_owned(),
            version: version.trim().to_owned(),
            description: self.description.trim().to_owned(),
            steps: self.steps.clone(),
            applicability: self.applicability.clone(),
            // Renumbering keeps the signature, and that is the point of leaving
            // the version out of the canonical bytes: the store assigns version
            // numbers, so a signature that broke on renumbering could never
            // survive being stored or imported.
            signature: self.signature.clone(),
        }
    }

    /// Is this one of the templates this build ships, under the same version?
    ///
    /// It matters for removal: a built-in is re-created by
    /// [`crate::store::Store::seed_builtin_templates`] the next time the database
    /// is opened, so deleting the row would only look like it worked. Retiring it
    /// is the operation that lasts.
    pub fn is_builtin(&self) -> bool {
        Self::builtin()
            .iter()
            .any(|b| b.id == self.id && b.version == self.version)
    }

    /// Everything that must hold before an edited template is stored: the fields
    /// a procedure cannot do without, the bounds, [`Self::validate`], and a
    /// **plan against [`RenderContext::sample`]**.
    ///
    /// The trial plan is the part that earns its keep. It resolves every
    /// `{{variable}}` and asks every step for the parameters its kind reads, so an
    /// unknown variable or a missing `slot` is refused here — at the desk, by the
    /// person who typed it — rather than in front of a key with a holder waiting.
    /// The store goes through this gate too, so nothing reaches the database that
    /// cannot be planned.
    pub fn check(&self) -> Result<(), TemplateError> {
        check_id(&self.id)?;
        if self.name.trim().is_empty() {
            return Err(TemplateError::Missing("a name"));
        }
        bound("the name", self.name.trim(), crate::domain::MAX_TEXT)?;
        if self.description.trim().is_empty() {
            return Err(TemplateError::Missing(
                "a description — it is what the operator reads before running it",
            ));
        }
        bound(
            "the description",
            self.description.trim(),
            crate::domain::MAX_NOTE,
        )?;
        if self.steps.is_empty() {
            return Err(TemplateError::Missing("at least one step"));
        }
        if self.steps.len() > MAX_STEPS {
            return Err(TemplateError::TooManySteps(MAX_STEPS));
        }
        for step in &self.steps {
            step.check()?;
        }
        self.applicability.check()?;
        self.validate()?;
        // Every step is planned, including the ones that arrive disabled: the
        // wizard can enable an optional step on any run, and a step that only
        // breaks when somebody ticks it is a trap.
        let mut all = self.clone();
        for step in &mut all.steps {
            step.enabled = true;
        }
        plan(&all, &RenderContext::sample()).map(|_| ())
    }

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

        // Ordering constraint, found the hard way on a 5.7.4 key: once
        // `forcePINChange` is set, the authenticator refuses the PIN for
        // everything except changing it. So every FIDO2 step that authenticates
        // with the PIN has to come *before* the step that marks the key.
        //
        // Checked here rather than left to the executor because the failure is a
        // property of the template, not of the run: it would fail identically on
        // every key, every time, and the right place to say so is the moment the
        // procedure is written — which is also when `check()` runs, before
        // anything can be stored.
        if let Some(marker) = self
            .steps
            .iter()
            .position(|s| s.enabled && s.kind == StepKind::Fido2ForcePinChange)
            && let Some(later) = self.steps[marker + 1..]
                .iter()
                .find(|s| s.enabled && s.kind.needs_fido2_pin())
        {
            return Err(TemplateError::PinLockedBeforeUse {
                marker: self.steps[marker].id.clone(),
                later: later.id.clone(),
            });
        }

        Ok(())
    }

    pub fn enabled_steps(&self) -> impl Iterator<Item = &TemplateStep> {
        self.steps.iter().filter(|s| s.enabled)
    }

    /// The default procedure: FIDO2 PIN + OTP access code + on-key FIDO2
    /// credential + PIV signing certificate bound to the holder's e-mail.
    ///
    /// Named for what it is — the organisation's standard procedure — and not for
    /// any particular organisation: the unit that runs this tool sets its own name
    /// in Settings, and `{{org}}` carries it into the steps. A unit that wants a
    /// different procedure edits or duplicates this one from the Templates screen.
    ///
    /// This mirrors `docs/bootstrap-procedure.md`; keep the two in step.
    pub fn org_standard() -> Self {
        Self {
            id: "org-standard".into(),
            name: "Organisation standard bootstrap".into(),
            // **Version 2**, and the bump is the fix rather than an increment.
            //
            // Version 1 shipped with `fido2-force-pin-change` ahead of
            // `fido2-credential`, an ordering that cannot complete on real
            // hardware: the mark takes the PIN out of use, so the credential step
            // is handed a PIN the authenticator refuses. `validate()` now catches
            // it, which is what an operator opening v1 in the editor sees.
            //
            // It is a new version rather than a correction to v1 because seeding
            // deliberately never overwrites a stored `(id, version)` — a run
            // recorded "org-standard v1", and rewriting what that version *said*
            // would rewrite what a key was told to have applied to it. So v1
            // stays on record, broken and explainable, and the wizard offers v2
            // because `latest_per_id` takes the newest version that is not
            // retired.
            version: "2".into(),
            description: "The standard procedure for {{org}}: FIDO2 PIN (transport, forced \
                          change on first use), OTP slot access code, initial on-key FIDO2 \
                          credential, and a PIV signing certificate carrying the holder's \
                          e-mail."
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
                // **Last of the FIDO2 steps, and it has to be.** A key marked
                // `forcePINChange` refuses its PIN for everything except changing
                // it, so any FIDO2 step placed after this one — the credential in
                // particular — is handed a PIN the authenticator will no longer
                // accept. This procedure used to mark the key before creating the
                // credential, and could not have completed on real hardware;
                // found on a 5.7.4 key, and now caught by `check()` and by the
                // mock. See `features/bootstrap-engine.md` ordering rule 5.
                TemplateStep::new(
                    "fido2-force-pin-change",
                    StepKind::Fido2ForcePinChange,
                    "Require {{holder.name}} to change the transport PIN before first use",
                )
                .with_param("enforcement", "firmware-if-available")
                .optional(),
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
            // **Unrestricted, deliberately.** The standard procedure is the one a
            // unit applies to whatever it bought, and a rule here would be this
            // build guessing at a fleet it has never seen. Phase 3 exists so a unit
            // can write the rule it actually has — two key models, or a procedure a
            // later firmware changed out from under — not so the shipped template
            // can carry one.
            applicability: Applicability::default(),
            // The built-ins ship **unsigned**, and that is not an omission. A
            // signature is only worth what the key behind it is worth, and this
            // build has no organisation's key to sign with — shipping one signed by
            // the author of the tool would say something untrue about who approved
            // the procedure for a given deployment. A unit that signs its templates
            // exports these, has them signed, and imports them back.
            signature: None,
        }
    }

    /// A minimal template for keys that only need WebAuthn.
    ///
    /// Its steps are a filtered view of [`Self::org_standard`], **including their
    /// order** — so its version tracks that procedure's rather than being written
    /// here. The alternative was tried and failed: `org-standard` was bumped to v2
    /// to carry the corrected forced-change ordering into registers seeded by an
    /// older build, this constructor picked the fix up for free, and yet every such
    /// register kept a broken `fido-only` v1, because seeding asks whether an
    /// `(id, version)` exists and not whether it is correct. Deriving the version
    /// means a correction to the procedure this one is cut from cannot again reach
    /// the code and stop at the database. The cost is a version whose steps are
    /// unchanged when a bump only touched steps this subset drops — a spare entry in
    /// the catalogue, which is the cheaper of the two failures.
    pub fn fido_only() -> Self {
        let full = Self::org_standard();
        Self {
            id: "fido-only".into(),
            name: "FIDO2 only".into(),
            version: full.version.clone(),
            description: "FIDO2 PIN plus the initial on-key credential. No PIV, no OTP.".into(),
            steps: full
                .steps
                .into_iter()
                .filter(|s| {
                    matches!(
                        s.kind,
                        StepKind::Fido2Pin
                            | StepKind::Fido2MinPinLength
                            | StepKind::Fido2ForcePinChange
                            | StepKind::Fido2Credential
                            | StepKind::Verify
                    )
                })
                .collect(),
            applicability: full.applicability,
            signature: None,
        }
    }

    /// Templates shipped with the app.
    pub fn builtin() -> Vec<Self> {
        vec![Self::org_standard(), Self::fido_only()]
    }

    /// The wording this build ships for an id, so the editor can offer *restore
    /// the built-in steps* after an edit that went wrong.
    pub fn builtin_for(id: &str) -> Option<Self> {
        Self::builtin().into_iter().find(|b| b.id == id)
    }
}

/// The newest version of one template id among the candidates.
pub fn latest_of<'a>(
    templates: &'a [BootstrapTemplate],
    id: &str,
) -> Option<&'a BootstrapTemplate> {
    templates
        .iter()
        .filter(|t| t.id == id)
        .max_by(|a, b| version_order(&a.version).cmp(&version_order(&b.version)))
}

/// One entry per template id: the newest version of each, ordered by name.
///
/// This is what the bootstrap wizard offers. Older versions stay in the database
/// because runs refer to them, but offering them for a *new* run would be
/// offering a procedure that has already been superseded.
pub fn latest_per_id(templates: &[BootstrapTemplate]) -> Vec<BootstrapTemplate> {
    let mut ids: Vec<&str> = templates.iter().map(|t| t.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut out: Vec<BootstrapTemplate> = ids
        .into_iter()
        .filter_map(|id| latest_of(templates, id).cloned())
        .collect();
    out.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
    out
}

/// Versions of one id present in a list, for [`crate::versioning::next_version`].
pub fn versions_of(templates: &[BootstrapTemplate], id: &str) -> Vec<String> {
    templates
        .iter()
        .filter(|t| t.id == id)
        .map(|t| t.version.clone())
        .collect()
}

/// The audit event, target and detail for a version of a template being stored.
///
/// `previous` is the version the editor had open — `None` when the id had nothing
/// on record, which makes the entry an *addition* rather than an *edit*. Lives
/// next to the model so the shape of the entry is covered by a test rather than
/// buried in paint-adjacent code.
///
/// The detail names the versions and counts the steps; the steps themselves are
/// in the database under that version, so there is no reason to copy a procedure
/// into an audit row that can never be corrected.
pub fn edit_audit_entry(
    stored: &BootstrapTemplate,
    previous: Option<&str>,
) -> (&'static str, String, String) {
    let event = match previous {
        Some(_) => "template.changed",
        None => "template.created",
    };
    let target = format!("template:{}", stored.id);
    let detail = format!(
        "id={} version={} previous={} name={} steps={} enabled={}",
        stored.id,
        stored.version,
        previous.unwrap_or("none"),
        stored.name,
        stored.steps.len(),
        stored.enabled_steps().count(),
    );
    (event, target, detail)
}

/// A template as the database holds it: the procedure, plus the three facts the
/// Templates screen needs and the template itself cannot know.
///
/// `runs` is why this type exists. "Can this be removed?" is not a property of a
/// procedure but of what refers to it, and the operator should be able to read
/// the answer *before* clicking rather than as a refusal afterwards.
#[derive(Debug, Clone)]
pub struct StoredTemplate {
    pub template: BootstrapTemplate,
    /// Withdrawn from the wizard, kept on record. `None` while in use.
    pub retired_at: Option<String>,
    /// How many bootstrap runs recorded this exact `(id, version)`.
    pub runs: usize,
    pub updated_at: String,
}

impl StoredTemplate {
    pub fn is_retired(&self) -> bool {
        self.retired_at.is_some()
    }

    /// Why this version cannot be deleted, or `None` when it can.
    ///
    /// Two refusals, both about honesty rather than safety:
    ///
    /// * A version a **run refers to** must stay: a run that says it applied
    ///   `org-standard v1` and no `org-standard v1` to look up is not a record.
    ///   Retiring withdraws it from the wizard and keeps the evidence.
    /// * A **built-in** of this build is re-created the next time the database is
    ///   opened, so deleting it would only appear to work.
    ///
    /// In both cases the message names retirement, which is the operation that
    /// does what the operator asked for.
    pub fn removal_refusal(&self) -> Option<String> {
        if self.runs > 0 {
            return Some(format!(
                "{} bootstrap run(s) recorded this version — retire it instead, which withdraws \
                 it from the wizard and keeps the record",
                self.runs
            ));
        }
        if self.template.is_builtin() {
            return Some(
                "this version is shipped with the application and would be re-created the next \
                 time the database is opened — retire it instead"
                    .to_owned(),
            );
        }
        None
    }

    /// One-line detail for the audit entry of a retirement or a removal.
    pub fn audit_detail(&self) -> String {
        format!(
            "id={} version={} runs={} steps={} builtin={}",
            self.template.id,
            self.template.version,
            self.runs,
            self.template.steps.len(),
            self.template.is_builtin()
        )
    }
}
