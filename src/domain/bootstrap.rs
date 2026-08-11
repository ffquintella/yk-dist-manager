//! The record of *what was applied* to a key during bootstrap.
//!
//! A [`BootstrapRun`] is the evidence attached to a distribution: which
//! template (and template version) ran, which steps succeeded, when, and under
//! which operator. No step ever stores the secret it set — only the fact that
//! it was set, and where custody of the secret went. See
//! `features/bootstrap-engine.md`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The catalogue of things the bootstrap procedure can do to a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepKind {
    /// Set or change the FIDO2 PIN (`ykman fido access change-pin`).
    Fido2Pin,
    /// Raise the minimum FIDO2 PIN length (firmware 5.7+).
    Fido2MinPinLength,
    /// Mark the FIDO2 PIN so the holder must change it before first use
    /// (CTAP 2.1, firmware 5.7+). The custody model depends on this step.
    Fido2ForcePinChange,
    /// Register the initial discoverable credential(s), resident on the key.
    Fido2Credential,
    /// Write the 6-byte access code that write-protects the OTP slots.
    OtpAccessCode,
    /// Program an OTP slot (Yubico OTP / challenge-response / static).
    OtpSlotConfig,
    /// Set the PIV PIN and PUK.
    PivPinPuk,
    /// Replace the default PIV management key.
    PivManagementKey,
    /// Generate the signing key **on the device** (never imported).
    PivKeygen,
    /// Produce a CSR for the generated key.
    PivCsr,
    /// Import the issued signing certificate into the slot.
    PivCertImport,
    /// Read the key back and confirm the end state.
    Verify,
}

impl StepKind {
    /// Every kind a template may use, in the order the standard procedure applies
    /// them — which is the order the "add a step" list offers, so a template
    /// built by hand comes out in a sensible sequence.
    pub const ALL: [StepKind; 12] = [
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
    ];

    pub fn label(&self) -> &'static str {
        match self {
            StepKind::Fido2Pin => "FIDO2 PIN",
            StepKind::Fido2MinPinLength => "FIDO2 minimum PIN length",
            StepKind::Fido2ForcePinChange => "FIDO2 forced PIN change",
            StepKind::Fido2Credential => "FIDO2 resident credential",
            StepKind::OtpAccessCode => "OTP slot access code",
            StepKind::OtpSlotConfig => "OTP slot configuration",
            StepKind::PivPinPuk => "PIV PIN + PUK",
            StepKind::PivManagementKey => "PIV management key",
            StepKind::PivKeygen => "PIV on-device key generation",
            StepKind::PivCsr => "PIV certificate request",
            StepKind::PivCertImport => "PIV certificate import",
            StepKind::Verify => "Verification",
        }
    }

    /// Id-shaped name, used as the default step id when a step is added by hand.
    ///
    /// These are the ids the built-in templates use, so a hand-built template
    /// reads like the shipped one in a run record.
    pub fn slug(&self) -> &'static str {
        match self {
            StepKind::Fido2Pin => "fido2-pin",
            StepKind::Fido2MinPinLength => "fido2-min-pin-length",
            StepKind::Fido2ForcePinChange => "fido2-force-pin-change",
            StepKind::Fido2Credential => "fido2-credential",
            StepKind::OtpAccessCode => "otp-access-code",
            StepKind::OtpSlotConfig => "otp-slot-config",
            StepKind::PivPinPuk => "piv-pin-puk",
            StepKind::PivManagementKey => "piv-management-key",
            StepKind::PivKeygen => "piv-keygen",
            StepKind::PivCsr => "piv-csr",
            StepKind::PivCertImport => "piv-cert-import",
            StepKind::Verify => "verify",
        }
    }

    /// Steps that write a secret whose custody must be recorded.
    pub fn sets_secret(&self) -> bool {
        matches!(
            self,
            StepKind::Fido2Pin
                | StepKind::OtpAccessCode
                | StepKind::PivPinPuk
                | StepKind::PivManagementKey
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepStatus {
    Pending,
    Running,
    Done,
    Failed,
    /// Not applicable to this key, or deliberately deselected by the operator.
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunStatus {
    /// Planned but not executed (dry run).
    Planned,
    Running,
    Completed,
    /// At least one required step failed.
    Failed,
    Aborted,
}

/// Outcome of a single step. `detail` is operator-facing text and must be
/// secret-free.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepOutcome {
    pub step_id: String,
    pub kind: StepKind,
    pub status: StepStatus,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub detail: String,
}

impl StepOutcome {
    pub fn planned(step_id: impl Into<String>, kind: StepKind, detail: impl Into<String>) -> Self {
        Self {
            step_id: step_id.into(),
            kind,
            status: StepStatus::Pending,
            started_at: None,
            finished_at: None,
            detail: detail.into(),
        }
    }
}

/// One execution of a bootstrap template against one key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapRun {
    pub id: Uuid,
    pub key_serial: u32,
    pub holder_id: Option<Uuid>,
    pub template_id: String,
    pub template_version: String,
    pub operator: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: RunStatus,
    pub steps: Vec<StepOutcome>,
    /// Where the secrets set by this run are kept (e.g. `sealed-envelope`,
    /// `bastionvault:kv/yubikeys/20423633`). Never the secrets themselves.
    pub custody: String,
}

impl BootstrapRun {
    pub fn new(
        key_serial: u32,
        holder_id: Option<Uuid>,
        template_id: impl Into<String>,
        template_version: impl Into<String>,
        operator: impl Into<String>,
        steps: Vec<StepOutcome>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            key_serial,
            holder_id,
            template_id: template_id.into(),
            template_version: template_version.into(),
            operator: operator.into(),
            started_at: Utc::now(),
            finished_at: None,
            status: RunStatus::Planned,
            steps,
            custody: String::new(),
        }
    }

    /// Summary counts `(done, failed, skipped, pending)`.
    pub fn tally(&self) -> (usize, usize, usize, usize) {
        let mut t = (0, 0, 0, 0);
        for step in &self.steps {
            match step.status {
                StepStatus::Done => t.0 += 1,
                StepStatus::Failed => t.1 += 1,
                StepStatus::Skipped => t.2 += 1,
                StepStatus::Pending | StepStatus::Running => t.3 += 1,
            }
        }
        t
    }

    /// A run is only `Completed` when nothing failed and nothing is left pending.
    pub fn settle(&mut self) {
        let (_, failed, _, pending) = self.tally();
        self.status = if failed > 0 {
            RunStatus::Failed
        } else if pending > 0 {
            RunStatus::Running
        } else {
            RunStatus::Completed
        };
        if self.status != RunStatus::Running {
            self.finished_at = Some(Utc::now());
        }
    }

    /// One-line summary for the distribution record and reports.
    pub fn summary(&self) -> String {
        let applied: Vec<&str> = self
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Done)
            .map(|s| s.kind.label())
            .collect();
        if applied.is_empty() {
            format!(
                "{} {} — nothing applied",
                self.template_id, self.template_version
            )
        } else {
            format!(
                "{} {} — {}",
                self.template_id,
                self.template_version,
                applied.join(", ")
            )
        }
    }
}
