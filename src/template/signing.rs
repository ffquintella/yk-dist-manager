//! Signing and verifying a bootstrap template
//! (`features/bootstrap-templates.md` phase 5).
//!
//! A template decides what gets written to security hardware. An unauthorised
//! edit of one is therefore not a data-quality problem, it is an attack: change
//! `pin_policy`, point `san_email` somewhere else, drop the step that forces the
//! holder to change the transport PIN, and every key prepared from then on is
//! quietly wrong. The audit trail records *who* changed a template, which is
//! accountability after the fact; a signature is the control that stops the
//! changed template from being used at all.
//!
//! ## This module verifies. It cannot sign, and that is deliberate
//!
//! AGENTS.md §2: no secret anywhere it can persist — not in a column, a log, a
//! settings file or a temporary file. A signing key is a secret, so the only
//! signature scheme this application can carry is a **public-key** one where it
//! holds nothing but public keys. The private half lives wherever the
//! organisation keeps its keys (an HSM, a smartcard, an offline machine), and
//! signing is an out-of-band step over [`canonical_bytes`], which is documented
//! precisely so that any tool can produce the signature.
//!
//! The practical consequence, stated because it is a real cost: a unit that edits
//! a template in this application produces an **unsigned** version, and getting a
//! signature on it means exporting it and having whoever holds the key sign it.
//! Pilot mode ([`crate::settings::AppSettings::templates_must_be_signed`] left
//! off) is what makes that workable while an organisation decides whose key it is
//! — and it is visible on screen and in the audit trail rather than silent.
//!
//! ## What the signature covers, and what it does not
//!
//! [`canonical_bytes`] covers the **procedure and its id**: the name, the
//! description, and every step in order with its kind, flags and parameters. It
//! deliberately does **not** cover the version number.
//!
//! That follows from a rule this feature already had: a version is assigned by
//! whichever database stored the template
//! ([`crate::versioning::next_version`]), so two units importing the same signed
//! procedure will number it differently. If the version were signed, importing a
//! template would break its signature — the signature would be verifying local
//! bookkeeping rather than the procedure. What matters is that nobody can change
//! *what the procedure does* without the key, and the id is covered so a signed
//! procedure cannot be re-labelled as a different template.
//!
//! ## Canonical bytes are netstrings, not `serde_json`
//!
//! Serialising the struct would make the signature depend on the struct's field
//! order and on serde's output, so adding a field would silently invalidate every
//! signature ever made. Instead the encoding is written out by hand, field by
//! field, as length-prefixed strings (`<len>:<bytes>`) which need no escaping and
//! cannot be made ambiguous by a value containing a separator. A golden-vector
//! test pins the exact bytes, so a refactor that changes the encoding fails
//! loudly instead of quietly rejecting every signed template in the field.
//!
//! Adding a field later means a new format tag ([`CANONICAL_FORMAT`]) and keeping
//! the old one for verification, which is why the tag is the first thing in the
//! bytes.

use serde::{Deserialize, Serialize};

use crate::template::BootstrapTemplate;

/// Tag identifying the canonical encoding, and the first field in it.
///
/// A signature is over one specific encoding. If the encoding ever gains a
/// field, this tag changes and the old one stays supported for verification —
/// which is only possible because the tag is inside the signed bytes.
pub const CANONICAL_FORMAT: &str = "ykdm-template-v1";

/// The tag for a template that uses a field v1 had no room for: the applicability
/// rule and the per-step attempt budget
/// (`features/bootstrap-templates.md` phases 3 and 7).
///
/// **A template that uses neither is still encoded as v1**, byte for byte, and
/// that is the whole design of this second tag. Bumping the format for every
/// template would have invalidated every signature ever made and changed every
/// fingerprint on every screen, to carry two fields that almost no template sets.
///
/// Which encoding applies is decided by the template's own contents and the tag is
/// the first thing inside the signed bytes, so the two cannot be confused: adding
/// a rule to a v1-signed template moves it to v2 and the signature stops
/// verifying, which is correct — the policy changed. Removing one does the same in
/// the other direction, which is the case that matters, since removing a
/// restriction *widens* the keys a signed procedure may be applied to.
pub const CANONICAL_FORMAT_V2: &str = "ykdm-template-v2";

/// The only signature algorithm this build knows.
///
/// Ed25519: small keys, no parameter choices to get wrong, and no way to produce
/// a valid-looking signature by picking bad parameters. Named in the signature
/// rather than assumed, so a build that meets an algorithm it does not know
/// **refuses** instead of treating the template as unsigned — see
/// [`Trust::UnknownAlgorithm`].
pub const ALGORITHM: &str = "ed25519";

/// Length of a hex-encoded Ed25519 public key (32 bytes) and signature (64).
const KEY_HEX_LEN: usize = 64;
const SIGNATURE_HEX_LEN: usize = 128;

/// A signature over a template's [`canonical_bytes`].
///
/// Stored inside the template's JSON body and carried in an exported file. It is
/// *not* part of the canonical bytes — a signature cannot cover itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateSignature {
    /// Which key signed it, as a label an operator can recognise
    /// (`esi-template-key-2026`). Matched against
    /// [`crate::settings::AppSettings::template_keys`].
    pub key_id: String,
    /// [`ALGORITHM`]. Present so an unknown one is a refusal, not a silent pass.
    pub algorithm: String,
    /// Hex, lower case. Hex rather than base64 because the audit chain and the
    /// document digests in this application are already hex, and one encoding is
    /// one fewer thing to get wrong when a signature is read out over a phone.
    pub signature: String,
}

/// A public key this deployment trusts to sign templates.
///
/// Lives in the settings file, which holds **no secret** — a public key is not
/// one, and the settings file is exactly the right place for "whose signature do
/// we accept here", because the answer is per-deployment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateKey {
    /// Matched against [`TemplateSignature::key_id`].
    pub id: String,
    /// Hex-encoded Ed25519 public key, 64 hex characters.
    pub public_key: String,
    /// Whose key it is, in the operator's words. Never load-bearing.
    #[serde(default)]
    pub comment: String,
}

impl TemplateKey {
    /// Refuse a key that cannot verify anything, at the moment it is typed.
    ///
    /// A trust store holding a malformed key is worse than an empty one: every
    /// template signed by that key id reads as *invalid* rather than as
    /// *misconfigured*, and the operator goes looking for the wrong problem.
    pub fn check(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("the key needs an id — the same label the signature carries".into());
        }
        if self.id.chars().count() > crate::template::MAX_ID {
            return Err(format!(
                "a key id is at most {} characters",
                crate::template::MAX_ID
            ));
        }
        let hex_key = self.public_key.trim();
        if hex_key.len() != KEY_HEX_LEN || hex::decode(hex_key).is_err() {
            return Err(format!(
                "an Ed25519 public key is {KEY_HEX_LEN} hex characters (32 bytes); this one is \
                 {} character(s)",
                hex_key.len()
            ));
        }
        Ok(())
    }

    fn verifying_key(&self) -> Option<ed25519_dalek::VerifyingKey> {
        let bytes: [u8; 32] = hex::decode(self.public_key.trim()).ok()?.try_into().ok()?;
        ed25519_dalek::VerifyingKey::from_bytes(&bytes).ok()
    }
}

/// What verification concluded about a template.
///
/// Every variant is a *distinct operational situation*, because collapsing them
/// would leave the operator unable to act: an unknown key id is somebody's trust
/// store missing an entry, while a bad signature over a known key is a template
/// that has been altered. Those want opposite responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// No signature at all. Usable only in pilot mode, and audited when used.
    Unsigned,
    /// Verified against a key in the trust store.
    Signed { key_id: String },
    /// Signed by a key id this deployment does not have. **Not** a valid
    /// signature: it may be genuine and the trust store incomplete, or it may be
    /// signed by anybody at all — indistinguishable from here, which is why it is
    /// never treated as signed.
    UnknownKey { key_id: String },
    /// A key we have, and a signature that does not verify against it. The
    /// template has been altered since it was signed, or the file is damaged.
    Invalid { key_id: String, reason: String },
    /// A signature naming an algorithm this build cannot check.
    UnknownAlgorithm { key_id: String, algorithm: String },
}

impl Trust {
    /// May a run use this template *without* pilot mode?
    ///
    /// Only a verified signature. Everything else — unsigned, unknown key,
    /// altered, unknown algorithm — is a refusal when signatures are required.
    pub fn is_verified(&self) -> bool {
        matches!(self, Trust::Signed { .. })
    }

    /// Words, not a colour. Every variant has its own, which
    /// `tests/unit_accessibility.rs` asserts — a badge that says nothing is one
    /// only a sighted operator with a working display can read.
    pub fn label(&self) -> &'static str {
        match self {
            Trust::Unsigned => "unsigned",
            Trust::Signed { .. } => "signed",
            Trust::UnknownKey { .. } => "signed by an unknown key",
            Trust::Invalid { .. } => "signature does not match",
            Trust::UnknownAlgorithm { .. } => "unknown signature algorithm",
        }
    }

    /// The whole sentence, including what to do about it.
    pub fn describe(&self) -> String {
        match self {
            Trust::Unsigned => "unsigned — usable only while unsigned templates are allowed".into(),
            Trust::Signed { key_id } => format!("signed by `{key_id}`, and the signature verifies"),
            Trust::UnknownKey { key_id } => format!(
                "signed by `{key_id}`, which is not in this deployment's list of template keys. \
                 Add that key in Settings if it is one you trust — until then this counts as \
                 unsigned, because a signature nobody can check is not a signature"
            ),
            Trust::Invalid { key_id, reason } => format!(
                "the signature by `{key_id}` does NOT match this procedure ({reason}). It has \
                 been altered since it was signed, or the file is damaged. Do not run it: get the \
                 template again from whoever signed it"
            ),
            Trust::UnknownAlgorithm { key_id, algorithm } => format!(
                "signed by `{key_id}` using `{algorithm}`, which this build cannot verify — it \
                 knows `{ALGORITHM}`. Treated as unverified rather than trusted"
            ),
        }
    }

    /// What the audit entry says about the trust of a template that was run.
    pub fn audit_detail(&self) -> String {
        match self {
            Trust::Unsigned => "signature=none".into(),
            Trust::Signed { key_id } => format!("signature=verified key={key_id}"),
            Trust::UnknownKey { key_id } => format!("signature=unknown_key key={key_id}"),
            Trust::Invalid { key_id, .. } => format!("signature=invalid key={key_id}"),
            Trust::UnknownAlgorithm { key_id, algorithm } => {
                format!("signature=unknown_algorithm key={key_id} algorithm={algorithm}")
            }
        }
    }
}

/// The exact bytes a signature is made over. See the module documentation.
///
/// Length-prefixed fields, so no value can be confused with a separator however
/// it is spelled:
///
/// ```text
/// 16:ykdm-template-v1 12:org-standard 8:Standard … 5:steps 2:11 …
/// ```
///
/// (spaces added for readability only — the real encoding has none).
pub fn canonical_bytes(template: &BootstrapTemplate) -> Vec<u8> {
    // v2 only when the template actually uses what v2 adds. See
    // [`CANONICAL_FORMAT_V2`]: this is what keeps every existing signature and
    // every printed fingerprint valid.
    let v2 =
        !template.applicability.is_unrestricted() || template.steps.iter().any(|s| s.attempts > 1);

    let mut out = Vec::new();
    push(
        &mut out,
        if v2 {
            CANONICAL_FORMAT_V2
        } else {
            CANONICAL_FORMAT
        },
    );
    push(&mut out, template.id.trim());
    push(&mut out, template.name.trim());
    push(&mut out, template.description.trim());
    // The count is written before the steps: without it, one template's steps
    // could be re-partitioned into another's and produce the same bytes.
    push(&mut out, &template.steps.len().to_string());
    for step in &template.steps {
        push(&mut out, step.id.trim());
        push(&mut out, step.kind.slug());
        push(&mut out, if step.enabled { "enabled" } else { "disabled" });
        push(
            &mut out,
            if step.required {
                "required"
            } else {
                "optional"
            },
        );
        push(&mut out, step.description.trim());
        push(&mut out, &step.params.len().to_string());
        // `params` is a BTreeMap, so this order is the map's own and stable.
        for (key, value) in &step.params {
            push(&mut out, key);
            push(&mut out, value);
        }
    }

    if v2 {
        // Appended rather than interleaved, so the bytes above stay exactly what
        // v1 produced and the difference between the two encodings is a suffix.
        let rule = &template.applicability;
        push(&mut out, rule.min_firmware.as_deref().unwrap_or("").trim());
        push(&mut out, rule.max_firmware.as_deref().unwrap_or("").trim());
        push(&mut out, &rule.requires_applications.len().to_string());
        for application in &rule.requires_applications {
            push(&mut out, application.trim());
        }
        // The count again, so the attempt budgets cannot be re-partitioned against
        // a different list of steps — the same reason it precedes the steps above.
        push(&mut out, &template.steps.len().to_string());
        for step in &template.steps {
            push(&mut out, &step.attempts.to_string());
        }
    }

    out
}

/// One netstring: the byte length, a colon, the bytes.
fn push(out: &mut Vec<u8>, field: &str) {
    out.extend_from_slice(field.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(field.as_bytes());
}

/// The digest of the canonical bytes, hex — for showing a template's identity on
/// screen and for an out-of-band comparison ("is your v3 the same as mine?").
///
/// Not what is signed: Ed25519 signs the message itself. This exists so two
/// people can compare procedures over a phone without reading out 128 hex
/// characters of signature.
pub fn fingerprint(template: &BootstrapTemplate) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(canonical_bytes(template));
    hex::encode(&digest[..8])
}

/// Verify a template against the deployment's trusted keys.
pub fn verify(template: &BootstrapTemplate, keys: &[TemplateKey]) -> Trust {
    let Some(signature) = &template.signature else {
        return Trust::Unsigned;
    };
    let key_id = signature.key_id.trim().to_owned();

    if signature.algorithm.trim() != ALGORITHM {
        return Trust::UnknownAlgorithm {
            key_id,
            algorithm: signature.algorithm.trim().to_owned(),
        };
    }

    let Some(key) = keys.iter().find(|k| k.id.trim() == key_id) else {
        return Trust::UnknownKey { key_id };
    };
    let Some(verifying) = key.verifying_key() else {
        // A malformed key in the trust store. Reported as invalid *naming the
        // reason*, so the operator fixes the settings rather than suspecting the
        // template.
        return Trust::Invalid {
            key_id,
            reason: "the public key in this deployment's settings is not a usable Ed25519 key"
                .into(),
        };
    };

    let hex_signature = signature.signature.trim();
    if hex_signature.len() != SIGNATURE_HEX_LEN {
        return Trust::Invalid {
            key_id,
            reason: format!("a signature is {SIGNATURE_HEX_LEN} hex characters"),
        };
    }
    let Ok(raw) = hex::decode(hex_signature) else {
        return Trust::Invalid {
            key_id,
            reason: "the signature is not hex".into(),
        };
    };
    let Ok(bytes): Result<[u8; 64], _> = raw.try_into() else {
        return Trust::Invalid {
            key_id,
            reason: format!("a signature is {SIGNATURE_HEX_LEN} hex characters"),
        };
    };

    // `verify_strict` rather than `verify`: it rejects the small-order public keys
    // and non-canonical encodings that make a signature verifiable under more
    // than one key. For a control whose whole job is "this and no other key
    // authorised this procedure", the strict check is the one that means it.
    match verifying.verify_strict(
        &canonical_bytes(template),
        &ed25519_dalek::Signature::from(bytes),
    ) {
        Ok(()) => Trust::Signed { key_id },
        Err(_) => Trust::Invalid {
            key_id,
            reason: "it does not verify over this procedure's canonical bytes".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::StepKind;
    use crate::template::TemplateStep;

    /// A named change to one field of a template, for
    /// [`every_field_of_a_step_is_covered_by_the_signature`].
    type Mutation = (&'static str, fn(&mut BootstrapTemplate));

    fn template() -> BootstrapTemplate {
        BootstrapTemplate {
            id: "unit-test".into(),
            name: "Unit test".into(),
            version: "1".into(),
            description: "A procedure for a test".into(),
            steps: vec![
                TemplateStep::new("fido-pin", StepKind::Fido2Pin, "Set the PIN")
                    .with_param("min_length", "6"),
            ],
            applicability: Default::default(),
            signature: None,
        }
    }

    /// The signing half, which the application deliberately does not have. A test
    /// may hold a private key: it is a fixture, it protects nothing, and it exists
    /// only inside this process.
    fn sign(template: &BootstrapTemplate, key_id: &str, seed: [u8; 32]) -> BootstrapTemplate {
        use ed25519_dalek::{Signer, SigningKey};
        let signing = SigningKey::from_bytes(&seed);
        let signature = signing.sign(&canonical_bytes(template));
        let mut signed = template.clone();
        signed.signature = Some(TemplateSignature {
            key_id: key_id.into(),
            algorithm: ALGORITHM.into(),
            signature: hex::encode(signature.to_bytes()),
        });
        signed
    }

    fn trust_store(key_id: &str, seed: [u8; 32]) -> Vec<TemplateKey> {
        use ed25519_dalek::SigningKey;
        vec![TemplateKey {
            id: key_id.into(),
            public_key: hex::encode(SigningKey::from_bytes(&seed).verifying_key().to_bytes()),
            comment: "the test's own key".into(),
        }]
    }

    #[test]
    fn the_canonical_encoding_is_pinned_byte_for_byte() {
        // A golden vector, because the encoding is a wire format the moment
        // anything is signed with it: a refactor that "tidies" it would reject
        // every signed template in the field, and would do it silently. If this
        // test fails, the answer is a new CANONICAL_FORMAT tag, not a new
        // expectation.
        let bytes = canonical_bytes(&template());
        let text = String::from_utf8(bytes).expect("the encoding is UTF-8");
        assert_eq!(
            text,
            "16:ykdm-template-v19:unit-test9:Unit test22:A procedure for a test1:1\
             8:fido-pin9:fido2-pin7:enabled8:required11:Set the PIN1:1\
             10:min_length1:6"
        );
    }

    #[test]
    fn the_version_is_not_signed_because_the_database_assigns_it() {
        // Importing a template renumbers it, so a signature over the version
        // would break on import — it would be verifying local bookkeeping.
        let v1 = template();
        let v7 = v1.as_version("7");
        assert_eq!(canonical_bytes(&v1), canonical_bytes(&v7));
    }

    #[test]
    fn a_valid_signature_verifies_and_names_its_key() {
        let signed = sign(&template(), "esi-2026", [7u8; 32]);
        assert_eq!(
            verify(&signed, &trust_store("esi-2026", [7u8; 32])),
            Trust::Signed {
                key_id: "esi-2026".into()
            }
        );
        assert!(verify(&signed, &trust_store("esi-2026", [7u8; 32])).is_verified());
    }

    #[test]
    fn a_signature_survives_renumbering_but_not_an_edited_step() {
        let signed = sign(&template(), "esi-2026", [7u8; 32]);
        let keys = trust_store("esi-2026", [7u8; 32]);

        // Renumbering is what the store does on save and on import.
        assert!(verify(&signed.as_version("12"), &keys).is_verified());

        // Changing what the procedure *does* is what the signature is for. This
        // is the whole point of the feature: a one-character change to a
        // parameter must not survive verification.
        let mut tampered = signed.clone();
        tampered.steps[0]
            .params
            .insert("min_length".into(), "4".into());
        match verify(&tampered, &keys) {
            Trust::Invalid { key_id, .. } => assert_eq!(key_id, "esi-2026"),
            other => panic!("a tampered template must not verify: {other:?}"),
        }
        assert!(!verify(&tampered, &keys).is_verified());
    }

    #[test]
    fn every_field_of_a_step_is_covered_by_the_signature() {
        // A signature over some of a step is a signature over none of it: an
        // attacker would simply change the field nobody covered. Each of these
        // must break verification on its own.
        let signed = sign(&template(), "k", [3u8; 32]);
        let keys = trust_store("k", [3u8; 32]);
        assert!(verify(&signed, &keys).is_verified());

        // Plain function pointers rather than boxed closures: none of these
        // captures anything, and a named type says so once instead of inline.
        let mutations: Vec<Mutation> = vec![
            ("id", |t: &mut BootstrapTemplate| t.id = "other".into()),
            ("name", |t: &mut BootstrapTemplate| t.name = "Other".into()),
            ("description", |t: &mut BootstrapTemplate| {
                t.description = "Other".into()
            }),
            ("step id", |t: &mut BootstrapTemplate| {
                t.steps[0].id = "other".into()
            }),
            ("step kind", |t: &mut BootstrapTemplate| {
                t.steps[0].kind = StepKind::Verify
            }),
            ("step description", |t: &mut BootstrapTemplate| {
                t.steps[0].description = "Other".into()
            }),
            ("enabled", |t: &mut BootstrapTemplate| {
                t.steps[0].enabled = false
            }),
            ("required", |t: &mut BootstrapTemplate| {
                t.steps[0].required = false
            }),
            ("a parameter value", |t: &mut BootstrapTemplate| {
                t.steps[0].params.insert("min_length".into(), "8".into());
            }),
            ("a parameter name", |t: &mut BootstrapTemplate| {
                t.steps[0].params.remove("min_length");
                t.steps[0].params.insert("min_len".into(), "6".into());
            }),
            ("an added step", |t: &mut BootstrapTemplate| {
                t.steps
                    .push(TemplateStep::new("v", StepKind::Verify, "Verify"));
            }),
            ("a removed step", |t: &mut BootstrapTemplate| {
                t.steps.clear()
            }),
        ];

        for (what, mutate) in mutations {
            let mut tampered = signed.clone();
            mutate(&mut tampered);
            assert!(
                !verify(&tampered, &keys).is_verified(),
                "changing {what} must break the signature"
            );
        }
    }

    #[test]
    fn reordering_the_steps_breaks_the_signature() {
        // Order is the order of execution, and this feature has already been bitten
        // by it: `org-standard` v1 could not complete on hardware because the
        // forced PIN change came before the credential step. A signature that did
        // not cover the order would authorise that swap.
        let mut two_steps = template();
        two_steps.steps.push(TemplateStep::new(
            "verify",
            StepKind::Verify,
            "Read it back",
        ));
        let signed = sign(&two_steps, "k", [9u8; 32]);
        let keys = trust_store("k", [9u8; 32]);
        assert!(verify(&signed, &keys).is_verified());

        let mut swapped = signed.clone();
        swapped.steps.swap(0, 1);
        assert!(!verify(&swapped, &keys).is_verified());
    }

    #[test]
    fn a_signature_from_another_key_does_not_verify() {
        let signed = sign(&template(), "esi-2026", [1u8; 32]);
        // Same key *id*, different key material: the attacker's own key, labelled
        // to look like the organisation's.
        let keys = trust_store("esi-2026", [2u8; 32]);
        assert!(matches!(verify(&signed, &keys), Trust::Invalid { .. }));
    }

    #[test]
    fn an_unknown_key_id_is_not_treated_as_signed() {
        // The failure mode this prevents: accepting a signature nobody can check
        // because the template says it was signed. "Signed by somebody" is not a
        // control.
        let signed = sign(&template(), "somebody-elses-key", [5u8; 32]);
        let trust = verify(&signed, &trust_store("esi-2026", [5u8; 32]));
        assert_eq!(
            trust,
            Trust::UnknownKey {
                key_id: "somebody-elses-key".into()
            }
        );
        assert!(!trust.is_verified());
        assert!(
            trust.describe().contains("Settings"),
            "{}",
            trust.describe()
        );
    }

    #[test]
    fn an_unknown_algorithm_is_refused_rather_than_ignored() {
        let mut signed = sign(&template(), "k", [4u8; 32]);
        signed.signature.as_mut().unwrap().algorithm = "rsa-pkcs1".into();
        let trust = verify(&signed, &trust_store("k", [4u8; 32]));
        assert_eq!(
            trust,
            Trust::UnknownAlgorithm {
                key_id: "k".into(),
                algorithm: "rsa-pkcs1".into()
            }
        );
        assert!(!trust.is_verified());
    }

    #[test]
    fn a_template_with_no_signature_is_unsigned_not_invalid() {
        // The distinction matters: unsigned is a deployment that has not started
        // signing, invalid is an attack or a damaged file. Reporting the first as
        // the second would train the operator to ignore the second.
        assert_eq!(verify(&template(), &[]), Trust::Unsigned);
        assert!(!verify(&template(), &[]).is_verified());
    }

    #[test]
    fn a_malformed_signature_is_invalid_and_says_why() {
        let keys = trust_store("k", [6u8; 32]);
        for (bad, expect) in [
            ("not hex at all", "hex characters"),
            ("ab", "hex characters"),
        ] {
            let mut signed = sign(&template(), "k", [6u8; 32]);
            signed.signature.as_mut().unwrap().signature = bad.into();
            match verify(&signed, &keys) {
                Trust::Invalid { reason, .. } => {
                    assert!(reason.contains(expect), "{bad}: {reason}")
                }
                other => panic!("{bad} should be Invalid, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_malformed_key_in_the_settings_blames_the_settings() {
        // The operator has to be sent to the right place. A broken trust store
        // reported as "this template has been altered" is how somebody spends an
        // afternoon on the wrong problem.
        let signed = sign(&template(), "k", [8u8; 32]);
        let keys = vec![TemplateKey {
            id: "k".into(),
            public_key: "nonsense".into(),
            comment: String::new(),
        }];
        match verify(&signed, &keys) {
            Trust::Invalid { reason, .. } => assert!(reason.contains("settings"), "{reason}"),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn a_key_is_checked_when_it_is_typed() {
        use ed25519_dalek::SigningKey;
        let good = hex::encode(
            SigningKey::from_bytes(&[1u8; 32])
                .verifying_key()
                .to_bytes(),
        );

        assert!(
            TemplateKey {
                id: "esi".into(),
                public_key: good.clone(),
                comment: String::new()
            }
            .check()
            .is_ok()
        );

        let no_id = TemplateKey {
            id: "  ".into(),
            public_key: good,
            comment: String::new(),
        };
        assert!(no_id.check().is_err());

        let short = TemplateKey {
            id: "esi".into(),
            public_key: "abcd".into(),
            comment: String::new(),
        };
        let message = short.check().expect_err("4 hex characters is not a key");
        assert!(message.contains("64"), "{message}");
    }

    #[test]
    fn the_fingerprint_follows_the_procedure_and_not_the_version() {
        let t = template();
        assert_eq!(fingerprint(&t), fingerprint(&t.as_version("9")));
        let mut other = t.clone();
        other.steps[0]
            .params
            .insert("min_length".into(), "8".into());
        assert_ne!(fingerprint(&t), fingerprint(&other));
        assert_eq!(fingerprint(&t).len(), 16, "8 bytes, hex");
    }

    #[test]
    fn every_trust_state_has_its_own_words_and_a_sentence() {
        let states = [
            Trust::Unsigned,
            Trust::Signed { key_id: "k".into() },
            Trust::UnknownKey { key_id: "k".into() },
            Trust::Invalid {
                key_id: "k".into(),
                reason: "r".into(),
            },
            Trust::UnknownAlgorithm {
                key_id: "k".into(),
                algorithm: "a".into(),
            },
        ];
        let labels: std::collections::BTreeSet<&str> = states.iter().map(|s| s.label()).collect();
        assert_eq!(labels.len(), states.len(), "{labels:?}");
        for state in &states {
            assert!(!state.describe().trim().is_empty());
            assert!(state.audit_detail().starts_with("signature="));
            // No secret anywhere: the only things near a signature are public.
            assert!(!state.audit_detail().contains("private"));
        }
        assert!(
            states.iter().filter(|s| s.is_verified()).count() == 1,
            "exactly one state may run under a signature requirement"
        );
    }
}
