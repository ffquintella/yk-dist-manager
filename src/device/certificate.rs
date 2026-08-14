//! Reading an issued certificate, and checking it before it is written to a key.
//!
//! The issuing CA is **not** integrated: the decision of 2026-08-13 is that the
//! operator brings the certificate to the tool. The CSR comes out of the run, goes
//! to whichever CA the deployment uses, and the signed certificate comes back
//! through the wizard as a file or pasted text — `features/ca-integration.md`
//! phase 1's manual/offline mode, which is the mode every other issuer is a
//! special case of.
//!
//! That makes this module load-bearing in a way an API client would not be. When a
//! CA endpoint hands back a certificate, the chain of custody is machine-checked
//! at both ends. When a human pastes one, the only check is the one here, so it is
//! done **before the write** rather than after:
//!
//! * it has to parse as an X.509 certificate;
//! * its public key has to be the key in the slot ([`must_match_public_key`]) —
//!   otherwise it imports cleanly and then fails at every signature the holder
//!   makes, which is the worst possible place to discover a mix-up between two
//!   holders' certificates;
//! * what it actually says — subject, issuer, validity, the `rfc822Name` SANs —
//!   is summarised for the operator ([`Summary`]) so a certificate for the wrong
//!   person is visible before it is written and not after.
//!
//! Pure and always compiled: no card, no network, and available in the
//! `ykman`-only build too, so a certificate can be inspected wherever it arrives.

use x509_cert::Certificate;
use x509_cert::der::{Decode, Encode};

use super::write::{Result, WriteError};

/// The largest certificate this will look at.
///
/// A PIV slot's object is bounded by the applet anyway (about 3 KiB); the limit
/// here is so a pasted file that is not a certificate at all is refused by size
/// instead of parsed. `AGENTS.md` §2 asks for a maximum on every input.
pub const MAX_CERTIFICATE_BYTES: usize = 8 * 1024;

/// What a certificate says, in the terms the operator needs to check it.
///
/// Non-secret by nature — a certificate is a public document — so this is safe to
/// show on screen, put in a step's detail and keep as evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub subject: String,
    pub issuer: String,
    pub serial: String,
    pub not_before: String,
    pub not_after: String,
    /// Every `rfc822Name` in the subject alternative name extension.
    ///
    /// Plural because a certificate may carry more than one, and the check is
    /// "is the holder's address among them", not "is it the only one".
    pub email_sans: Vec<String>,
    /// The key usages asserted, in the X.509 names
    /// (`digitalSignature`, `nonRepudiation`, …).
    ///
    /// Empty means the extension is absent, which is not the same as "none": a
    /// certificate with no key-usage extension is unconstrained. See
    /// [`Summary::signing_verdict`], which says so rather than guessing.
    pub key_usages: Vec<String>,
    /// Extended key usages, as dotted OIDs with the well-known ones named.
    pub extended_key_usages: Vec<String>,
}

impl Summary {
    /// Does this certificate carry `email` as an `rfc822Name`?
    ///
    /// The comparison is case-insensitive on the whole address. The local part is
    /// formally case-sensitive, but no corporate mail system treats it that way,
    /// and a case mismatch here would refuse a certificate that works.
    pub fn covers_email(&self, email: &str) -> bool {
        let wanted = email.trim().to_ascii_lowercase();
        !wanted.is_empty()
            && self
                .email_sans
                .iter()
                .any(|san| san.trim().to_ascii_lowercase() == wanted)
    }

    /// One line for a step's detail or a status bar.
    pub fn one_line(&self) -> String {
        format!(
            "subject={} issuer={} serial={} valid={}..{} rfc822Name=[{}] key_usage=[{}] eku=[{}]",
            self.subject,
            self.issuer,
            self.serial,
            self.not_before,
            self.not_after,
            self.email_sans.join(","),
            self.key_usages.join(","),
            self.extended_key_usages.join(","),
        )
    }

    /// Can this certificate be used for the thing slot 9c exists for — signing on
    /// the holder's behalf (`features/ca-integration.md` phase 2, check 4)?
    ///
    /// Three answers, not two, and the third is the one that matters. A CA that
    /// issued an *encryption* certificate against a signing request produces a
    /// certificate that imports cleanly and then fails every signature — the same
    /// class of failure as a mismatched public key, which is why it is checked
    /// before the write. But a certificate carrying **no** key-usage extension is
    /// unconstrained rather than wrong, and refusing it would refuse a perfectly
    /// usable certificate from a minimal internal CA.
    pub fn signing_verdict(&self) -> Fitness {
        if self.key_usages.is_empty() && self.extended_key_usages.is_empty() {
            return Fitness::NotStated;
        }
        // `digitalSignature` is the usage a signature needs; `nonRepudiation`
        // (renamed `contentCommitment`) is the stronger form a consignment context
        // may ask for, and either one permits signing.
        let signs = self
            .key_usages
            .iter()
            .any(|usage| usage == "digitalSignature" || usage == "nonRepudiation");
        // An EKU that names nothing relevant is a positive statement that this
        // certificate is for something else.
        let purpose = self.extended_key_usages.is_empty()
            || self.extended_key_usages.iter().any(|eku| {
                matches!(
                    eku.as_str(),
                    "clientAuth" | "emailProtection" | "codeSigning" | "anyExtendedKeyUsage"
                )
            });

        if self.key_usages.is_empty() {
            // EKU only: judge on that alone rather than demanding a KU the CA did
            // not assert.
            return if purpose {
                Fitness::Fit
            } else {
                Fitness::Unfit
            };
        }
        if signs && purpose {
            Fitness::Fit
        } else {
            Fitness::Unfit
        }
    }
}

/// Whether a certificate is fit for the purpose the slot is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fitness {
    /// It says it can be used for this.
    Fit,
    /// It says it cannot — an encryption-only or server certificate in a signing
    /// slot.
    Unfit,
    /// It carries neither extension, so it constrains nothing. Reported as its own
    /// answer rather than folded into either: "the CA did not say" and "the CA said
    /// yes" are different facts about a certificate somebody will sign with.
    NotStated,
}

impl Fitness {
    /// A word, never a colour — `features/gui-shell.md` phase 10.
    pub fn label(&self) -> &'static str {
        match self {
            Fitness::Fit => "usable for signing",
            Fitness::Unfit => "not issued for signing",
            Fitness::NotStated => "does not state a usage",
        }
    }
}

/// One thing checked about a certificate read back out of a slot.
///
/// `features/step-piv-signing-certificate.md` phase 7 asks for four checks and
/// this carries them uniformly, because the interesting outcome is per check: an
/// operator needs to see *which* of subject, address and usage disagreed, not one
/// boolean over all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// `subject`, `rfc822Name`, `key usage`, `chain`.
    pub name: &'static str,
    /// `None` when the check could not be made — the counterpart of
    /// [`Fitness::NotStated`], and never silently a pass.
    pub passed: Option<bool>,
    pub detail: String,
}

impl Check {
    fn new(name: &'static str, passed: Option<bool>, detail: impl Into<String>) -> Self {
        Self {
            name,
            passed,
            detail: detail.into(),
        }
    }

    /// `subject=ok`, `rfc822Name=FAILED`, `chain=unchecked` — the form a step's
    /// detail carries, so a stored run says what was verified years later.
    pub fn to_field(&self) -> String {
        format!(
            "{}={}",
            self.name.replace(' ', "_"),
            match self.passed {
                Some(true) => "ok",
                Some(false) => "FAILED",
                None => "unchecked",
            }
        )
    }
}

/// Verify a certificate read back out of a slot against what the run asked for.
///
/// The **read-back** verification, as distinct from the checks made before the
/// import: those prove the certificate is the right one to write, and these prove
/// the write landed and the card holds what the register says it does. An auditor
/// with a key in hand runs exactly this, months later, against a run record.
///
/// `expected_subject` is the rendered subject the CSR asked for. It is compared
/// **loosely** — see [`same_subject`] — because a CA is entitled to normalise a
/// distinguished name, and a check that failed on attribute order would be a check
/// that always failed.
///
/// The **chain** is reported as unchecked, deliberately and visibly: it needs a
/// trust store this tool does not have (`features/ca-integration.md` phase 2,
/// check 5). Saying so is the point — a verification that silently omitted it
/// would read as a full one.
pub fn verify_read_back(
    summary: &Summary,
    expected_subject: &str,
    expected_email: &str,
) -> Vec<Check> {
    let mut checks = Vec::new();

    checks.push(if expected_subject.trim().is_empty() {
        Check::new(
            "subject",
            None,
            "the run recorded no subject to compare against",
        )
    } else if same_subject(&summary.subject, expected_subject) {
        Check::new("subject", Some(true), summary.subject.clone())
    } else {
        Check::new(
            "subject",
            Some(false),
            format!(
                "the slot holds `{}` and this run asked for `{}`",
                summary.subject, expected_subject
            ),
        )
    });

    checks.push(if expected_email.trim().is_empty() {
        Check::new(
            "rfc822Name",
            None,
            "the run recorded no address to compare against",
        )
    } else if summary.covers_email(expected_email) {
        Check::new("rfc822Name", Some(true), expected_email.trim().to_owned())
    } else {
        Check::new(
            "rfc822Name",
            Some(false),
            format!(
                "the certificate carries [{}] and not {} — signatures made with it will not \
                 validate against the holder's address",
                summary.email_sans.join(","),
                expected_email.trim()
            ),
        )
    });

    let fitness = summary.signing_verdict();
    checks.push(Check::new(
        "key usage",
        match fitness {
            Fitness::Fit => Some(true),
            Fitness::Unfit => Some(false),
            Fitness::NotStated => None,
        },
        format!(
            "{} (key_usage=[{}] eku=[{}])",
            fitness.label(),
            summary.key_usages.join(","),
            summary.extended_key_usages.join(",")
        ),
    ));

    checks.push(Check::new(
        "chain",
        None,
        "not checked: this build has no trust store to chain to \
         (features/ca-integration.md phase 2, check 5)",
    ));

    checks
}

/// Do two distinguished names name the same subject?
///
/// Compared as a **set of attribute assertions**, case-insensitively, rather than
/// as strings. Two things make a string comparison wrong here and neither is
/// exotic: a CA may reorder the RDNs, and it may re-space `CN=Ana Silva, OU=ESI`.
/// A check that failed on either would fail on every real certificate, and a check
/// that always fails gets switched off.
///
/// What it does **not** do is normalise the values themselves — a subject with a
/// different `CN` is a different subject, which is the whole point.
fn same_subject(actual: &str, expected: &str) -> bool {
    let parts = |dn: &str| -> std::collections::BTreeSet<String> {
        dn.split(',')
            .map(|part| part.trim().to_ascii_lowercase())
            .filter(|part| !part.is_empty())
            .collect()
    };
    parts(actual) == parts(expected)
}

/// Pull the DER out of a PEM `CERTIFICATE` document, or accept raw DER.
///
/// Both are accepted because both are what a CA hands over: a `.pem`/`.crt` in
/// text, or a `.cer`/`.der` in binary. Refusing one of them would send the
/// operator to a conversion tool for no reason.
pub fn der_from_pem(text: &str) -> Option<Vec<u8>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Raw DER pasted or read from a file: a certificate is a SEQUENCE, so it
    // starts with 0x30. Text cannot, which makes this an unambiguous test.
    if trimmed.as_bytes().first() == Some(&0x30) {
        return Some(trimmed.as_bytes().to_vec());
    }

    if !trimmed.contains("-----BEGIN") {
        return None;
    }

    // Only the first document, and only a CERTIFICATE. A chain file holds the
    // leaf first, and importing an intermediate into the holder's slot is a
    // mistake worth refusing rather than guessing at.
    let mut body = String::new();
    let mut inside = false;
    for line in trimmed.lines() {
        let line = line.trim();
        if line.starts_with("-----BEGIN") {
            if !line.contains("CERTIFICATE") || line.contains("REQUEST") {
                return None;
            }
            inside = true;
            continue;
        }
        if line.starts_with("-----END") {
            break;
        }
        if inside {
            body.push_str(line);
        }
    }
    if body.is_empty() {
        return None;
    }
    unbase64(&body)
}

/// Read whatever the operator pasted or loaded, for showing on screen.
///
/// `Err` carries the sentence to display rather than a typed error: every failure
/// here is "that is not the file you think it is", and the screen's job is to say
/// so before the run rather than to branch on why.
///
/// Lives here rather than in the wizard so it is covered by tests — `src/ui/` and
/// `src/app.rs` are outside the coverage gate, and `AGENTS.md` §4 makes that a
/// contract: logic that cannot be tested is in the wrong place.
pub fn preview(text: &str) -> std::result::Result<Summary, String> {
    if text.trim().is_empty() {
        return Err("nothing loaded yet".into());
    }
    let der = der_from_pem(text).ok_or_else(|| {
        "this is not a PEM or DER certificate — check it is what the CA returned and not the \
         request that was sent to them"
            .to_owned()
    })?;
    summarise(&der, "certificate.preview").map_err(|e| e.detail())
}

/// Parse a certificate and describe it.
pub fn summarise(der: &[u8], operation: &'static str) -> Result<Summary> {
    let certificate = parse(der, operation)?;
    let tbs = &certificate.tbs_certificate;

    Ok(Summary {
        subject: tbs.subject.to_string(),
        issuer: tbs.issuer.to_string(),
        // Hex, the form every CA portal and CRL shows it in.
        serial: hex::encode_upper(tbs.serial_number.as_bytes()),
        not_before: tbs.validity.not_before.to_string(),
        not_after: tbs.validity.not_after.to_string(),
        email_sans: email_sans(&certificate),
        key_usages: key_usages(&certificate),
        extended_key_usages: extended_key_usages(&certificate),
    })
}

/// The certificate's `SubjectPublicKeyInfo`, DER-encoded.
pub fn public_key_der(der: &[u8], operation: &'static str) -> Result<Vec<u8>> {
    let certificate = parse(der, operation)?;
    certificate
        .tbs_certificate
        .subject_public_key_info
        .to_der()
        .map_err(|e| WriteError::Failed {
            operation,
            reason: format!("the certificate's public key could not be read: {e}"),
        })
}

/// Refuse a certificate whose public key is not the one in the slot.
///
/// `slot_key_der` is the slot's `SubjectPublicKeyInfo`, as the card reports it.
/// The comparison is of the encoded structures, which is the strict reading: same
/// algorithm, same parameters, same key.
pub fn must_match_public_key(
    certificate_der: &[u8],
    slot_key_der: &[u8],
    operation: &'static str,
) -> Result<()> {
    let certificate_key = public_key_der(certificate_der, operation)?;
    if certificate_key == slot_key_der {
        return Ok(());
    }
    Err(WriteError::Failed {
        operation,
        reason: "this certificate was issued for a different key than the one in the slot — \
                 nothing was written. A certificate imported over the wrong key installs \
                 cleanly and then fails every signature the holder makes"
            .into(),
    })
}

fn parse(der: &[u8], operation: &'static str) -> Result<Certificate> {
    if der.is_empty() {
        return Err(WriteError::Failed {
            operation,
            reason: "no certificate was supplied".into(),
        });
    }
    if der.len() > MAX_CERTIFICATE_BYTES {
        return Err(WriteError::Failed {
            operation,
            reason: format!(
                "the certificate is {} bytes, past the {MAX_CERTIFICATE_BYTES}-byte limit — that \
                 is a chain or the wrong file rather than one certificate",
                der.len()
            ),
        });
    }
    Certificate::from_der(der).map_err(|e| WriteError::Failed {
        operation,
        reason: format!("this is not an X.509 certificate: {e}"),
    })
}

fn email_sans(certificate: &Certificate) -> Vec<String> {
    use x509_cert::der::oid::AssociatedOid;
    use x509_cert::ext::pkix::SubjectAltName;
    use x509_cert::ext::pkix::name::GeneralName;

    let Some(extensions) = certificate.tbs_certificate.extensions.as_ref() else {
        return Vec::new();
    };

    extensions
        .iter()
        .filter(|extension| extension.extn_id == SubjectAltName::OID)
        .filter_map(|extension| SubjectAltName::from_der(extension.extn_value.as_bytes()).ok())
        .flat_map(|san| {
            san.0
                .into_iter()
                .filter_map(|name| match name {
                    GeneralName::Rfc822Name(email) => Some(email.as_str().to_owned()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The `keyUsage` bits, in the names RFC 5280 gives them.
///
/// Named rather than numbered because the whole reason to read this is to show an
/// operator what a certificate is for, and `bit 0` is not that.
fn key_usages(certificate: &Certificate) -> Vec<String> {
    use x509_cert::der::oid::AssociatedOid;
    use x509_cert::ext::pkix::KeyUsage;

    use x509_cert::ext::pkix::KeyUsages;

    // The flags in RFC 5280's own order, so two certificates with the same usages
    // always produce the same list — a step detail that reordered between runs
    // would look like a change.
    const NAMES: [(KeyUsages, &str); 9] = [
        (KeyUsages::DigitalSignature, "digitalSignature"),
        (KeyUsages::NonRepudiation, "nonRepudiation"),
        (KeyUsages::KeyEncipherment, "keyEncipherment"),
        (KeyUsages::DataEncipherment, "dataEncipherment"),
        (KeyUsages::KeyAgreement, "keyAgreement"),
        (KeyUsages::KeyCertSign, "keyCertSign"),
        (KeyUsages::CRLSign, "cRLSign"),
        (KeyUsages::EncipherOnly, "encipherOnly"),
        (KeyUsages::DecipherOnly, "decipherOnly"),
    ];

    let Some(extensions) = certificate.tbs_certificate.extensions.as_ref() else {
        return Vec::new();
    };
    extensions
        .iter()
        .filter(|extension| extension.extn_id == KeyUsage::OID)
        .filter_map(|extension| KeyUsage::from_der(extension.extn_value.as_bytes()).ok())
        .flat_map(|usage| {
            NAMES
                .iter()
                .filter(|(flag, _)| usage.0.contains(*flag))
                .map(|(_, name)| (*name).to_owned())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The `extKeyUsage` OIDs, with the ones this tool has an opinion about named and
/// the rest left as dotted numbers.
///
/// Unknown OIDs are kept rather than dropped: an EKU nobody here recognises is
/// still a statement the CA made, and hiding it would make a constrained
/// certificate look unconstrained.
fn extended_key_usages(certificate: &Certificate) -> Vec<String> {
    use x509_cert::der::oid::AssociatedOid;
    use x509_cert::ext::pkix::ExtendedKeyUsage;

    const NAMES: [(&str, &str); 7] = [
        ("2.5.29.37.0", "anyExtendedKeyUsage"),
        ("1.3.6.1.5.5.7.3.1", "serverAuth"),
        ("1.3.6.1.5.5.7.3.2", "clientAuth"),
        ("1.3.6.1.5.5.7.3.3", "codeSigning"),
        ("1.3.6.1.5.5.7.3.4", "emailProtection"),
        ("1.3.6.1.5.5.7.3.8", "timeStamping"),
        ("1.3.6.1.5.5.7.3.9", "OCSPSigning"),
    ];

    let Some(extensions) = certificate.tbs_certificate.extensions.as_ref() else {
        return Vec::new();
    };
    extensions
        .iter()
        .filter(|extension| extension.extn_id == ExtendedKeyUsage::OID)
        .filter_map(|extension| ExtendedKeyUsage::from_der(extension.extn_value.as_bytes()).ok())
        .flat_map(|eku| {
            eku.0
                .into_iter()
                .map(|oid| {
                    let dotted = oid.to_string();
                    NAMES
                        .iter()
                        .find(|(known, _)| *known == dotted)
                        .map(|(_, name)| (*name).to_owned())
                        .unwrap_or(dotted)
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Base64 decode, matching the encoder in [`super::csr`]: same reason, which is
/// that a dependency for thirty lines is one to keep updated for ever.
fn unbase64(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let cleaned: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if cleaned.is_empty() || !cleaned.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(cleaned.len() / 4 * 3);
    for chunk in cleaned.chunks(4) {
        let pad = chunk.iter().filter(|b| **b == b'=').count();
        let mut n = 0u32;
        for (i, b) in chunk.iter().enumerate() {
            n |= if *b == b'=' { 0 } else { value(*b)? } << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A self-signed P-256 certificate for `CN=Ana Silva` with
    /// `rfc822Name=ana.silva@example.org`, generated once with `openssl` and
    /// pasted here so the parser is tested against a real document rather than
    /// against something this crate produced.
    ///
    /// ```text
    /// openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
    ///   -keyout /dev/null -nodes -days 3650 -subj '/CN=Ana Silva' \
    ///   -addext 'subjectAltName=email:ana.silva@example.org'
    /// ```
    const SAMPLE: &str = include_str!("../../tests/fixtures/certificate_with_email_san.pem");

    /// The same subject and address, plus the extensions a signing certificate
    /// carries — `keyUsage = digitalSignature, nonRepudiation` and
    /// `extendedKeyUsage = clientAuth, emailProtection`.
    ///
    /// ```text
    /// openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
    ///   -keyout /dev/null -nodes -days 3650 \
    ///   -subj '/CN=Ana Silva/OU=ESI/O=Example Organisation' \
    ///   -addext 'subjectAltName=email:ana.silva@example.org' \
    ///   -addext 'keyUsage=critical,digitalSignature,nonRepudiation' \
    ///   -addext 'extendedKeyUsage=clientAuth,emailProtection'
    /// ```
    const SIGNING: &str = include_str!("../../tests/fixtures/certificate_signing_usage.pem");

    /// An **encryption** certificate for the same person: the realistic CA mix-up,
    /// where a signing request comes back issued under the wrong profile. It
    /// imports cleanly and then fails every signature the holder makes.
    const ENCRYPTION: &str = include_str!("../../tests/fixtures/certificate_encryption_usage.pem");

    fn summary_of(pem: &str) -> Summary {
        summarise(&der_from_pem(pem).expect("the fixture is PEM"), "test").unwrap()
    }

    #[test]
    fn a_pem_certificate_is_read_and_described() {
        let der = der_from_pem(SAMPLE).expect("the fixture is PEM");
        let summary = summarise(&der, "test").unwrap();

        assert!(
            summary.subject.contains("Ana Silva"),
            "subject was {}",
            summary.subject
        );
        assert_eq!(summary.email_sans, vec!["ana.silva@example.org".to_owned()]);
        assert!(!summary.serial.is_empty());
        assert!(!summary.not_after.is_empty());
    }

    #[test]
    fn the_email_check_is_case_insensitive_and_refuses_an_absent_address() {
        let der = der_from_pem(SAMPLE).unwrap();
        let summary = summarise(&der, "test").unwrap();

        assert!(summary.covers_email("ANA.SILVA@EXAMPLE.ORG"));
        assert!(summary.covers_email("  ana.silva@example.org "));
        assert!(!summary.covers_email("someone.else@example.org"));
        assert!(
            !summary.covers_email(""),
            "an empty address must not match a certificate that has SANs"
        );
    }

    #[test]
    fn raw_der_is_accepted_as_well_as_pem() {
        let der = der_from_pem(SAMPLE).unwrap();
        let text = String::from_utf8_lossy(&der).to_string();
        let again = der_from_pem(&text).expect("DER starts with 0x30 and is taken as is");
        assert_eq!(again.first(), Some(&0x30));
    }

    #[test]
    fn a_certificate_request_is_not_mistaken_for_a_certificate() {
        // The realistic paste error: the operator copies back the CSR the tool
        // just gave them instead of what the CA returned.
        let csr = "-----BEGIN CERTIFICATE REQUEST-----\nMIIB\n-----END CERTIFICATE REQUEST-----";
        assert_eq!(der_from_pem(csr), None);
    }

    #[test]
    fn text_that_is_not_a_certificate_is_refused_rather_than_parsed() {
        assert_eq!(der_from_pem(""), None);
        assert_eq!(der_from_pem("   \n  "), None);
        assert_eq!(der_from_pem("just a note from the CA team"), None);
        assert_eq!(
            der_from_pem("-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----"),
            None,
            "an empty body is not a certificate"
        );
    }

    #[test]
    fn a_document_that_parses_as_pem_but_not_as_x509_is_an_error_with_a_reason() {
        let err = summarise(&[0x30, 0x03, 0x02, 0x01, 0x01], "test").unwrap_err();
        assert!(
            err.detail().contains("not an X.509 certificate"),
            "{}",
            err.detail()
        );
        assert!(summarise(&[], "test").is_err());
    }

    #[test]
    fn a_file_far_too_large_is_refused_by_size() {
        let mut oversized = vec![0x30, 0x82, 0xFF, 0xFF];
        oversized.extend(std::iter::repeat_n(0u8, MAX_CERTIFICATE_BYTES));
        let err = summarise(&oversized, "test").unwrap_err();
        assert!(err.detail().contains("limit"), "{}", err.detail());
    }

    #[test]
    fn a_certificate_matches_its_own_public_key_and_nothing_else() {
        let der = der_from_pem(SAMPLE).unwrap();
        let own = public_key_der(&der, "test").unwrap();

        assert!(must_match_public_key(&der, &own, "test").is_ok());

        // One byte of the key changed is a different key.
        let mut other = own.clone();
        let last = other.len() - 1;
        other[last] ^= 0xFF;
        let err = must_match_public_key(&der, &other, "test").unwrap_err();
        assert!(
            err.detail().contains("different key"),
            "the refusal has to say why: {}",
            err.detail()
        );
        assert!(
            err.detail().contains("nothing was written"),
            "and that the key was left alone: {}",
            err.detail()
        );
    }

    #[test]
    fn the_one_line_summary_carries_what_the_operator_checks() {
        let der = der_from_pem(SAMPLE).unwrap();
        let line = summarise(&der, "test").unwrap().one_line();
        assert!(line.contains("Ana Silva"));
        assert!(line.contains("ana.silva@example.org"));
        assert!(line.contains("valid="));
    }

    #[test]
    fn the_preview_describes_a_good_certificate_and_explains_a_bad_one() {
        let summary = preview(SAMPLE).expect("the fixture previews");
        assert!(summary.subject.contains("Ana Silva"));

        assert_eq!(preview("   ").unwrap_err(), "nothing loaded yet");

        // The realistic mistake, and the message has to name it.
        let message =
            preview("-----BEGIN CERTIFICATE REQUEST-----\nMIIB\n-----END CERTIFICATE REQUEST-----")
                .unwrap_err();
        assert!(
            message.contains("request"),
            "the operator needs to be told which document they pasted: {message}"
        );
    }

    #[test]
    fn a_signing_certificates_usages_are_read_by_name() {
        let summary = summary_of(SIGNING);
        assert_eq!(
            summary.key_usages,
            vec!["digitalSignature", "nonRepudiation"],
            "in RFC 5280's order, so two reads of one certificate never differ"
        );
        assert_eq!(
            summary.extended_key_usages,
            vec!["clientAuth", "emailProtection"]
        );
        assert_eq!(summary.signing_verdict(), Fitness::Fit);
    }

    #[test]
    fn an_encryption_certificate_is_refused_for_a_signing_slot() {
        // The realistic CA mix-up: a signing request comes back issued under the
        // encryption profile. It imports cleanly and then fails every signature the
        // holder makes, which is why the usage is checked rather than assumed.
        let summary = summary_of(ENCRYPTION);
        assert_eq!(summary.signing_verdict(), Fitness::Unfit);
        assert!(summary.key_usages.iter().any(|u| u == "keyEncipherment"));

        let checks = verify_read_back(&summary, "CN=Ana Silva,OU=ESI,O=Example Organisation", "");
        let usage = checks
            .iter()
            .find(|check| check.name == "key usage")
            .expect("the usage is one of the checks");
        assert_eq!(usage.passed, Some(false));
        assert!(usage.detail.contains("not issued for signing"), "{usage:?}");
    }

    #[test]
    fn a_certificate_that_states_no_usage_is_unchecked_and_not_a_failure() {
        // A minimal internal CA may issue with neither extension. That constrains
        // nothing, so refusing it would refuse a perfectly usable certificate — and
        // reporting it as a pass would claim the CA said something it did not.
        let summary = summary_of(SAMPLE);
        assert!(summary.key_usages.is_empty());
        assert_eq!(summary.signing_verdict(), Fitness::NotStated);

        let usage = verify_read_back(&summary, "", "")
            .into_iter()
            .find(|check| check.name == "key usage")
            .unwrap();
        assert_eq!(usage.passed, None);
        assert_eq!(usage.to_field(), "key_usage=unchecked");
    }

    #[test]
    fn a_read_back_checks_subject_address_and_usage_and_says_the_chain_was_not_checked() {
        // `features/step-piv-signing-certificate.md` phase 7. The chain is the one
        // check this build cannot make, and it is reported as unchecked rather than
        // omitted — a verification that quietly left it out would read as a full one.
        let summary = summary_of(SIGNING);
        let checks = verify_read_back(
            &summary,
            "CN=Ana Silva,OU=ESI,O=Example Organisation",
            "ana.silva@example.org",
        );

        let names: Vec<&str> = checks.iter().map(|check| check.name).collect();
        assert_eq!(names, vec!["subject", "rfc822Name", "key usage", "chain"]);
        assert_eq!(
            checks
                .iter()
                .filter(|check| check.passed == Some(false))
                .count(),
            0,
            "{checks:?}"
        );

        let chain = checks.last().unwrap();
        assert_eq!(chain.passed, None);
        assert!(chain.detail.contains("trust store"), "{chain:?}");
        assert_eq!(chain.to_field(), "chain=unchecked");
    }

    #[test]
    fn a_certificate_for_another_holder_fails_the_read_back() {
        let summary = summary_of(SIGNING);
        let checks = verify_read_back(
            &summary,
            "CN=Bruno Costa,OU=ESI,O=Example Organisation",
            "bruno.costa@example.org",
        );
        let failed: Vec<&str> = checks
            .iter()
            .filter(|check| check.passed == Some(false))
            .map(|check| check.name)
            .collect();
        assert_eq!(failed, vec!["subject", "rfc822Name"], "{checks:?}");
        assert!(
            checks[1].detail.contains("will not validate"),
            "the message has to say what goes wrong for the holder: {:?}",
            checks[1]
        );
    }

    #[test]
    fn a_subject_a_ca_renormalised_is_still_the_same_subject() {
        // A CA is entitled to reorder and re-space a distinguished name. A check
        // that failed on that would fail on every real certificate, and a check that
        // always fails gets switched off.
        assert!(same_subject(
            "CN=Ana Silva, OU=ESI, O=Example Organisation",
            "O=Example Organisation,CN=Ana Silva,OU=ESI"
        ));
        assert!(same_subject("CN=Ana Silva", "cn=ana silva"));
        // But a different name is a different subject, which is the whole point.
        assert!(!same_subject(
            "CN=Ana Silva,OU=ESI",
            "CN=Bruno Costa,OU=ESI"
        ));
        assert!(!same_subject("CN=Ana Silva", "CN=Ana Silva,OU=ESI"));
    }

    #[test]
    fn a_run_with_nothing_recorded_to_compare_against_reports_unchecked_not_ok() {
        let summary = summary_of(SIGNING);
        let checks = verify_read_back(&summary, "", "");
        assert_eq!(checks[0].passed, None, "{:?}", checks[0]);
        assert_eq!(checks[1].passed, None, "{:?}", checks[1]);
        assert!(
            checks.iter().all(|check| check.passed != Some(false)),
            "nothing to compare against is not a failure: {checks:?}"
        );
    }

    #[test]
    fn every_fitness_verdict_reads_as_its_own_words() {
        let labels: Vec<&str> = [Fitness::Fit, Fitness::Unfit, Fitness::NotStated]
            .iter()
            .map(|f| f.label())
            .collect();
        assert_eq!(
            labels.len(),
            labels
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
    }

    #[test]
    fn base64_with_a_broken_length_is_refused() {
        // A truncated paste, which is what a copy out of a terminal produces.
        assert_eq!(unbase64("MIIB0"), None);
        assert_eq!(unbase64("MIIB!!!!"), None);
        assert_eq!(unbase64(""), None);
    }
}
