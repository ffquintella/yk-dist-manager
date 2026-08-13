//! What each step kind actually does, and how it decides it has nothing to do.
//!
//! One function per step would be tidier to read and worse to maintain: the
//! interesting part is not the call, it is the **idempotency check** in front of
//! it, and keeping those together makes it possible to see at a glance that every
//! step has one. `features/bootstrap-engine.md` rule 4 is the requirement —
//! re-running a template on a part-configured key must detect the already-applied
//! state and skip, not blindly overwrite.
//!
//! Why that matters concretely: under custody model B the holder is *told* to
//! change the transport PIN. A second run that blindly called `set_pin` would
//! either fail (the transport PIN is no longer current) or, worse, succeed and
//! silently replace a PIN the holder chose — putting the key back into a state
//! the holder does not know about.

use std::collections::BTreeMap;

use crate::device::write::{CredentialRequest, WriteError};
use crate::domain::StepKind;
use crate::secret::{Secret, SecretKind};
use crate::template::TemplateStep;
use crate::template::plan::PlannedCommand;

use super::{RunRecorder, Transports};

/// The facts a step needs that come from the run rather than the template.
pub struct StepContext<'a> {
    pub serial: u32,
    pub relying_party: &'a str,
    pub holder_display: &'a str,
    pub certificate_subject: &'a str,
    pub certificate_email: &'a str,
}

/// How a step ended, short of failing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcomeKind {
    /// Something was written.
    Applied { detail: String },
    /// The key already had it, so nothing was written.
    AlreadyApplied { detail: String },
    /// This key or this build cannot do it, and that is not an error — a
    /// firmware gate, or a step waiting on a decision nobody has made yet.
    NotApplicable { detail: String },
}

impl StepOutcomeKind {
    fn applied(detail: impl Into<String>) -> Self {
        StepOutcomeKind::Applied {
            detail: detail.into(),
        }
    }

    fn already(detail: impl Into<String>) -> Self {
        StepOutcomeKind::AlreadyApplied {
            detail: detail.into(),
        }
    }

    fn skip(detail: impl Into<String>) -> Self {
        StepOutcomeKind::NotApplicable {
            detail: detail.into(),
        }
    }
}

/// Perform one step.
///
/// `secrets` accumulates what this run generated: a step that needs a secret
/// another step already produced (the PIV PIN authenticates key generation, the
/// CSR and the certificate import) reuses it rather than generating a second one.
pub fn perform(
    command: &PlannedCommand,
    step: &TemplateStep,
    ctx: &StepContext<'_>,
    transports: &mut Transports<'_>,
    secrets: &mut Vec<Secret>,
    recorder: &mut dyn RunRecorder,
) -> Result<StepOutcomeKind, WriteError> {
    let params = &step.params;
    let serial = ctx.serial;

    match command.kind {
        StepKind::Fido2Pin => {
            let state = transports.backend.fido2_state(serial)?;
            if state.pin_set {
                // We cannot change a PIN whose current value we do not hold, and
                // under model B the holder may already have replaced it. Skipping
                // is the only honest option; overwriting is not available to us
                // and failing would be misleading.
                return Ok(StepOutcomeKind::already(
                    "a FIDO2 PIN is already set on this key; it was left alone",
                ));
            }
            let index = ensure(
                secrets,
                SecretKind::Fido2Pin,
                pin_length(params),
                recorder,
                &step.id,
            )?;
            transports.backend.set_pin(serial, &secrets[index])?;
            Ok(StepOutcomeKind::applied("[native] FIDO2 transport PIN set"))
        }

        StepKind::Fido2MinPinLength => {
            let wanted = number(params, "length").unwrap_or(6) as u8;
            let state = transports.backend.fido2_state(serial)?;
            if state
                .min_pin_length
                .is_some_and(|current| current >= wanted)
            {
                return Ok(StepOutcomeKind::already(format!(
                    "minimum PIN length is already at least {wanted}"
                )));
            }
            let Some(index) = find(secrets, SecretKind::Fido2Pin) else {
                return Ok(StepOutcomeKind::skip(
                    "no FIDO2 PIN was set by this run, so the policy could not be applied",
                ));
            };
            transports
                .backend
                .set_min_pin_length(serial, wanted, &secrets[index])?;
            Ok(StepOutcomeKind::applied(format!(
                "[native] minimum FIDO2 PIN length set to {wanted}"
            )))
        }

        StepKind::Fido2ForcePinChange => {
            let state = transports.backend.fido2_state(serial)?;
            if state.force_pin_change_set {
                return Ok(StepOutcomeKind::already(
                    "the key already requires the holder to change the PIN",
                ));
            }
            let Some(index) = find(secrets, SecretKind::Fido2Pin) else {
                return Ok(StepOutcomeKind::skip(
                    "no FIDO2 PIN was set by this run, so the forced change could not be marked",
                ));
            };
            transports
                .backend
                .force_pin_change(serial, &secrets[index])?;
            // The step that makes model B real where the firmware supports it.
            recorder
                .audit(
                    "secret.change_enforcement",
                    &format!("serial:{serial}"),
                    "step=fido2-force-pin-change enforcement=enforced-by-firmware",
                )
                .ok();
            Ok(StepOutcomeKind::applied(
                "[native] the holder must replace the transport PIN before first use",
            ))
        }

        StepKind::Fido2Credential => {
            let state = transports.backend.fido2_state(serial)?;
            if state.resident_credentials > 0 {
                return Ok(StepOutcomeKind::already(format!(
                    "the key already holds {} resident credential(s)",
                    state.resident_credentials
                )));
            }
            let Some(index) = find(secrets, SecretKind::Fido2Pin) else {
                return Ok(StepOutcomeKind::skip(
                    "a resident credential needs the PIN this run did not set",
                ));
            };
            let request = CredentialRequest {
                relying_party: text(params, "rp").unwrap_or(ctx.relying_party.to_owned()),
                relying_party_name: ctx.relying_party.to_owned(),
                user_name: ctx.certificate_email.to_owned(),
                user_display_name: ctx.holder_display.to_owned(),
                // The whole point of the step: `ykman` cannot create one at all.
                resident: true,
                require_user_verification: true,
            };
            let evidence = transports
                .backend
                .make_credential(serial, &request, &secrets[index])?;
            Ok(StepOutcomeKind::applied(format!(
                "[native] resident credential {} created for {}",
                evidence.credential_id_hex, evidence.relying_party
            )))
        }

        StepKind::OtpAccessCode => {
            let state = transports.backend.otp_state(serial)?;
            if state.access_code_set {
                return Ok(StepOutcomeKind::already(
                    "an OTP access code is already set; the slot stays as it is",
                ));
            }
            let slot = number(params, "slot").unwrap_or(2) as u8;
            let index = ensure(secrets, SecretKind::OtpAccessCode, 0, recorder, &step.id)?;
            transports
                .backend
                .set_access_code(serial, slot, &secrets[index])?;
            Ok(StepOutcomeKind::applied(format!(
                "[native] OTP slot {slot} write-protected"
            )))
        }

        StepKind::OtpSlotConfig => {
            let slot = number(params, "slot").unwrap_or(1) as u8;
            let state = transports.backend.otp_state(serial)?;
            let already = if slot == 1 {
                state.slot_one_programmed
            } else {
                state.slot_two_programmed
            };
            if already {
                return Ok(StepOutcomeKind::already(format!(
                    "OTP slot {slot} is already programmed"
                )));
            }
            let configuration = text(params, "mode").unwrap_or_else(|| "challenge-response".into());
            let code = find(secrets, SecretKind::OtpAccessCode).map(|i| &secrets[i]);
            transports
                .backend
                .program_slot(serial, slot, &configuration, code)?;
            Ok(StepOutcomeKind::applied(format!(
                "[native] OTP slot {slot} programmed as {configuration}"
            )))
        }

        StepKind::PivPinPuk => {
            let state = transports.backend.piv_state(serial)?;
            if state.pin_changed_from_default {
                return Ok(StepOutcomeKind::already(
                    "the PIV PIN is no longer the factory default; it was left alone",
                ));
            }
            let pin = ensure(secrets, SecretKind::PivPin, 0, recorder, &step.id)?;
            let puk = ensure(secrets, SecretKind::PivPuk, 0, recorder, &step.id)?;
            // Two disjoint borrows of the vector are not expressible, so the
            // values are read out by index in one step.
            let (pin_secret, puk_secret) = pair(secrets, pin, puk);
            transports
                .backend
                .set_pin_and_puk(serial, None, pin_secret, None, puk_secret)?;
            // Model B is procedural for PIV at every firmware level: there is no
            // force-change flag, so the term is the only mechanism.
            recorder
                .audit(
                    "secret.change_enforcement",
                    &format!("serial:{serial}"),
                    "step=piv-pin-puk enforcement=instructed-on-handover",
                )
                .ok();
            Ok(StepOutcomeKind::applied(
                "[native] PIV transport PIN and PUK set",
            ))
        }

        StepKind::PivManagementKey => {
            let state = transports.backend.piv_state(serial)?;
            if state.management_key_changed {
                return Ok(StepOutcomeKind::already(
                    "the PIV management key is already not the default",
                ));
            }
            let Some(pin_index) = find(secrets, SecretKind::PivPin) else {
                return Ok(StepOutcomeKind::skip(
                    "protecting the management key needs the PIV PIN this run did not set",
                ));
            };
            let key_index = ensure(secrets, SecretKind::PivManagementKey, 0, recorder, &step.id)?;
            let protect = flag(params, "protect").unwrap_or(true);
            let (key, pin) = pair(secrets, key_index, pin_index);
            transports
                .backend
                .set_management_key(serial, None, key, protect, pin)?;
            Ok(StepOutcomeKind::applied(if protect {
                "[native] random management key generated and PIN-protected on the key"
            } else {
                "[native] management key replaced"
            }))
        }

        StepKind::PivKeygen => {
            let slot = text(params, "slot").unwrap_or_else(|| "9c".into());
            let state = transports.backend.piv_state(serial)?;
            if state.slot_occupied(&slot) {
                return Ok(StepOutcomeKind::already(format!(
                    "PIV slot {slot} already holds a certificate; generating would destroy it"
                )));
            }
            let Some(pin) = find(secrets, SecretKind::PivPin) else {
                return Ok(StepOutcomeKind::skip(
                    "key generation authenticates with the PIV PIN this run did not set",
                ));
            };
            let algorithm = text(params, "algorithm").unwrap_or_else(|| "ECCP256".into());
            let evidence =
                transports
                    .backend
                    .generate_key(serial, &slot, &algorithm, &secrets[pin])?;
            // The attestation goes into the step's detail, which is what the run
            // record keeps (`features/device-detection.md` phase 6). A public
            // certificate, so there is nothing here that must not persist — and
            // storing it is the difference between "generated on the device" as a
            // claim and as something an auditor can check years later, against
            // Yubico's attestation root, without the key in hand.
            //
            // Its absence is stated rather than omitted: a step detail that simply
            // did not mention attestation would read the same whether the proof was
            // missing or nobody had looked.
            let proof = match &evidence.attestation_pem {
                Some(pem) => format!(
                    "; attestation ({} bytes) proves on-device generation\n{pem}",
                    pem.len()
                ),
                None => "; NO attestation — on-device generation is unproven for this key                          (firmware below 4.3, or the applet refused)"
                    .to_owned(),
            };
            Ok(StepOutcomeKind::applied(format!(
                "[native] {} key generated on the device in slot {}{proof}",
                evidence.algorithm, evidence.slot
            )))
        }

        StepKind::PivCsr => {
            let slot = text(params, "slot").unwrap_or_else(|| "9c".into());
            let Some(pin) = find(secrets, SecretKind::PivPin) else {
                return Ok(StepOutcomeKind::skip(
                    "a CSR is signed by the on-device key, which needs the PIV PIN",
                ));
            };
            let subject = text(params, "subject").unwrap_or(ctx.certificate_subject.to_owned());
            // The SAN comes from the deployment's policy (`crate::san`), already
            // rendered into the context, not from the template: which form it
            // takes follows from *which CA* issues the certificate, and that is
            // one answer per deployment rather than one per procedure. A
            // template may still override it, for the unit that genuinely needs
            // two procedures with different SANs.
            let san = text(params, "san_email").unwrap_or(ctx.certificate_email.to_owned());
            let csr =
                transports
                    .backend
                    .create_csr(serial, &slot, &subject, &san, &secrets[pin])?;
            Ok(StepOutcomeKind::applied(format!(
                "[native] CSR produced for {subject} with rfc822Name={san} ({} bytes)",
                csr.len()
            )))
        }

        StepKind::PivCertImport => {
            // The one step blocked on a decision rather than on code. Open
            // question 1 in roadmap.md: which CA issues the signing certificate?
            // Until that is answered there is no issued certificate to import, so
            // the step reports why rather than failing as if something broke.
            Ok(StepOutcomeKind::skip(
                "no certificate to import: the issuing CA is not decided yet \
                 (features/ca-integration.md, roadmap open question 1)",
            ))
        }

        StepKind::Verify => {
            let fido2 = transports.backend.fido2_state(serial)?;
            let piv = transports.backend.piv_state(serial)?;
            let otp = transports.backend.otp_state(serial)?;
            // Re-read the attestation for the signing slot rather than trusting the
            // keygen step's copy: verification exists to check the key as it is now,
            // and a proof carried forward from an earlier step would be evidence about
            // that step instead. Absent is recorded, not silent.
            let slot = text(params, "slot").unwrap_or_else(|| "9c".into());
            let attested = match transports.backend.attest(serial, &slot) {
                Ok(pem) => format!("attested_{slot}=yes ({} bytes)", pem.len()),
                Err(e) => format!("attested_{slot}=no ({e})"),
            };
            Ok(StepOutcomeKind::applied(format!(
                "[native] read back: fido2_pin={} force_change={} credentials={} \
                 piv_slots=[{}] mgmt_key_changed={} piv_pin_retries={} otp_access_code={} {attested}",
                fido2.pin_set,
                fido2.force_pin_change_set,
                fido2.resident_credentials,
                piv.occupied_slots.join(","),
                piv.management_key_changed,
                match piv.pin_retries {
                    Some(left) => left.to_string(),
                    None => "unread".to_owned(),
                },
                otp.access_code_set
            )))
        }
    }
}

/// Find a secret this run has already generated.
fn find(secrets: &[Secret], kind: SecretKind) -> Option<usize> {
    secrets.iter().position(|s| s.kind() == kind)
}

/// Get the secret of this kind, generating it if the run has not needed one yet.
fn ensure(
    secrets: &mut Vec<Secret>,
    kind: SecretKind,
    length: usize,
    recorder: &mut dyn RunRecorder,
    step_id: &str,
) -> Result<usize, WriteError> {
    if let Some(index) = find(secrets, kind) {
        return Ok(index);
    }
    let secret = Secret::generate(kind, length).map_err(|e| WriteError::Failed {
        operation: "secret generation",
        reason: e.to_string(),
    })?;
    // Length and kind, never the value.
    recorder
        .audit("secret.generated", "", &secret.audit_detail(step_id))
        .ok();
    secrets.push(secret);
    Ok(secrets.len() - 1)
}

/// Two secrets from the same vector, by index.
///
/// The borrow checker will not hand out two `&` into a `Vec` through indexing in
/// one expression when a `&mut` is live, and `split_at_mut` gymnastics would be
/// worse to read than this.
fn pair(secrets: &[Secret], first: usize, second: usize) -> (&Secret, &Secret) {
    (&secrets[first], &secrets[second])
}

fn text(params: &BTreeMap<String, String>, key: &str) -> Option<String> {
    params
        .get(key)
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn number(params: &BTreeMap<String, String>, key: &str) -> Option<u32> {
    text(params, key).and_then(|v| v.parse().ok())
}

fn flag(params: &BTreeMap<String, String>, key: &str) -> Option<bool> {
    text(params, key).map(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "yes" | "1"))
}

/// The PIN length a template asked for, floored by the domain minimum.
fn pin_length(params: &BTreeMap<String, String>) -> usize {
    number(params, "length").unwrap_or(6) as usize
}
