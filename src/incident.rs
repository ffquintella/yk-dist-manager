//! The incident note: what to send when a key is lost or stolen
//! (`features/key-lifecycle-and-revocation.md` phase 7).
//!
//! ## Why the tool writes this rather than the operator
//!
//! The norm treats a possible credential compromise as an incident to be reported
//! to the ESI, and the report has to say what was on the key. That is precisely
//! the fact nobody remembers: a certificate serial, the relying party a resident
//! credential was registered with, whether the OTP slots were protected by a code
//! that travelled to the holder. All of it is in the register, written by the run
//! that put it there — so a note assembled from the record is both faster to
//! produce and more accurate than one written from memory the morning after.
//!
//! ## What it deliberately does *not* do
//!
//! It does not send anything. Who reports an incident, to whom, and inside what
//! deadline is process rather than software, and a tool that mailed the ESI would
//! be making a decision that is not its to make. The note is text the operator
//! reads, checks and sends however their unit sends things — with the PDF for the
//! case where it has to be filed as a document.
//!
//! It also does not claim to be complete, and says so in the note. Two kinds of
//! dependency cannot be derived from the register: a key that unlocks something
//! this tool does not know about (a database password by challenge-response, an
//! SSH authorised key, a service registration nobody recorded), and a credential
//! created outside a bootstrap run. Those get a *check by hand* section rather
//! than silence, because a note that reads as exhaustive is worse than one that
//! names its own blind spot.

use chrono::{DateTime, Utc};

use crate::domain::lifecycle::{
    Dependency, DependencyKind, KeyIncident, Remediation, RemediationKind,
};
use crate::domain::{Holder, YubiKeyRecord};
use crate::pdf::{self, TextDocument};

/// Everything the note is written from. Borrowed, and every field optional except
/// the incident itself: a note about a key whose inventory row was never completed
/// is still a note worth producing.
#[derive(Debug, Clone, Copy)]
pub struct NoteRequest<'a> {
    pub incident: &'a KeyIncident,
    pub key: Option<&'a YubiKeyRecord>,
    pub holder: Option<&'a Holder>,
    pub dependencies: &'a [Dependency],
    pub remediations: &'a [Remediation],
    /// The unit or organisation, from Settings. Omitted rather than guessed.
    pub organisation: &'a str,
    /// Who prepared the note — the operator, as everywhere else.
    pub prepared_by: &'a str,
    /// Where a loss is reported, from Settings. The one line that turns the note
    /// from a record into an action.
    pub report_to: &'a str,
    /// Passed in rather than read from the clock, so rendering is pure and the
    /// tests do not depend on when they run.
    pub prepared_at: DateTime<Utc>,
}

/// The note's title, used as the PDF heading and the suggested filename stem.
pub fn heading(incident: &KeyIncident) -> String {
    format!(
        "Security incident — YubiKey serial {} {}",
        incident.key_serial,
        incident.kind.audit_name()
    )
}

/// A filename an operator will recognise a week later.
pub fn filename(incident: &KeyIncident) -> String {
    format!(
        "incident-{}-{}",
        incident.key_serial,
        incident.reported_at.date_naive()
    )
}

/// The note, as lines. One place, so the text on screen, the copy in the
/// clipboard and the PDF cannot disagree — the same rule the consignment term is
/// built under.
pub fn lines(request: &NoteRequest<'_>) -> Vec<String> {
    let incident = request.incident;
    let mut out: Vec<String> = Vec::new();

    let mut section = |title: &str, body: Vec<String>| {
        if !out.is_empty() {
            out.push(String::new());
        }
        out.push(title.to_owned());
        out.push("-".repeat(title.chars().count()));
        out.extend(body);
    };

    // ---------------------------------------------------------------- summary
    let mut summary = vec![
        field("Serial", &incident.key_serial.to_string()),
        field("Event", incident.kind.label()),
        field(
            "Reported on",
            &incident.reported_at.date_naive().to_string(),
        ),
        field("Reported by", &incident.reported_by),
    ];
    if !incident.holder_display.is_empty() {
        summary.push(field("Holder", &incident.holder_display));
    }
    if let Some(holder) = request.holder {
        summary.push(field("Holder e-mail", &holder.email));
        if !holder.unit.is_empty() {
            summary.push(field("Unit", &holder.unit));
        }
    }
    if let Some(key) = request.key {
        if !key.model.is_empty() {
            summary.push(field("Model", &key.model));
        }
        if !key.firmware.is_empty() {
            summary.push(field("Firmware", &key.firmware));
        }
        summary.push(field("Register status", key.status.label()));
    }
    summary.push(field(
        "Note prepared",
        &format!(
            "{} by {}",
            request.prepared_at.date_naive(),
            or_else(request.prepared_by, "(operator not recorded)")
        ),
    ));
    if !request.organisation.is_empty() {
        summary.push(field("Organisation", request.organisation));
    }
    section("Incident", summary);

    // ----------------------------------------------------------- what happened
    if !incident.circumstances.trim().is_empty() {
        section(
            "Circumstances as reported",
            wrapped(incident.circumstances.trim()),
        );
    }

    // ------------------------------------------------- what was on the key
    let mut carried: Vec<String> = Vec::new();
    if request.dependencies.is_empty() {
        carried.push(
            "Nothing: this register holds no completed bootstrap run for this serial. That is \
             not the same as an empty key — a key configured outside this tool would look the \
             same from here."
                .to_owned(),
        );
    }
    for dependency in request.dependencies {
        let settled = dependency.settled_by(request.remediations);
        let state = match (dependency.kind.settled_by(), settled) {
            (Some(_), Some(remediation)) => format!(
                "dealt with on {} ({})",
                remediation.recorded_at.date_naive(),
                describe_remediation(remediation)
            ),
            (Some(_), None) => "OUTSTANDING".to_owned(),
            (None, _) => "for information".to_owned(),
        };
        carried.push(format!(
            "* {} {} — {}",
            dependency.kind.label(),
            dependency.subject,
            state
        ));
        if !dependency.detail.is_empty() {
            carried.extend(indented(&dependency.detail));
        }
        if let Some(at) = dependency.applied_at {
            carried.push(format!("    applied {}", at.date_naive()));
        }
    }
    section("What the key was carrying", carried);

    // ------------------------------------------------------- what was done
    let mut done: Vec<String> = Vec::new();
    if request.remediations.is_empty() {
        done.push("Nothing has been recorded yet.".to_owned());
    }
    for remediation in request.remediations {
        done.push(format!(
            "* {} — {} on {}",
            remediation.kind.label(),
            remediation.subject,
            remediation.recorded_at.date_naive()
        ));
        let extra = describe_remediation(remediation);
        if !extra.is_empty() {
            done.extend(indented(&extra));
        }
    }
    section("What has been done", done);

    // --------------------------------------------------------- what is owed
    let outstanding =
        crate::domain::lifecycle::outstanding(request.dependencies, request.remediations);
    let mut owed: Vec<String> = Vec::new();
    if outstanding.is_empty() {
        owed.push(
            "Nothing this register can see. Every certificate it knows of has been revoked and \
             every credential it knows of has been removed."
                .to_owned(),
        );
    }
    for dependency in &outstanding {
        owed.push(format!(
            "* {} {} — {}",
            dependency.kind.label(),
            dependency.subject,
            dependency.kind.instruction()
        ));
    }
    section("Still to do", owed);

    // ------------------------------------------------------ the blind spot
    section(
        "Check by hand",
        vec![
            "This note is assembled from the register, so it can only name what the register \
             holds. Four things it cannot know, and each has to be checked by somebody:"
                .to_owned(),
            String::new(),
            "* services the holder registered this key with directly — the credential is on \
             the key, and a relying party this tool never spoke to has no record here"
                .to_owned(),
            "* anything the key unlocked rather than signed: a database password by \
                 challenge-response, a disk, a password manager"
                .to_owned(),
            "* an SSH authorised key derived from a PIV slot, which stays authorised until it \
                 is removed from every host"
                .to_owned(),
            "* whether the holder's other keys are affected — a shared PIN or a re-used \
                 credential is a question about the person, not about this serial"
                .to_owned(),
        ]
        .into_iter()
        .flat_map(|line| wrapped(&line))
        .collect(),
    );

    // ---------------------------------------------------------------- to whom
    let mut to = Vec::new();
    if request.report_to.trim().is_empty() {
        to.extend(wrapped(
            "No reporting address is configured in this deployment. The norm requires a \
             possible credential compromise to be reported to the ESI; set the address in \
             Settings so the next note names it.",
        ));
    } else {
        to.push(field("Report to", request.report_to.trim()));
    }
    to.push(String::new());
    to.extend(wrapped(
        "Prepared by this tool from the distribution register. Sending it, and any deadline \
         it is subject to, is the unit's process — the tool records that the note was \
         produced, not that it was sent.",
    ));
    section("Reporting", to);

    out
}

/// The note as one block of text, for the panel and the clipboard.
pub fn text(request: &NoteRequest<'_>) -> String {
    let mut rendered = lines(request).join("\n");
    rendered.push('\n');
    rendered
}

/// The note as a PDF, for a unit that files its incident reports as documents.
///
/// Not stored in the register: the incident, the dependency list and the
/// remediations are all in there already, so filing a rendering of them would be
/// a second copy that can go stale. The consignment term is filed because a
/// *signature* on it is evidence; nothing signs this.
pub fn document(request: &NoteRequest<'_>) -> TextDocument {
    TextDocument {
        heading: heading(request.incident),
        lines: lines(request),
        author: request.organisation.to_owned(),
        // Deliberately free of the holder's name: this is file metadata, and it
        // travels further than the body does.
        subject: format!(
            "Security incident note — serial {}",
            request.incident.key_serial
        ),
        footer: format!(
            "Incident note — serial {} — prepared {}",
            request.incident.key_serial,
            request.prepared_at.date_naive()
        ),
        created: pdf::pdf_date(&request.prepared_at),
    }
}

/// `Label: value`, padded so a column of them lines up.
fn field(label: &str, value: &str) -> String {
    format!("{label:<16} {value}")
}

fn or_else<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

/// Wrap to the page width, so the text on screen breaks where the PDF does.
fn wrapped(text: &str) -> Vec<String> {
    pdf::wrap(text, pdf::columns())
}

/// A continuation line under a bullet.
fn indented(text: &str) -> Vec<String> {
    pdf::wrap(text, pdf::columns().saturating_sub(4))
        .into_iter()
        .map(|line| format!("    {line}"))
        .collect()
}

/// The reference and reason a remediation carries, as one phrase.
fn describe_remediation(remediation: &Remediation) -> String {
    let mut parts = Vec::new();
    if remediation.kind == RemediationKind::CertificateRevoked && !remediation.reason.is_empty() {
        parts.push(format!("reason {}", remediation.reason));
    }
    if !remediation.reference.is_empty() {
        parts.push(format!("reference {}", remediation.reference));
    }
    if !remediation.detail.is_empty() {
        parts.push(remediation.detail.clone());
    }
    if !remediation.recorded_by.is_empty() {
        parts.push(format!("recorded by {}", remediation.recorded_by));
    }
    parts.join(", ")
}

/// Does this list still hold something nobody has dealt with?
///
/// The question the Inventory screen asks to decide whether an incident may be
/// closed, and the same one the note answers in prose.
pub fn is_settled(dependencies: &[Dependency], remediations: &[Remediation]) -> bool {
    crate::domain::lifecycle::outstanding(dependencies, remediations).is_empty()
}

/// The dependencies that are somebody's action, counted by kind, for a one-line
/// summary on a row.
pub fn summarise(dependencies: &[Dependency], remediations: &[Remediation]) -> String {
    let mut certificates = 0usize;
    let mut credentials = 0usize;
    for dependency in crate::domain::lifecycle::outstanding(dependencies, remediations) {
        match dependency.kind {
            DependencyKind::Certificate => certificates += 1,
            DependencyKind::Credential => credentials += 1,
            _ => {}
        }
    }
    match (certificates, credentials) {
        (0, 0) => "nothing outstanding".to_owned(),
        (c, 0) => format!("{c} certificate(s) to revoke"),
        (0, k) => format!("{k} credential(s) to remove"),
        (c, k) => format!("{c} certificate(s) to revoke, {k} credential(s) to remove"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::lifecycle::{IncidentKind, RevocationReason};
    use uuid::Uuid;

    fn incident() -> KeyIncident {
        KeyIncident::new(
            20_423_633,
            IncidentKind::Stolen,
            Utc::now(),
            "Ana Silva",
            "Ana Silva <ana@example.org>",
            "bag taken on the bus",
            "operator",
        )
        .unwrap()
    }

    fn certificate_dependency() -> Dependency {
        Dependency {
            kind: DependencyKind::Certificate,
            subject: "0A1B2C".into(),
            detail: "certificate imported into slot 9c".into(),
            run_id: Uuid::new_v4(),
            applied_at: Some(Utc::now()),
        }
    }

    fn request<'a>(
        incident: &'a KeyIncident,
        dependencies: &'a [Dependency],
        remediations: &'a [Remediation],
    ) -> NoteRequest<'a> {
        NoteRequest {
            incident,
            key: None,
            holder: None,
            dependencies,
            remediations,
            organisation: "Unit",
            prepared_by: "operator",
            report_to: "esi@example.org",
            prepared_at: Utc::now(),
        }
    }

    #[test]
    fn the_note_names_the_certificate_and_calls_it_outstanding() {
        let incident = incident();
        let dependencies = vec![certificate_dependency()];
        let note = text(&request(&incident, &dependencies, &[]));

        assert!(note.contains("20423633"), "{note}");
        assert!(note.contains("0A1B2C"), "{note}");
        assert!(note.contains("OUTSTANDING"), "{note}");
        assert!(note.contains("Ana Silva"), "{note}");
        assert!(note.contains("esi@example.org"), "{note}");
        // The blind spot is named rather than left implied.
        assert!(note.contains("Check by hand"), "{note}");
    }

    #[test]
    fn a_revoked_certificate_reads_as_dealt_with_and_leaves_nothing_owed() {
        let incident = incident();
        let dependencies = vec![certificate_dependency()];
        let revoked = Remediation::certificate_revoked(
            20_423_633,
            Some(incident.id),
            "0A1B2C",
            RevocationReason::KeyCompromise,
            "CRL-2026-14",
            "operator",
            "",
        )
        .unwrap();

        let note = text(&request(
            &incident,
            &dependencies,
            std::slice::from_ref(&revoked),
        ));
        assert!(note.contains("dealt with on"), "{note}");
        assert!(note.contains("keyCompromise"), "{note}");
        assert!(!note.contains("OUTSTANDING"), "{note}");
        assert!(is_settled(&dependencies, std::slice::from_ref(&revoked)));
        assert_eq!(summarise(&dependencies, &[revoked]), "nothing outstanding");
        assert_eq!(summarise(&dependencies, &[]), "1 certificate(s) to revoke");
    }

    #[test]
    fn a_deployment_with_no_reporting_address_is_told_to_set_one() {
        let incident = incident();
        let mut req = request(&incident, &[], &[]);
        req.report_to = "  ";
        let note = text(&req);
        assert!(
            note.contains("No reporting address is configured"),
            "{note}"
        );
    }

    #[test]
    fn the_note_renders_as_a_pdf_that_names_the_serial() {
        let incident = incident();
        let dependencies = vec![certificate_dependency()];
        let doc = document(&request(&incident, &dependencies, &[]));
        let bytes = pdf::render(&doc);
        assert!(bytes.starts_with(b"%PDF-"));
        assert!(doc.heading.contains("20423633"));
        // No personal data in the metadata that travels with the file.
        assert!(!doc.subject.contains("Ana"));
    }
}
