//! Assemble a PKCS#10 certificate request whose SAN carries the holder's e-mail
//! (`features/step-piv-signing-certificate.md`, `features/bootstrap-engine.md`
//! phase 6).
//!
//! ## Why this file exists at all
//!
//! The `rfc822Name` SAN *is* the reason the PIV step is native. `ykman` can generate a
//! key and produce a request, but not put an e-mail SAN in it, and a signing
//! certificate whose subject alternative name does not carry the holder's address is a
//! certificate that will not validate the signatures it was issued for — silently, at
//! the point somebody relies on it. The `yubikey` crate offers self-signed
//! certificates and raw signing, not PKCS#10, so the request has to be assembled here
//! and signed *through the card*.
//!
//! ## The shape: pure assembly, injected signature
//!
//! [`build`] takes a closure that signs a digest. Everything else — parsing the
//! subject, encoding the SAN as an extension request, DER-encoding the
//! `CertificationRequestInfo`, wrapping the result — is pure, deterministic and
//! testable with no hardware and no PIN. The card is one closure at one point.
//!
//! That split is not cosmetic. Signing needs a key, the key needs a PIN, and the PIN
//! needs a run in progress: a design where the whole request could only be exercised
//! with a YubiKey in a port is a design whose ASN.1 nobody checks. Here the bytes are
//! checked by unit tests, and by an `openssl`-backed interop test that parses the
//! finished request back and asserts the SAN survived.
//!
//! ## Deliberately ECDSA only
//!
//! RSA requires the caller to apply PKCS#1 v1.5 padding before handing bytes to the
//! card, and a mistake there produces a signature that verifies nowhere for reasons
//! nobody can see from the output. The standard procedure specifies `ECCP256`, so RSA
//! is refused with a message that says so rather than approximated.

use x509_cert::der::asn1::{BitString, Ia5String, OctetString, SetOfVec};
use x509_cert::der::{Decode, Encode};
use x509_cert::ext::Extension;
use x509_cert::ext::pkix::SubjectAltName;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::name::Name;
use x509_cert::request::{CertReq, CertReqInfo, ExtensionReq, Version};
use x509_cert::spki::{AlgorithmIdentifierOwned, ObjectIdentifier, SubjectPublicKeyInfoOwned};

use super::write::WriteError;

const OP: &str = "piv.create_csr";

/// `ecdsa-with-SHA256`, RFC 5758. Parameters absent, as that RFC requires.
const ECDSA_WITH_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");
/// `ecdsa-with-SHA384`.
const ECDSA_WITH_SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.3");

/// Which curve the slot holds, and therefore which digest to sign.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Curve {
    P256,
    P384,
}

impl Curve {
    /// The algorithm name the template uses (`ECCP256`).
    pub fn from_algorithm(algorithm: &str) -> Result<Self, WriteError> {
        match algorithm.trim().to_ascii_uppercase().as_str() {
            "ECCP256" => Ok(Curve::P256),
            "ECCP384" => Ok(Curve::P384),
            other => Err(WriteError::Unsupported {
                operation: OP,
                reason: format!(
                    "a certificate request can only be built for an ECDSA key here, and this slot \
                     holds {other}. RSA needs PKCS#1 v1.5 padding applied before the card signs, \
                     and a mistake there produces a signature that verifies nowhere for reasons \
                     the output does not show — so it is refused rather than guessed. The standard \
                     procedure specifies ECCP256."
                ),
            }),
        }
    }

    fn signature_algorithm(&self) -> ObjectIdentifier {
        match self {
            Curve::P256 => ECDSA_WITH_SHA256,
            Curve::P384 => ECDSA_WITH_SHA384,
        }
    }

    /// The digest the card is asked to sign.
    ///
    /// For ECDSA the input is the hash itself, sized to the curve — 32 bytes for P-256
    /// with SHA-256, 48 for P-384 with SHA-384. No padding, which is exactly why RSA
    /// is out of scope above.
    fn digest(&self, message: &[u8]) -> Vec<u8> {
        use sha2::Digest;
        match self {
            Curve::P256 => sha2::Sha256::digest(message).to_vec(),
            Curve::P384 => sha2::Sha384::digest(message).to_vec(),
        }
    }
}

/// The bytes the card must sign, and the request they belong to.
///
/// Returned as a pair rather than signed in place so a caller can log the digest, or a
/// test can sign it with a software key.
#[derive(Debug)]
pub struct Pending {
    info: CertReqInfo,
    curve: Curve,
    /// The DER of `CertificationRequestInfo` — the signature covers exactly this.
    pub to_be_signed: Vec<u8>,
}

impl Pending {
    /// The digest to hand to the card.
    pub fn digest(&self) -> Vec<u8> {
        self.curve.digest(&self.to_be_signed)
    }
}

/// Assemble the request info for a generated key.
///
/// `subject` is an RFC 4514 distinguished name (`CN=Ana Silva,OU=ESI`).
/// `public_key_pem` is what `generate_key` returned — a `SubjectPublicKeyInfo`, which
/// is what a request has to carry.
/// `san_email` is the holder's address, and is **required**: an empty one is refused
/// rather than omitted, because a request without the SAN is the failure this whole
/// module exists to prevent, and it would come back from the CA looking successful.
pub fn prepare(
    subject: &str,
    public_key_pem: &str,
    san_email: &str,
    algorithm: &str,
) -> Result<Pending, WriteError> {
    let curve = Curve::from_algorithm(algorithm)?;

    let san_email = san_email.trim();
    if san_email.is_empty() {
        return Err(WriteError::Failed {
            operation: OP,
            reason: "the request has no e-mail for the rfc822Name SAN. The SAN is the reason this \
                     step is native: a signing certificate without it does not validate the \
                     signatures it was issued for, and the CA would return it looking correct."
                .into(),
        });
    }

    let subject: Name = subject.parse().map_err(|e| WriteError::Failed {
        operation: OP,
        reason: format!(
            "the certificate subject {subject:?} is not a valid distinguished name: {e}"
        ),
    })?;

    let der = pem_body(public_key_pem).ok_or_else(|| WriteError::Failed {
        operation: OP,
        reason: "the generated public key is not PEM".into(),
    })?;
    let public_key = SubjectPublicKeyInfoOwned::from_der(&der).map_err(|e| WriteError::Failed {
        operation: OP,
        reason: format!("the generated public key is not a SubjectPublicKeyInfo: {e}"),
    })?;

    let attributes = san_attribute(san_email)?;

    let info = CertReqInfo {
        version: Version::V1,
        subject,
        public_key,
        attributes,
    };
    let to_be_signed = info.to_der().map_err(|e| WriteError::Failed {
        operation: OP,
        reason: format!("the certification request info could not be encoded: {e}"),
    })?;

    Ok(Pending {
        info,
        curve,
        to_be_signed,
    })
}

/// Wrap a signature produced over [`Pending::digest`] into a PEM request.
///
/// `signature` is the card's output: a DER `Ecdsa-Sig-Value`, which is what a PKCS#10
/// signature field carries for an ECDSA algorithm, so it goes in as-is. It is *parsed*
/// on the way in rather than trusted — a truncated APDU response would otherwise
/// produce a request that only fails at the CA.
pub fn finish(pending: Pending, signature: &[u8]) -> Result<String, WriteError> {
    // Not a structural validation of the curve points, which needs the public key and
    // buys little here — just enough to catch a response that is not a signature at
    // all, which is the realistic failure (a short read, or an error body).
    if signature.first() != Some(&0x30) || signature.len() < 8 {
        return Err(WriteError::Failed {
            operation: OP,
            reason: format!(
                "the card returned {} byte(s) that are not a DER ECDSA signature — the request was \
                 not assembled, so nothing was sent anywhere",
                signature.len()
            ),
        });
    }

    let request = CertReq {
        info: pending.info,
        algorithm: AlgorithmIdentifierOwned {
            oid: pending.curve.signature_algorithm(),
            // Absent, as RFC 5758 requires for the ecdsa-with-SHA* identifiers. A
            // NULL here is a common and quietly wrong choice.
            parameters: None,
        },
        signature: BitString::from_bytes(signature).map_err(|e| WriteError::Failed {
            operation: OP,
            reason: format!("the signature could not be encoded: {e}"),
        })?,
    };

    let der = request.to_der().map_err(|e| WriteError::Failed {
        operation: OP,
        reason: format!("the certification request could not be encoded: {e}"),
    })?;
    Ok(pem("CERTIFICATE REQUEST", &der))
}

/// Assemble, sign through `sign`, and return the PEM request.
///
/// `sign` receives the digest and returns the card's DER signature. Everything either
/// side of it is pure.
pub fn build(
    subject: &str,
    public_key_pem: &str,
    san_email: &str,
    algorithm: &str,
    sign: impl FnOnce(&[u8]) -> Result<Vec<u8>, WriteError>,
) -> Result<String, WriteError> {
    let pending = prepare(subject, public_key_pem, san_email, algorithm)?;
    let signature = sign(&pending.digest())?;
    finish(pending, &signature)
}

/// The `extensionRequest` attribute holding a `subjectAltName` with one `rfc822Name`.
///
/// PKCS#10 has no place for an extension directly: extensions travel inside an
/// attribute (RFC 5272 §3.1), and the CA copies them into the issued certificate. Two
/// layers of wrapping, and getting either wrong produces a request that parses and
/// quietly carries no SAN — which is why the interop test reads it back with `openssl`
/// rather than trusting this code.
fn san_attribute(email: &str) -> Result<x509_cert::attr::Attributes, WriteError> {
    let name = Ia5String::new(email).map_err(|e| WriteError::Failed {
        operation: OP,
        reason: format!("{email:?} is not usable as an rfc822Name: {e}"),
    })?;
    let san = SubjectAltName(vec![GeneralName::Rfc822Name(name)]);
    let value = san.to_der().map_err(|e| WriteError::Failed {
        operation: OP,
        reason: format!("the subject alternative name could not be encoded: {e}"),
    })?;

    let extension = Extension {
        extn_id: <SubjectAltName as x509_cert::der::oid::AssociatedOid>::OID,
        // Not critical: a SAN in a request is a request, and marking it critical is how
        // a CA that does not honour it rejects the whole thing instead of telling you.
        critical: false,
        extn_value: OctetString::new(value).map_err(|e| WriteError::Failed {
            operation: OP,
            reason: format!("the extension value could not be encoded: {e}"),
        })?,
    };

    let attribute: x509_cert::attr::Attribute =
        ExtensionReq(vec![extension])
            .try_into()
            .map_err(|e| WriteError::Failed {
                operation: OP,
                reason: format!("the extension request could not be encoded: {e}"),
            })?;

    let mut attributes: SetOfVec<x509_cert::attr::Attribute> = Default::default();
    attributes
        .insert(attribute)
        .map_err(|e| WriteError::Failed {
            operation: OP,
            reason: format!("the request attributes could not be assembled: {e}"),
        })?;
    Ok(attributes)
}

/// Read the DER out of a PEM document, whatever its label.
fn pem_body(text: &str) -> Option<Vec<u8>> {
    let body: String = text
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    unbase64(body.trim())
}

fn pem(label: &str, der: &[u8]) -> String {
    use std::fmt::Write as _;
    let body = base64(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for chunk in body.as_bytes().chunks(64) {
        let _ = writeln!(out, "{}", String::from_utf8_lossy(chunk));
    }
    let _ = writeln!(out, "-----END {label}-----");
    out
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        let indices = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        for (position, index) in indices.iter().enumerate() {
            if position <= chunk.len() {
                out.push(ALPHABET[*index as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut have = 0;
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    for byte in text.bytes() {
        if byte == b'=' || byte.is_ascii_whitespace() {
            continue;
        }
        let index = ALPHABET.iter().position(|c| *c == byte)? as u32;
        bits = (bits << 6) | index;
        have += 6;
        if have >= 8 {
            have -= 8;
            out.push((bits >> have) as u8);
            bits &= (1 << have) - 1;
        }
    }
    Some(out)
}

/// Decode a finished request, so a test asserts on what was produced rather than on
/// what was passed in.
#[cfg(test)]
fn decode_request(pem_text: &str) -> CertReq {
    let der = pem_body(pem_text).expect("the request is PEM");
    CertReq::from_der(&der).expect("the request is DER")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real P-256 `SubjectPublicKeyInfo`, so the assembly is exercised against
    /// bytes a card would actually return rather than a placeholder.
    const P256_SPKI: &str = "-----BEGIN PUBLIC KEY-----\n\
        MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEd0lYFqWkVdvNMlHIQIe4Nk3JLtEV\n\
        1oGT7Cm6PSGnTFvGjTG7EiUyz6+9Rr9oGH0F4KiDIfMoP3Zt4Wpn1cvJqQ==\n\
        -----END PUBLIC KEY-----\n";

    /// Signature-shaped bytes: a DER SEQUENCE of two INTEGERs, which is what an
    /// ECDSA signature is. Not a valid signature over anything — the assembly does
    /// not verify one, and a test that pretended otherwise would be asserting
    /// something this module does not do.
    fn signature_shaped() -> Vec<u8> {
        let mut der = vec![0x30, 0x44, 0x02, 0x20];
        der.extend(std::iter::repeat_n(0x11, 32));
        der.extend([0x02, 0x20]);
        der.extend(std::iter::repeat_n(0x22, 32));
        der
    }

    #[test]
    fn the_request_carries_the_email_as_an_rfc822_name() {
        // The one property this module exists for. Checked by decoding the finished
        // request rather than by inspecting what was passed in, because the SAN sits
        // two layers deep — an extension inside an extensionRequest attribute — and
        // both layers can be wrong in a way that still parses.
        let pem = build(
            "CN=Ana Silva,OU=ESI",
            P256_SPKI,
            "ana@example.org",
            "ECCP256",
            |_digest| Ok(signature_shaped()),
        )
        .expect("the request builds");

        let request = decode_request(&pem);
        let attribute = request
            .info
            .attributes
            .iter()
            .next()
            .expect("one attribute: the extension request");

        let value = attribute.values.iter().next().expect("one value");
        let extensions: Vec<Extension> =
            Vec::<Extension>::from_der(&value.to_der().unwrap()[..]).expect("extensions decode");
        let san_extension = extensions
            .iter()
            .find(|e| e.extn_id == <SubjectAltName as x509_cert::der::oid::AssociatedOid>::OID)
            .expect("a subjectAltName extension");
        let san =
            SubjectAltName::from_der(san_extension.extn_value.as_bytes()).expect("the SAN decodes");

        match &san.0[0] {
            GeneralName::Rfc822Name(email) => assert_eq!(email.as_str(), "ana@example.org"),
            other => panic!("the SAN must be an rfc822Name, not {other:?}"),
        }
        assert!(
            !san_extension.critical,
            "a critical SAN in a request makes a CA that ignores it reject the whole request \
             instead of saying so"
        );
    }

    #[test]
    fn the_subject_survives_into_the_request() {
        let pem = build(
            "CN=Ana Silva,OU=ESI,O=FGV",
            P256_SPKI,
            "ana@example.org",
            "ECCP256",
            |_| Ok(signature_shaped()),
        )
        .unwrap();
        let request = decode_request(&pem);
        let subject = request.info.subject.to_string();
        assert!(subject.contains("CN=Ana Silva"), "{subject}");
        assert!(subject.contains("OU=ESI"), "{subject}");
    }

    #[test]
    fn a_request_with_no_email_is_refused_rather_than_built_without_the_san() {
        // The failure worth being loud about. A request that silently lost its SAN
        // comes back from the CA as a certificate that looks right and does not work.
        let error = build("CN=Ana", P256_SPKI, "   ", "ECCP256", |_| {
            panic!("must not reach the card")
        })
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("rfc822Name"), "{message}");
    }

    #[test]
    fn the_card_signs_the_digest_of_exactly_the_bytes_that_are_wrapped() {
        // The signature has to cover the encoded CertificationRequestInfo — not the
        // whole request, not the info re-encoded afterwards. Asserted by capturing
        // what the closure was given and recomputing it from the finished request.
        let pending = prepare("CN=Ana", P256_SPKI, "ana@example.org", "ECCP256").unwrap();
        let expected = pending.digest();
        assert_eq!(expected.len(), 32, "SHA-256, sized to P-256");

        let mut seen = Vec::new();
        let pem = build(
            "CN=Ana",
            P256_SPKI,
            "ana@example.org",
            "ECCP256",
            |digest| {
                seen = digest.to_vec();
                Ok(signature_shaped())
            },
        )
        .unwrap();
        assert_eq!(seen, expected);

        let request = decode_request(&pem);
        let reencoded = request.info.to_der().unwrap();
        use sha2::Digest;
        assert_eq!(
            sha2::Sha256::digest(&reencoded).to_vec(),
            seen,
            "the info in the finished request must be byte-identical to what was signed"
        );
    }

    #[test]
    fn p384_uses_sha384_and_says_so_in_the_algorithm_identifier() {
        let pending = prepare("CN=Ana", P256_SPKI, "ana@example.org", "ECCP384").unwrap();
        assert_eq!(pending.digest().len(), 48);
        let pem = finish(pending, &signature_shaped()).unwrap();
        let request = decode_request(&pem);
        assert_eq!(request.algorithm.oid, ECDSA_WITH_SHA384);
        assert!(
            request.algorithm.parameters.is_none(),
            "RFC 5758 requires absent parameters for ecdsa-with-SHA*; a NULL here is quietly wrong"
        );
    }

    #[test]
    fn rsa_is_refused_with_the_reason_rather_than_padded_by_guesswork() {
        let error = Curve::from_algorithm("RSA2048").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("PKCS#1"), "{message}");
        assert!(message.contains("ECCP256"), "{message}");
    }

    #[test]
    fn a_card_response_that_is_not_a_signature_is_caught_before_anything_leaves() {
        // A short APDU read, or an error body where a signature was expected. Catching
        // it here means the operator hears about it now rather than from the CA.
        let pending = prepare("CN=Ana", P256_SPKI, "ana@example.org", "ECCP256").unwrap();
        let error = finish(pending, &[0x00, 0x01, 0x02]).unwrap_err();
        assert!(error.to_string().contains("not assembled"), "{error}");
    }

    #[test]
    fn a_subject_that_is_not_a_distinguished_name_is_refused_by_name() {
        let error = prepare("not a DN at all", P256_SPKI, "a@b.co", "ECCP256").unwrap_err();
        assert!(error.to_string().contains("distinguished name"), "{error}");
    }

    #[test]
    fn base64_round_trips_including_every_padding_length() {
        // The codec is hand-written here rather than pulled in. Three lengths, because
        // the padding branch is where a hand-written encoder goes wrong.
        for length in 0..=32 {
            let data: Vec<u8> = (0..length).map(|i| (i * 7 + 3) as u8).collect();
            let text = base64(&data);
            assert_eq!(
                unbase64(&text).as_deref(),
                Some(&data[..]),
                "length {length}"
            );
        }
    }
}
