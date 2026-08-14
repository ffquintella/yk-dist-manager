//! What happens to a key after the hand-over: the loss report, what the loss
//! obliges, and the record that each obligation was met
//! (`features/key-lifecycle-and-revocation.md` phases 2, 3, 4, 6 and 8).
//!
//! ## Why any of this is stored rather than derived
//!
//! `KeyStatus::Lost` was already reachable from the Inventory screen, and on its
//! own it is the theatre the feature file warns about: a status that says *lost*
//! while the certificate is still valid and the credential still works. What was
//! missing is not the status — it is everything the status obliges:
//!
//! * **who reported it, when, and in what circumstances** ([`KeyIncident`]);
//! * **what the key was carrying**, which is not a field anywhere: it is the
//!   evidence the bootstrap run left behind, read back out of the run
//!   ([`dependencies`]);
//! * **that each of those has been dealt with** ([`Remediation`]) — the
//!   certificate revoked at whichever CA issued it, the credential removed at the
//!   relying party, the applets returned to factory default;
//! * **that a returned key was sanitised before it went out again**
//!   ([`sanitisation`]), which is the one rule here that refuses an operation
//!   rather than recording one.
//!
//! ## The obligations are *recorded*, not performed
//!
//! Two of them cannot be performed by this tool, and saying so is the honest
//! shape rather than a gap:
//!
//! * **Revocation** happens at the CA, and the decision of 2026-08-13 is that the
//!   issuer is the operator (`features/ca-integration.md` phase 1) — the tool
//!   produces the request, somebody has it signed, the certificate comes back. The
//!   same path in reverse is the one that exists for revocation: the operator
//!   revokes it wherever it was issued, and the register records the reason and the
//!   CA's own reference. An automated revocation is an automation of that path
//!   (phase 3's later half), never a replacement for it.
//! * **Removal at the relying party** happens at the relying party, which for the
//!   FIDO2 credential this tool registers is a service somebody else runs. What
//!   this tool holds — and the reason `features/step-fido2-credentials.md` phase 4
//!   recorded them at all — is the credential ids, so the operator knows *what* to
//!   remove and the register can say it was removed.
//!
//! So the value here is not automation: it is that after an incident nobody has to
//! remember what was on the key, and nothing that was on it can be quietly
//! forgotten. [`outstanding`] is the list somebody has to work through, and it
//! comes from the run record rather than from memory.

use std::collections::BTreeSet;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::device::reset::Applet;
use crate::domain::bootstrap::{BootstrapRun, StepKind, StepStatus};
use crate::domain::{ValidationError, optional_note, optional_text, require_text};

/// Why a key is out of the holder's hands and not coming back as it went out.
///
/// Two kinds, not one, because they are not the same event: a key left in a taxi
/// may turn up, and a key taken from a bag is evidence of intent. Both revoke the
/// same credentials — the difference is what the incident note says and what the
/// unit does next, which is why the kind is recorded rather than flattened into
/// `Lost`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IncidentKind {
    Lost,
    Stolen,
}

impl IncidentKind {
    pub const ALL: [IncidentKind; 2] = [IncidentKind::Lost, IncidentKind::Stolen];

    pub fn label(&self) -> &'static str {
        match self {
            IncidentKind::Lost => "Lost",
            IncidentKind::Stolen => "Stolen",
        }
    }

    /// Stable snake-case name, for the database column and for audit details.
    pub fn audit_name(&self) -> &'static str {
        match self {
            IncidentKind::Lost => "lost",
            IncidentKind::Stolen => "stolen",
        }
    }
}

/// The reason a certificate was revoked, in the vocabulary the CRL uses.
///
/// RFC 5280 §5.3.1 names these, and the name matters beyond bookkeeping: a
/// relying party that checks a CRL is told *why*, and `keyCompromise` is the only
/// one of them that invalidates signatures made before the revocation date. A key
/// somebody else may be holding is a compromised key, which is why that is the
/// default an incident offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RevocationReason {
    KeyCompromise,
    AffiliationChanged,
    Superseded,
    CessationOfOperation,
    Unspecified,
}

impl RevocationReason {
    pub const ALL: [RevocationReason; 5] = [
        RevocationReason::KeyCompromise,
        RevocationReason::AffiliationChanged,
        RevocationReason::Superseded,
        RevocationReason::CessationOfOperation,
        RevocationReason::Unspecified,
    ];

    /// The RFC 5280 name, which is what a CA's own interface asks for.
    pub fn audit_name(&self) -> &'static str {
        match self {
            RevocationReason::KeyCompromise => "keyCompromise",
            RevocationReason::AffiliationChanged => "affiliationChanged",
            RevocationReason::Superseded => "superseded",
            RevocationReason::CessationOfOperation => "cessationOfOperation",
            RevocationReason::Unspecified => "unspecified",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            RevocationReason::KeyCompromise => "keyCompromise — the key may be in other hands",
            RevocationReason::AffiliationChanged => "affiliationChanged — the holder has left",
            RevocationReason::Superseded => "superseded — a replacement was issued",
            RevocationReason::CessationOfOperation => "cessationOfOperation — the key is retired",
            RevocationReason::Unspecified => "unspecified",
        }
    }

    /// What an incident of this kind is asking for, before the operator changes it.
    pub fn for_incident(kind: IncidentKind) -> Self {
        match kind {
            // Both, and deliberately: a key nobody can produce is a key whose
            // private material is unaccounted for, whether it was taken or dropped.
            IncidentKind::Lost | IncidentKind::Stolen => RevocationReason::KeyCompromise,
        }
    }
}

/// One report that a key is gone: when, who said so, and what they said.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyIncident {
    pub id: Uuid,
    pub key_serial: u32,
    pub kind: IncidentKind,
    /// When the key went missing, or the closest the report gets to it. Typed by
    /// the operator, because a loss is reported after it happened.
    pub reported_at: DateTime<Utc>,
    /// Who reported it — usually the holder, sometimes their manager or a
    /// security desk. Not the operator: that is [`Self::recorded_by`].
    pub reported_by: String,
    /// The holder as the register knew them at the time, copied rather than
    /// joined so the report still reads after a holder row is edited.
    pub holder_display: String,
    /// What happened, in the reporter's terms. Free text, bounded, and never
    /// quoted into the audit trail — an audit entry cannot be corrected, and this
    /// is a field that gets corrected.
    pub circumstances: String,
    pub recorded_at: DateTime<Utc>,
    pub recorded_by: String,
    /// Set when every obligation the incident raised has been dealt with, or
    /// deliberately waived with a reason.
    pub closed_at: Option<DateTime<Utc>>,
    pub closing_note: String,
}

impl KeyIncident {
    /// Build a report, refusing one that names nobody.
    ///
    /// `reported_by` is required and everything else optional, which is the one
    /// judgement in this constructor: a report with no circumstances is thin but
    /// usable, and a report nobody's name is attached to is not a report — it is
    /// the register asserting a loss on its own authority.
    pub fn new(
        key_serial: u32,
        kind: IncidentKind,
        reported_at: DateTime<Utc>,
        reported_by: &str,
        holder_display: &str,
        circumstances: &str,
        recorded_by: &str,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            id: Uuid::new_v4(),
            key_serial,
            kind,
            reported_at,
            reported_by: require_text("reported_by", reported_by)?,
            holder_display: optional_text("holder", holder_display)?,
            circumstances: optional_note("circumstances", circumstances)?,
            recorded_at: Utc::now(),
            recorded_by: optional_text("recorded_by", recorded_by)?,
            closed_at: None,
            closing_note: String::new(),
        })
    }

    /// Secret-free audit detail. The circumstances are summarised by length, for
    /// the reason [`crate::domain::key::note_audit_detail`] does the same.
    pub fn audit_detail(&self) -> String {
        format!(
            "kind={} reported_at={} reported_by={} holder={} circumstance_chars={}",
            self.kind.audit_name(),
            self.reported_at.to_rfc3339(),
            self.reported_by,
            if self.holder_display.is_empty() {
                "(unrecorded)"
            } else {
                &self.holder_display
            },
            self.circumstances.chars().count()
        )
    }

    pub fn is_open(&self) -> bool {
        self.closed_at.is_none()
    }
}

/// A date typed as `YYYY-MM-DD`, at midnight UTC.
///
/// The one input on the loss form that is not free text. Parsed here rather than
/// in the screen because a refusal is a rule, and rules in paint code are not
/// covered by the gate: an operator typing yesterday's date must not be able to
/// record a loss dated `31/02`, and an empty field means *today* rather than an
/// error, because the common case is a loss reported the day it happened.
pub fn parse_report_date(value: &str, today: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(today);
    }
    let date = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
        .map_err(|_| format!("`{trimmed}` is not a date — write it as YYYY-MM-DD"))?;
    let at = Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight is a valid time"));
    if at > today {
        return Err(format!(
            "{trimmed} is in the future — a loss is reported after it happened"
        ));
    }
    Ok(at)
}

/// One thing the incident obliges somebody to do, and it was recorded as done.
///
/// Three kinds share one table because they share one shape: an operator saying
/// *this specific thing has been dealt with, elsewhere, and here is the
/// reference*. What differs is the subject — a certificate serial, a credential
/// id, a set of applets — and that is a column rather than three tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemediationKind {
    /// The certificate was revoked at the CA that issued it.
    CertificateRevoked,
    /// The credential was removed at the relying party it was registered with.
    CredentialRemoved,
    /// The applets named in the subject were returned to factory default.
    Sanitised,
}

impl RemediationKind {
    pub fn audit_name(&self) -> &'static str {
        match self {
            RemediationKind::CertificateRevoked => "certificate-revoked",
            RemediationKind::CredentialRemoved => "credential-removed",
            RemediationKind::Sanitised => "sanitised",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            RemediationKind::CertificateRevoked => "certificate revoked",
            RemediationKind::CredentialRemoved => "credential removed",
            RemediationKind::Sanitised => "sanitised",
        }
    }

    /// The audit event this remediation writes.
    ///
    /// Named in `features/key-lifecycle-and-revocation.md`, and one per kind
    /// rather than a single `key.remediated` with a field, because the event name
    /// is what the audit filter offers and "show me the revocations" is a question
    /// somebody asks.
    pub fn audit_event(&self) -> &'static str {
        match self {
            RemediationKind::CertificateRevoked => "key.certificate_revoked",
            RemediationKind::CredentialRemoved => "key.credential_removed",
            RemediationKind::Sanitised => "key.sanitised",
        }
    }
}

/// The separator between applet slugs in a [`RemediationKind::Sanitised`]
/// subject, matching the `applets=fido2+piv+otp` form the reset's own audit
/// entries use.
const APPLET_JOIN: char = '+';

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Remediation {
    pub id: Uuid,
    pub key_serial: u32,
    /// The incident this answers, when there is one. A sanitisation before
    /// reissue has no incident, which is why it is optional.
    pub incident_id: Option<Uuid>,
    pub kind: RemediationKind,
    /// What was dealt with: a certificate serial, a credential id, or the applets
    /// that were reset joined by `+`.
    pub subject: String,
    /// The CA's revocation reference, the relying party's ticket, or the reset run
    /// — whatever lets somebody else check this claim.
    pub reference: String,
    /// For a revocation, the RFC 5280 reason. Empty for the other kinds.
    pub reason: String,
    pub recorded_at: DateTime<Utc>,
    pub recorded_by: String,
    pub detail: String,
}

/// The four free-text fields a remediation carries, so the constructor below takes
/// *what was done* rather than a row of six strings whose order a caller has to
/// remember.
struct Wording<'a> {
    subject: &'a str,
    reference: &'a str,
    reason: &'a str,
    detail: &'a str,
}

impl Remediation {
    fn new(
        key_serial: u32,
        incident_id: Option<Uuid>,
        kind: RemediationKind,
        recorded_by: &str,
        wording: Wording<'_>,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            id: Uuid::new_v4(),
            key_serial,
            incident_id,
            kind,
            subject: require_text("subject", wording.subject)?,
            reference: optional_text("reference", wording.reference)?,
            reason: optional_text("reason", wording.reason)?,
            recorded_at: Utc::now(),
            recorded_by: optional_text("recorded_by", recorded_by)?,
            detail: optional_note("detail", wording.detail)?,
        })
    }

    /// The certificate whose serial is `subject` was revoked at its issuer.
    pub fn certificate_revoked(
        key_serial: u32,
        incident_id: Option<Uuid>,
        certificate_serial: &str,
        reason: RevocationReason,
        reference: &str,
        recorded_by: &str,
        detail: &str,
    ) -> Result<Self, ValidationError> {
        Self::new(
            key_serial,
            incident_id,
            RemediationKind::CertificateRevoked,
            recorded_by,
            Wording {
                subject: certificate_serial,
                reference,
                reason: reason.audit_name(),
                detail,
            },
        )
    }

    /// The credential whose id is `subject` was removed at `relying_party`.
    pub fn credential_removed(
        key_serial: u32,
        incident_id: Option<Uuid>,
        credential_id: &str,
        relying_party: &str,
        reference: &str,
        recorded_by: &str,
    ) -> Result<Self, ValidationError> {
        Self::new(
            key_serial,
            incident_id,
            RemediationKind::CredentialRemoved,
            recorded_by,
            Wording {
                subject: credential_id,
                reference,
                reason: "",
                detail: &format!("rp={relying_party}"),
            },
        )
    }

    /// These applets are at factory default, so nothing of the previous holder's
    /// is on the key.
    pub fn sanitised(
        key_serial: u32,
        applets: &[Applet],
        reference: &str,
        recorded_by: &str,
        detail: &str,
    ) -> Result<Self, ValidationError> {
        let subject = applets
            .iter()
            .map(|a| a.slug())
            .collect::<Vec<_>>()
            .join(&APPLET_JOIN.to_string());
        Self::new(
            key_serial,
            None,
            RemediationKind::Sanitised,
            recorded_by,
            Wording {
                subject: &subject,
                reference,
                reason: "",
                detail,
            },
        )
    }

    /// The applets a sanitisation covers; empty for every other kind.
    pub fn applets(&self) -> Vec<Applet> {
        if self.kind != RemediationKind::Sanitised {
            return Vec::new();
        }
        self.subject
            .split(APPLET_JOIN)
            .filter_map(|slug| Applet::ALL.into_iter().find(|a| a.slug() == slug.trim()))
            .collect()
    }

    /// Secret-free audit detail. There is nothing secret to omit — a certificate
    /// serial and a credential id are both public by construction — so this is the
    /// whole record bar the free-text note, which is summarised by length.
    pub fn audit_detail(&self) -> String {
        let mut detail = format!("subject={}", self.subject);
        if !self.reason.is_empty() {
            detail.push_str(&format!(" reason={}", self.reason));
        }
        if !self.reference.is_empty() {
            detail.push_str(&format!(" reference={}", self.reference));
        }
        if !self.detail.is_empty() {
            detail.push_str(&format!(" detail_chars={}", self.detail.chars().count()));
        }
        detail
    }
}

/// A thing that was on the key, which somebody now has to deal with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyKind {
    /// An X.509 certificate, which stays valid until it is revoked.
    Certificate,
    /// A FIDO2 credential, which stays registered until the relying party
    /// removes it.
    Credential,
    /// The access code that write-protects an OTP slot.
    OtpAccessCode,
    /// Where the secrets this run set went — the sealed envelope, usually.
    Custody,
}

impl DependencyKind {
    pub fn label(&self) -> &'static str {
        match self {
            DependencyKind::Certificate => "PIV certificate",
            DependencyKind::Credential => "FIDO2 credential",
            DependencyKind::OtpAccessCode => "OTP access code",
            DependencyKind::Custody => "secret custody",
        }
    }

    /// The remediation that answers this kind, if one can.
    ///
    /// `None` for the two that are not somebody's action: an access code and a
    /// sealed envelope are facts to take account of, not tickets to close.
    pub fn settled_by(&self) -> Option<RemediationKind> {
        match self {
            DependencyKind::Certificate => Some(RemediationKind::CertificateRevoked),
            DependencyKind::Credential => Some(RemediationKind::CredentialRemoved),
            DependencyKind::OtpAccessCode | DependencyKind::Custody => None,
        }
    }

    /// What has to happen, said as an instruction rather than as a category.
    pub fn instruction(&self) -> &'static str {
        match self {
            DependencyKind::Certificate => {
                "revoke it at the CA that issued it, then record the reference here"
            }
            DependencyKind::Credential => {
                "remove it at the relying party, then record that here — the credential id is \
                 what the relying party stores"
            }
            DependencyKind::OtpAccessCode => {
                "the code travelled to the holder in the sealed envelope, so a protected slot \
                 cannot be reprogrammed without it — a reset of the OTP applet is the way back"
            }
            DependencyKind::Custody => {
                "the secrets this run set went here; nothing was retained by this tool"
            }
        }
    }
}

/// One entry in the list an incident produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub kind: DependencyKind,
    /// The certificate serial, the credential id, the slot, or the custody
    /// location — whatever identifies this one thing.
    pub subject: String,
    /// Operator-facing context, read off the run that produced it.
    pub detail: String,
    /// The run this came from, so the evidence can be looked up.
    pub run_id: Uuid,
    pub applied_at: Option<DateTime<Utc>>,
}

impl Dependency {
    /// The remediation that settles this entry, if one has been recorded.
    ///
    /// Matched on the subject, case-insensitively, because a credential id is hex
    /// and an operator pasting it from a relying party's console may paste it in
    /// either case. A certificate serial has the same problem for the same reason.
    pub fn settled_by<'a>(&self, remediations: &'a [Remediation]) -> Option<&'a Remediation> {
        let wanted = self.kind.settled_by()?;
        let subject = normalise_subject(&self.subject);
        remediations
            .iter()
            .find(|r| r.kind == wanted && normalise_subject(&r.subject) == subject)
    }
}

/// Compare a serial or a credential id without caring about case, `0x` or the
/// colons a CA's console puts between the bytes.
fn normalise_subject(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The `name=value` token a step detail carries, if it carries it.
///
/// The same reading [`crate::bootstrap::credential_evidence`] does, and for the
/// same reason: the evidence a run leaves lives in its step details, so a register
/// written years ago answers this question without a schema change.
fn field(detail: &str, name: &str) -> Option<String> {
    detail
        .split_whitespace()
        .find_map(|token| token.strip_prefix(&format!("{name}=")))
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
}

/// Everything the runs against this key say was put on it.
///
/// Read off the run record rather than stored as a list, which is the same choice
/// `credential_evidence` made and for a stronger reason here: a dependency list
/// that was written when the key was bootstrapped would be a second truth about
/// what a run did, and the run is the evidence. It also means a register that
/// predates this feature produces a full list — there is nothing to backfill.
///
/// Only steps that **succeeded** count. A failed certificate import left nothing
/// on the key to revoke, and listing it would send somebody to a CA to revoke a
/// certificate that was never installed.
pub fn dependencies(runs: &[BootstrapRun]) -> Vec<Dependency> {
    let mut items = Vec::new();
    for run in runs {
        for step in run.steps.iter().filter(|s| s.status == StepStatus::Done) {
            match step.kind {
                StepKind::PivCertImport => {
                    let serial = field(&step.detail, "serial")
                        .unwrap_or_else(|| "(serial not recorded)".to_owned());
                    items.push(Dependency {
                        kind: DependencyKind::Certificate,
                        subject: serial,
                        detail: step.detail.lines().next().unwrap_or_default().to_owned(),
                        run_id: run.id,
                        applied_at: step.finished_at,
                    });
                }
                StepKind::Fido2Credential => {
                    let Some(id) = field(&step.detail, "credential_id") else {
                        continue;
                    };
                    let rp = field(&step.detail, "rp_id").unwrap_or_default();
                    items.push(Dependency {
                        kind: DependencyKind::Credential,
                        subject: id,
                        detail: if rp.is_empty() {
                            "relying party not recorded".to_owned()
                        } else {
                            format!("relying party {rp}")
                        },
                        run_id: run.id,
                        applied_at: step.finished_at,
                    });
                }
                StepKind::OtpAccessCode => {
                    items.push(Dependency {
                        kind: DependencyKind::OtpAccessCode,
                        subject: field(&step.detail, "slot")
                            .map(|slot| format!("slot {slot}"))
                            .unwrap_or_else(|| "OTP slot".to_owned()),
                        detail: step.detail.lines().next().unwrap_or_default().to_owned(),
                        run_id: run.id,
                        applied_at: step.finished_at,
                    });
                }
                _ => {}
            }
        }
        if !run.custody.trim().is_empty() {
            items.push(Dependency {
                kind: DependencyKind::Custody,
                subject: run.custody.trim().to_owned(),
                detail: format!("{} {}", run.template_id, run.template_version),
                run_id: run.id,
                applied_at: run.finished_at,
            });
        }
    }
    items
}

/// The entries nobody has dealt with yet — the working list after an incident.
pub fn outstanding<'a>(
    dependencies: &'a [Dependency],
    remediations: &[Remediation],
) -> Vec<&'a Dependency> {
    dependencies
        .iter()
        .filter(|dependency| {
            dependency.kind.settled_by().is_some() && dependency.settled_by(remediations).is_none()
        })
        .collect()
}

/// Which applets still carry a previous holder's credentials, and which are
/// known clear (`features/key-lifecycle-and-revocation.md` phase 6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sanitisation {
    /// Applets a run wrote to, with no factory reset recorded since.
    pub outstanding: Vec<Applet>,
    /// Applets returned to factory default after the last run that wrote to them.
    pub cleared: Vec<(Applet, DateTime<Utc>)>,
}

impl Sanitisation {
    /// True when nothing a run wrote is still on the key.
    pub fn is_clear(&self) -> bool {
        self.outstanding.is_empty()
    }

    /// One line for a badge or a status bar.
    pub fn describe(&self) -> String {
        if self.outstanding.is_empty() {
            return match self.cleared.len() {
                0 => "nothing was ever written to this key by this tool".to_owned(),
                _ => format!(
                    "sanitised: {}",
                    self.cleared
                        .iter()
                        .map(|(applet, _)| applet.label())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
        }
        format!(
            "not sanitised: {}",
            self.outstanding
                .iter()
                .map(|a| a.label())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    /// Why this key may not be reissued yet, in the words the operator needs.
    ///
    /// It names the action rather than the rule: the refusal is only useful if it
    /// points at the factory reset, which is on the same screen.
    pub fn refusal(&self, serial: u32) -> String {
        format!(
            "serial {serial} still carries what a bootstrap put on it ({}), so it cannot go back \
             into stock or be bootstrapped for somebody else. Return those applets to factory \
             default from *Attached now* — or, if the key was reset elsewhere, record the \
             sanitisation. A key that carried one holder's credentials to another is the failure \
             this refusal exists to prevent",
            self.outstanding
                .iter()
                .map(|a| a.label())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// Which applet a step writes to, or `None` for a step that writes to no applet.
pub fn applet_written_by(kind: StepKind) -> Option<Applet> {
    match kind {
        StepKind::Fido2Pin
        | StepKind::Fido2MinPinLength
        | StepKind::Fido2ForcePinChange
        | StepKind::Fido2Credential => Some(Applet::Fido2),
        StepKind::OtpAccessCode | StepKind::OtpSlotConfig => Some(Applet::Otp),
        StepKind::PivPinPuk
        | StepKind::PivManagementKey
        | StepKind::PivKeygen
        | StepKind::PivCertImport => Some(Applet::Piv),
        // A CSR is produced *from* the key without changing it, and verification
        // is a read. Neither leaves anything behind to sanitise.
        StepKind::PivCsr | StepKind::Verify => None,
    }
}

/// Work out what still has to be reset before this key may be reissued.
///
/// Per applet, and by time rather than by count: an applet is clear when a
/// sanitisation covering it was recorded **after** the last run that wrote to it.
/// Counting resets would let one reset years ago clear a key bootstrapped last
/// week, and treating the whole key as one unit would refuse a key whose PIV
/// applet was reset because its OTP slots never were.
///
/// A run with no `finished_at` — one still going, or one interrupted — is treated
/// as having written *now*, because the honest reading of an unfinished run is
/// that whatever it managed to write is still there.
pub fn sanitisation(runs: &[BootstrapRun], remediations: &[Remediation]) -> Sanitisation {
    let mut written: Vec<(Applet, DateTime<Utc>)> = Vec::new();
    for run in runs {
        for step in run.steps.iter().filter(|s| s.status == StepStatus::Done) {
            let Some(applet) = applet_written_by(step.kind) else {
                continue;
            };
            let at = step
                .finished_at
                .or(run.finished_at)
                .unwrap_or_else(Utc::now);
            match written.iter_mut().find(|(a, _)| *a == applet) {
                Some(entry) if entry.1 < at => entry.1 = at,
                Some(_) => {}
                None => written.push((applet, at)),
            }
        }
    }

    let cleared_at = |applet: Applet| -> Option<DateTime<Utc>> {
        remediations
            .iter()
            .filter(|r| r.kind == RemediationKind::Sanitised && r.applets().contains(&applet))
            .map(|r| r.recorded_at)
            .max()
    };

    let mut state = Sanitisation::default();
    // In `Applet::ALL` order rather than the order the runs happened to write, so
    // two registers with the same facts produce the same sentence.
    for applet in Applet::ALL {
        let Some((_, wrote_at)) = written.iter().find(|(a, _)| *a == applet) else {
            continue;
        };
        match cleared_at(applet) {
            Some(reset_at) if reset_at >= *wrote_at => state.cleared.push((applet, reset_at)),
            _ => state.outstanding.push(applet),
        }
    }
    state
}

/// The applets a reset actually cleared, from what the reset reported.
///
/// `Skipped` counts: the reset engine reports it for an applet that was **already
/// at factory default**, which is the state a sanitisation is asserting. `Failed`
/// does not, and that is the whole point of reading the outcomes rather than the
/// request — an operator who ticked three applets and got two is not owed a
/// record saying three.
pub fn cleared_by(outcomes: &[crate::device::reset::Outcome]) -> Vec<Applet> {
    use crate::device::reset::Status;
    let cleared: BTreeSet<Applet> = outcomes
        .iter()
        .filter(|o| matches!(o.status, Status::Done | Status::Skipped))
        .map(|o| o.applet)
        .collect();
    Applet::ALL
        .into_iter()
        .filter(|a| cleared.contains(a))
        .collect()
}

/// Where an RMA case has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RmaState {
    /// Sent to the supplier, nothing back yet.
    Sent,
    /// A replacement arrived, and its serial is on the case.
    Replaced,
    /// Closed with no replacement — a refund, a rejection, or a key written off.
    Closed,
}

impl RmaState {
    pub fn label(&self) -> &'static str {
        match self {
            RmaState::Sent => "sent",
            RmaState::Replaced => "replaced",
            RmaState::Closed => "closed",
        }
    }
}

/// A key sent back to the supplier, and what came back
/// (`features/key-lifecycle-and-revocation.md` phase 8).
///
/// Why the register needs this at all: a faulty key leaves the unit physically
/// but not administratively, and the two questions an audit asks — "where is
/// serial 20423633?" and "which key replaced it?" — have no answer in a register
/// that only knows the key is `Retired`. The replacement is a *link* rather than
/// a copy, so the new key keeps its own row, its own hand-overs and its own runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RmaCase {
    pub id: Uuid,
    pub key_serial: u32,
    /// The supplier's case number. Required: an RMA nobody can quote is an RMA
    /// nobody can chase.
    pub reference: String,
    pub sent_at: DateTime<Utc>,
    pub sent_by: String,
    pub fault: String,
    pub replacement_serial: Option<u32>,
    pub replaced_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub notes: String,
}

impl RmaCase {
    pub fn open(
        key_serial: u32,
        reference: &str,
        fault: &str,
        sent_at: DateTime<Utc>,
        sent_by: &str,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            id: Uuid::new_v4(),
            key_serial,
            reference: require_text("reference", reference)?,
            sent_at,
            sent_by: optional_text("sent_by", sent_by)?,
            fault: optional_note("fault", fault)?,
            replacement_serial: None,
            replaced_at: None,
            closed_at: None,
            notes: String::new(),
        })
    }

    pub fn state(&self) -> RmaState {
        if self.replacement_serial.is_some() {
            RmaState::Replaced
        } else if self.closed_at.is_some() {
            RmaState::Closed
        } else {
            RmaState::Sent
        }
    }

    pub fn is_open(&self) -> bool {
        self.state() == RmaState::Sent
    }

    pub fn audit_detail(&self) -> String {
        let mut detail = format!(
            "reference={} sent_at={}",
            self.reference,
            self.sent_at.date_naive()
        );
        if let Some(serial) = self.replacement_serial {
            detail.push_str(&format!(" replacement={serial}"));
        }
        detail
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::bootstrap::{RunStatus, StepOutcome};

    fn run_with(steps: Vec<StepOutcome>) -> BootstrapRun {
        let mut run = BootstrapRun::new(20423633, None, "org-standard", "v2", "op", steps);
        run.status = RunStatus::Completed;
        run.finished_at = Some(Utc::now());
        run
    }

    fn done(kind: StepKind, detail: &str) -> StepOutcome {
        let mut step = StepOutcome::planned(kind.slug(), kind, detail);
        step.status = StepStatus::Done;
        step.finished_at = Some(Utc::now());
        step
    }

    #[test]
    fn the_dependency_list_names_the_certificate_and_the_credential() {
        let run = run_with(vec![
            done(
                StepKind::PivCertImport,
                "[native] certificate imported into slot 9c — subject=CN=Ana issuer=CN=CA \
                 serial=0A1B2C valid=2026-01-01..2027-01-01",
            ),
            done(
                StepKind::Fido2Credential,
                "[native] resident credential registered — credential_id=DEADBEEF rp_id=idp.example \
                 algorithm=ES256 user_name=ana@example.org",
            ),
        ]);

        let deps = dependencies(&[run]);
        let certificate = deps
            .iter()
            .find(|d| d.kind == DependencyKind::Certificate)
            .expect("the certificate is a dependency");
        assert_eq!(certificate.subject, "0A1B2C");
        let credential = deps
            .iter()
            .find(|d| d.kind == DependencyKind::Credential)
            .expect("the credential is a dependency");
        assert_eq!(credential.subject, "DEADBEEF");
        assert!(credential.detail.contains("idp.example"));
    }

    #[test]
    fn a_failed_step_leaves_nothing_to_revoke() {
        let mut step =
            StepOutcome::planned("piv-cert-import", StepKind::PivCertImport, "serial=99");
        step.status = StepStatus::Failed;
        assert!(dependencies(&[run_with(vec![step])]).is_empty());
    }

    #[test]
    fn a_recorded_revocation_settles_the_certificate_however_it_is_typed() {
        let run = run_with(vec![done(
            StepKind::PivCertImport,
            "certificate imported into slot 9c — serial=0a1b2c",
        )]);
        let deps = dependencies(&[run]);
        let revoked = Remediation::certificate_revoked(
            20423633,
            None,
            // The same serial as a CA console shows it: upper case, colon-separated.
            "0A:1B:2C",
            RevocationReason::KeyCompromise,
            "CRL-2026-14",
            "op",
            "",
        )
        .unwrap();

        assert!(deps[0].settled_by(std::slice::from_ref(&revoked)).is_some());
        assert!(outstanding(&deps, &[revoked]).is_empty());
        assert_eq!(outstanding(&deps, &[]).len(), 1);
    }

    #[test]
    fn custody_is_listed_but_is_nobodys_ticket() {
        let mut run = run_with(vec![]);
        run.custody = "sealed-envelope".into();
        let deps = dependencies(&[run]);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].kind, DependencyKind::Custody);
        // Nothing to close: the envelope is a fact about the hand-over.
        assert!(outstanding(&deps, &[]).is_empty());
    }

    #[test]
    fn an_applet_a_run_wrote_to_needs_a_reset_recorded_after_the_run() {
        let run = run_with(vec![
            done(StepKind::Fido2Pin, "PIN set"),
            done(StepKind::PivPinPuk, "PIN and PUK set"),
        ]);
        let state = sanitisation(std::slice::from_ref(&run), &[]);
        assert_eq!(state.outstanding, vec![Applet::Fido2, Applet::Piv]);
        assert!(!state.is_clear());

        // A reset of one applet clears that one only.
        let piv = Remediation::sanitised(20423633, &[Applet::Piv], "reset", "op", "").unwrap();
        let state = sanitisation(std::slice::from_ref(&run), std::slice::from_ref(&piv));
        assert_eq!(state.outstanding, vec![Applet::Fido2]);

        let fido2 = Remediation::sanitised(20423633, &[Applet::Fido2], "reset", "op", "").unwrap();
        let state = sanitisation(&[run], &[piv, fido2]);
        assert!(state.is_clear());
        assert_eq!(state.cleared.len(), 2);
    }

    #[test]
    fn a_reset_before_the_run_does_not_clear_it() {
        let old_reset =
            Remediation::sanitised(20423633, &Applet::ALL, "an earlier reset", "op", "").unwrap();
        // The run finished after that reset was recorded.
        let mut step = done(StepKind::Fido2Credential, "credential_id=AA rp_id=rp");
        step.finished_at = Some(old_reset.recorded_at + chrono::Duration::seconds(1));
        let state = sanitisation(&[run_with(vec![step])], &[old_reset]);
        assert_eq!(state.outstanding, vec![Applet::Fido2]);
    }

    #[test]
    fn a_key_nothing_was_written_to_is_clear_without_a_reset() {
        let state = sanitisation(&[], &[]);
        assert!(state.is_clear());
        assert!(state.describe().contains("nothing was ever written"));
    }

    #[test]
    fn only_a_reset_that_answered_counts_as_cleared() {
        use crate::device::reset::{Outcome, Status};
        let outcomes = vec![
            Outcome {
                applet: Applet::Fido2,
                transport: "ykman",
                status: Status::Done,
                detail: "reset".into(),
            },
            Outcome {
                applet: Applet::Otp,
                transport: "ykman",
                status: Status::Skipped,
                detail: "already at factory default".into(),
            },
            Outcome {
                applet: Applet::Piv,
                transport: "native",
                status: Status::Failed,
                detail: "refused".into(),
            },
        ];
        assert_eq!(cleared_by(&outcomes), vec![Applet::Fido2, Applet::Otp]);
    }

    #[test]
    fn a_report_needs_somebody_to_have_reported_it() {
        let now = Utc::now();
        assert!(matches!(
            KeyIncident::new(
                1,
                IncidentKind::Lost,
                now,
                "  ",
                "Ana",
                "left in a taxi",
                "op"
            ),
            Err(ValidationError::Missing("reported_by"))
        ));
        let incident = KeyIncident::new(
            1,
            IncidentKind::Stolen,
            now,
            "Ana",
            "Ana",
            "bag taken",
            "op",
        )
        .unwrap();
        assert!(incident.is_open());
        let detail = incident.audit_detail();
        assert!(detail.contains("kind=stolen"), "{detail}");
        // The circumstances are counted, never quoted.
        assert!(!detail.contains("bag taken"), "{detail}");
        assert!(detail.contains("circumstance_chars=9"), "{detail}");
    }

    #[test]
    fn a_report_date_is_a_date_and_is_not_in_the_future() {
        let today = Utc.with_ymd_and_hms(2026, 8, 14, 12, 0, 0).unwrap();
        assert_eq!(parse_report_date("", today).unwrap(), today);
        assert_eq!(
            parse_report_date("2026-08-13", today)
                .unwrap()
                .date_naive()
                .to_string(),
            "2026-08-13"
        );
        assert!(parse_report_date("13/08/2026", today).is_err());
        assert!(parse_report_date("2026-02-31", today).is_err());
        assert!(
            parse_report_date("2026-08-15", today)
                .unwrap_err()
                .contains("future")
        );
    }

    #[test]
    fn an_rma_case_needs_a_reference_and_reports_its_state() {
        let now = Utc::now();
        assert!(RmaCase::open(1, "   ", "dead", now, "op").is_err());
        let mut case = RmaCase::open(1, "RMA-42", "will not enumerate", now, "op").unwrap();
        assert_eq!(case.state(), RmaState::Sent);
        case.closed_at = Some(now);
        assert_eq!(case.state(), RmaState::Closed);
        case.replacement_serial = Some(2);
        assert_eq!(case.state(), RmaState::Replaced);
        assert!(case.audit_detail().contains("replacement=2"));
    }
}
