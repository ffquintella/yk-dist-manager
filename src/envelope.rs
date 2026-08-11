//! The sealed-envelope slip: the one document this tool produces that carries a
//! secret on purpose.
//!
//! Custody model B always hands a transport secret over
//! (`features/secrets-custody.md`), so the hand-over channel is part of the
//! procedure rather than an afterthought. For a desk hand-over the show-once
//! panel is enough — the operator reads the PIN to the holder and dismisses it.
//! For a **posted or couriered** key it is not: something has to travel with the
//! key, sealed, and this module renders it.
//!
//! ## Everything here is shaped by one fact
//!
//! The output contains a live PIN. That makes it unlike every other artefact in
//! this codebase, and three rules follow:
//!
//! 1. **It is never stored.** The consignment term is filed in the database
//!    because it is evidence; a slip is not evidence, it is a courier. Filing one
//!    would turn a distribution register into a credential store — the exact
//!    thing `AGENTS.md` §2 forbids and the reason model B exists. There is no
//!    function here that writes to a [`crate::store::Store`], and
//!    [`crate::domain::DocumentKind`] deliberately has no variant for it.
//! 2. **The bytes are wiped.** [`render`] returns `Zeroizing<Vec<u8>>`, so the
//!    rendered PDF — which contains the PIN in plain text — does not linger in
//!    freed memory after it has been written out.
//! 3. **It says what it is.** The slip tells the holder to destroy it after
//!    changing the PIN, and tells whoever finds it loose that it is a credential.
//!    A slip that looked like ordinary paperwork would be left on a desk.
//!
//! ## The remaining exposure, stated rather than hidden
//!
//! To be printed, the slip has to reach a printer, and on most workstations that
//! means a file. `AGENTS.md` §2 says a secret is never written to a *temporary*
//! file — so this module never chooses a path, never writes to a temp directory,
//! and returns bytes for the caller to save somewhere the operator picked
//! deliberately. [`DISPOSAL_WARNING`] is what the GUI must show alongside that
//! save, because the honest position is that a saved slip is a secret on disk
//! until the operator deletes it.

use zeroize::Zeroizing;

use crate::pdf::{self, TextDocument};
use crate::secret::{Secret, ShowOnce};

/// Shown next to any action that writes a slip to disk.
pub const DISPOSAL_WARNING: &str = "This file contains the key's transport PIN in plain text. \
     Print it, seal it with the key, and delete the file. Do not keep it, do not e-mail it, and \
     do not put it on a share.";

/// What the slip needs to know beyond the secrets themselves.
#[derive(Debug, Clone, Default)]
pub struct SlipRequest {
    pub serial: u32,
    pub model: String,
    pub holder_name: String,
    pub holder_email: String,
    pub unit: String,
    pub organisation: String,
    pub operator: String,
    /// Rendered date, `YYYY-MM-DD`. Passed in rather than read from the clock so
    /// rendering stays pure.
    pub issued_on: String,
    pub template_id: String,
    pub template_version: String,
    /// True when the key's firmware enforces the PIN change itself, false when
    /// the instruction on this slip is the only mechanism.
    ///
    /// The difference is load-bearing under model B and is why the wording
    /// changes: below firmware 5.7, and for PIV at every level, nothing stops a
    /// holder ignoring the instruction, so the slip has to be more emphatic
    /// rather than less.
    pub change_enforced_by_firmware: bool,
    /// Where a lost key is reported. From Settings; omitted if the unit has not
    /// set one, rather than printing a placeholder nobody can act on.
    pub report_loss_to: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SlipError {
    #[error("there is nothing to put on a slip: the secrets have already been dismissed")]
    NothingToShow,
    #[error("no secret on this run is one the holder carries, so a slip would be blank")]
    NothingTheHolderCarries,
    #[error(
        "the slip cannot be set in the document encoding — these characters would be lost: {0}"
    )]
    Unrepresentable(String),
}

/// The slip as text, for the operator to review before it is printed.
///
/// Returns `Zeroizing<String>`, because this string contains the PIN.
pub fn render_text(
    request: &SlipRequest,
    secrets: &ShowOnce,
) -> Result<Zeroizing<String>, SlipError> {
    let lines = body(request, secrets)?;
    Ok(Zeroizing::new(lines.join("\n")))
}

/// The slip as a PDF, ready to print.
pub fn render(request: &SlipRequest, secrets: &ShowOnce) -> Result<Zeroizing<Vec<u8>>, SlipError> {
    let lines = body(request, secrets)?;

    // Checked before rendering rather than after: a slip with `?` where a PIN
    // digit should be is worse than a refusal, because it looks like a document.
    let joined = lines.join("\n");
    let lost = pdf::unrepresentable(&joined);
    if !lost.is_empty() {
        return Err(SlipError::Unrepresentable(lost.iter().collect::<String>()));
    }

    let doc = TextDocument {
        heading: "CONFIDENTIAL — SECURITY KEY TRANSPORT PIN".into(),
        lines,
        author: request.organisation.clone(),
        // No holder name and no serial in the metadata: a slip that is mailed or
        // left in a print queue should not announce whose key it is in a field
        // every file browser shows.
        subject: "Sealed transport-secret slip".into(),
        footer: "Destroy after the PIN has been changed".into(),
        created: String::new(),
    };
    Ok(Zeroizing::new(pdf::render(&doc)))
}

/// The slip's text, shared by both outputs so they cannot say different things.
fn body(request: &SlipRequest, secrets: &ShowOnce) -> Result<Vec<String>, SlipError> {
    if secrets.is_spent() {
        return Err(SlipError::NothingToShow);
    }
    let carried: Vec<&Secret> = secrets.for_the_holder().collect();
    if carried.is_empty() {
        return Err(SlipError::NothingTheHolderCarries);
    }

    let mut lines = Vec::new();

    lines.push(String::new());
    lines.push("This slip carries the temporary PIN for the security key it was".into());
    lines.push("sealed with. It is a credential. If you have found it loose, hand".into());
    lines.push("it to the issuing unit unopened.".into());
    lines.push(String::new());
    lines.push(rule());
    lines.push(String::new());

    lines.push(format!("Key serial   : {}", request.serial));
    if !request.model.trim().is_empty() {
        lines.push(format!("Model        : {}", request.model));
    }
    lines.push(format!("Issued to    : {}", request.holder_name));
    if !request.holder_email.trim().is_empty() {
        lines.push(format!("               {}", request.holder_email));
    }
    if !request.unit.trim().is_empty() {
        lines.push(format!("Unit         : {}", request.unit));
    }
    lines.push(format!("Issued on    : {}", request.issued_on));
    lines.push(format!("Issued by    : {}", request.operator));
    if !request.template_id.trim().is_empty() {
        lines.push(format!(
            "Procedure    : {} version {}",
            request.template_id, request.template_version
        ));
    }

    lines.push(String::new());
    lines.push(rule());
    lines.push(String::new());
    lines.push("TEMPORARY SECRETS".into());
    lines.push(String::new());

    for secret in &carried {
        lines.push(format!(
            "  {:<20} {}",
            secret.kind().label(),
            secret.expose()
        ));
    }

    lines.push(String::new());
    lines.push(rule());
    lines.push(String::new());
    lines.push("WHAT YOU MUST DO".into());
    lines.push(String::new());

    if request.change_enforced_by_firmware {
        lines.push("1. The key will require you to choose your own PIN the first".into());
        lines.push("   time you use it. The PIN above works once, for that.".into());
    } else {
        // The wording is the mechanism here, so it is emphatic on purpose: the
        // firmware will not stop the holder keeping the transport PIN forever.
        lines.push("1. Change the PIN above to one only you know, before you use".into());
        lines.push("   the key for anything. The key will NOT force you to, and".into());
        lines.push("   nobody can do it for you.".into());
    }
    lines.push("2. Destroy this slip once you have changed the PIN.".into());
    lines.push("3. Do not write your new PIN down, and do not share it. It is".into());
    lines.push("   how the key proves that you are the one using it.".into());
    if !request.report_loss_to.trim().is_empty() {
        lines.push(format!(
            "4. If the key is lost or you think someone else has used it,\n   report it immediately to {}.",
            request.report_loss_to
        ));
    } else {
        lines.push("4. If the key is lost or you think someone else has used it,".into());
        lines.push("   report it to the issuing unit immediately.".into());
    }

    lines.push(String::new());
    lines.push(rule());
    lines.push(String::new());
    lines.push("The issuing unit does not keep a copy of the PIN above. If you".into());
    lines.push("lose it before changing it, the key has to be reset and re-issued.".into());

    // A multi-line entry may have been pushed as one string; split so the page
    // layout sees real lines.
    Ok(lines
        .into_iter()
        .flat_map(|l| l.split('\n').map(str::to_owned).collect::<Vec<_>>())
        .collect())
}

fn rule() -> String {
    "-".repeat(58)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::{Secret, SecretKind};

    fn request() -> SlipRequest {
        SlipRequest {
            serial: 20_423_633,
            model: "YubiKey 5 NFC".into(),
            holder_name: "Ana Silva".into(),
            holder_email: "ana@example.org".into(),
            unit: "ESI".into(),
            organisation: "Example Org".into(),
            operator: "felipe".into(),
            issued_on: "2026-08-11".into(),
            template_id: "org-standard".into(),
            template_version: "1".into(),
            change_enforced_by_firmware: false,
            report_loss_to: "servicedesk@example.org".into(),
        }
    }

    fn panel() -> ShowOnce {
        ShowOnce::new(vec![
            Secret::generate(SecretKind::Fido2Pin, 8).unwrap(),
            Secret::generate(SecretKind::PivPin, 8).unwrap(),
            Secret::generate(SecretKind::PivPuk, 8).unwrap(),
            Secret::generate(SecretKind::PivManagementKey, 0).unwrap(),
            Secret::generate(SecretKind::OtpAccessCode, 0).unwrap(),
        ])
    }

    #[test]
    fn the_slip_carries_exactly_the_secrets_the_holder_needs() {
        let panel = panel();
        let text = render_text(&request(), &panel).unwrap();

        // Presence is checked by reading the panel, not by comparing against a
        // literal — so no credential enters this repository, and the assertion
        // still proves the slip is usable. (`AGENTS.md` §4 bans asserting *on* a
        // secret value; this asserts the document contains what the holder was
        // handed.)
        for secret in panel.for_the_holder() {
            assert!(
                text.contains(secret.expose()),
                "the {} is missing, so the holder cannot use the key",
                secret.kind().label()
            );
            assert!(text.contains(secret.kind().label()));
        }

        // And nothing else. The management key is protected onto the key and the
        // OTP access code is deliberately discarded, so neither is the holder's
        // to carry — putting them on paper would create custody nobody wanted.
        for kind in [SecretKind::PivManagementKey, SecretKind::OtpAccessCode] {
            assert!(
                !text.contains(kind.label()),
                "{} must not appear on a slip",
                kind.label()
            );
        }
    }

    #[test]
    fn the_slip_identifies_the_key_and_the_person_it_belongs_to() {
        let text = render_text(&request(), &panel()).unwrap();
        assert!(text.contains("20423633"));
        assert!(text.contains("Ana Silva"));
        assert!(text.contains("org-standard"));
        assert!(
            text.contains("felipe"),
            "who issued it is part of the record"
        );
    }

    #[test]
    fn the_instruction_is_emphatic_when_the_firmware_will_not_enforce_the_change() {
        // Below firmware 5.7, and for PIV always, this sentence is the only thing
        // standing between a transport PIN and a permanent one.
        let text = render_text(&request(), &panel()).unwrap();
        assert!(text.contains("NOT force you"), "got: {}", &*text);

        let enforced = SlipRequest {
            change_enforced_by_firmware: true,
            ..request()
        };
        let text = render_text(&enforced, &panel()).unwrap();
        assert!(
            text.contains("will require you to choose"),
            "got: {}",
            &*text
        );
        assert!(!text.contains("NOT force you"));
    }

    #[test]
    fn the_slip_says_it_is_a_credential_and_tells_the_holder_to_destroy_it() {
        let text = render_text(&request(), &panel()).unwrap();
        assert!(text.contains("It is a credential"));
        assert!(text.to_lowercase().contains("destroy this slip"));
        assert!(
            text.contains("does not keep a copy"),
            "the holder has to know there is no recovery"
        );
    }

    #[test]
    fn the_loss_channel_is_named_when_the_unit_has_one_and_generic_when_not() {
        let text = render_text(&request(), &panel()).unwrap();
        assert!(text.contains("servicedesk@example.org"));

        let unset = SlipRequest {
            report_loss_to: String::new(),
            ..request()
        };
        let text = render_text(&unset, &panel()).unwrap();
        assert!(
            text.contains("issuing unit immediately"),
            "a placeholder nobody can act on is worse than a generic instruction"
        );
        assert!(!text.contains("{}"));
    }

    #[test]
    fn a_dismissed_panel_cannot_be_turned_into_a_slip() {
        // The show-once rule has to hold here too, or the panel's guarantee is
        // worthless: dismiss, then print, would be a second look.
        let mut panel = panel();
        panel.dismiss();
        assert!(matches!(
            render_text(&request(), &panel),
            Err(SlipError::NothingToShow)
        ));
    }

    #[test]
    fn a_run_that_set_nothing_the_holder_carries_produces_no_slip() {
        // A FIDO-only template with the PIN set by the holder at the desk hands
        // over nothing, so there is nothing to seal — and a blank slip in an
        // envelope is a worse outcome than no envelope.
        let panel = ShowOnce::new(vec![
            Secret::generate(SecretKind::PivManagementKey, 0).unwrap(),
            Secret::generate(SecretKind::OtpAccessCode, 0).unwrap(),
        ]);
        assert!(matches!(
            render_text(&request(), &panel),
            Err(SlipError::NothingTheHolderCarries)
        ));
    }

    #[test]
    fn the_pdf_renders_and_declares_nothing_personal_in_its_metadata() {
        let panel = panel();
        let bytes = render(&request(), &panel).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));

        // The body carries the holder's name, and must — the slip is useless
        // without it. The *metadata* must not, because a file browser and a print
        // queue both show metadata for a file nobody has opened.
        let rendered = String::from_utf8_lossy(&bytes).into_owned();
        for field in ["/Title", "/Author", "/Subject"] {
            let value = rendered
                .split_once(field)
                .and_then(|(_, rest)| rest.split_once(')'))
                .map(|(v, _)| v.to_owned())
                .unwrap_or_default();
            assert!(
                !value.contains("Ana Silva"),
                "{field} leaked the holder: {value}"
            );
            assert!(
                !value.contains("20423633"),
                "{field} leaked the serial: {value}"
            );
        }
    }

    #[test]
    fn a_name_the_encoding_cannot_set_is_refused_rather_than_printed_with_gaps() {
        // A slip with `?` where a character should be still looks like a valid
        // document, which is how a wrong PIN gets typed.
        let awkward = SlipRequest {
            holder_name: "Łukasz Nowak".into(),
            ..request()
        };
        match render(&awkward, &panel()) {
            Err(SlipError::Unrepresentable(chars)) => assert!(chars.contains('Ł')),
            other => panic!("expected a refusal naming the characters, got: {other:?}"),
        }
    }

    #[test]
    fn the_disposal_warning_says_what_the_operator_has_to_do() {
        assert!(DISPOSAL_WARNING.contains("delete"));
        assert!(DISPOSAL_WARNING.contains("plain text"));
    }
}
