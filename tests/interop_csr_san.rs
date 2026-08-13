//! Does the certificate request this tool builds actually carry the SAN, according to
//! something that is not this tool?
//!
//! `features/step-piv-signing-certificate.md` rests on one claim: the PKCS#10 request
//! carries the holder's e-mail as an `rfc822Name` subject alternative name. That claim
//! is worth exactly as much as the thing that checks it — and a unit test that decodes
//! the request with the same crate that encoded it checks the round trip, not the
//! standard. If `x509-cert` and this code agree on a wrong encoding, both agree.
//!
//! So this test hands the finished request to `openssl`, which is what the CA on the
//! other end will be using, and reads the SAN back out of its human-readable dump. It
//! also verifies the **signature**, using a software key in place of the card, which
//! proves the signature covers the right bytes — the mistake that produces a request
//! every CA rejects with "signature failure" and no further detail.
//!
//! **Ignored by default**, like the other interop and hardware tests, because it
//! shells out to `openssl` and a build machine need not have one:
//!
//! ```bash
//! cargo test --features native-device --test interop_csr_san -- --ignored --nocapture
//! ```
//!
//! No key is touched: the signing closure is a local `openssl` key, and the point is
//! the bytes, not the hardware.

#![cfg(feature = "native-piv")]

use std::path::Path;
use std::process::Command;

use yk_dist_manager::device::csr;

fn run(what: &str, args: &[&str]) -> Vec<u8> {
    let output = Command::new(what)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("could not run {what}: {e}"));
    assert!(
        output.status.success(),
        "{what} {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Generate a P-256 key with `openssl` and return (private key path, public key PEM).
fn software_key(dir: &Path) -> (std::path::PathBuf, String) {
    let private = dir.join("key.pem");
    run(
        "openssl",
        &[
            "ecparam",
            "-genkey",
            "-name",
            "prime256v1",
            "-noout",
            "-out",
            private.to_str().unwrap(),
        ],
    );
    let public = run(
        "openssl",
        &["ec", "-in", private.to_str().unwrap(), "-pubout"],
    );
    (private, String::from_utf8(public).unwrap())
}

#[test]
#[ignore = "shells out to openssl"]
fn openssl_reads_the_rfc822_name_back_out_of_the_request_we_built() {
    let dir = tempfile::tempdir().unwrap();
    let (private, public_pem) = software_key(dir.path());

    // Stand in for the card: sign the digest with the software key. `openssl pkeyutl
    // -sign` over a pre-computed digest is exactly what the card does — raw ECDSA over
    // 32 bytes, DER signature out — so the substitution tests the same code path the
    // card takes.
    let request = csr::build(
        "CN=Ana Silva,OU=ESI,O=FGV",
        &public_pem,
        "ana.silva@example.org",
        "ECCP256",
        |digest| {
            let digest_file = dir.path().join("digest.bin");
            std::fs::write(&digest_file, digest).unwrap();
            Ok(run(
                "openssl",
                &[
                    "pkeyutl",
                    "-sign",
                    "-inkey",
                    private.to_str().unwrap(),
                    "-in",
                    digest_file.to_str().unwrap(),
                    "-pkeyopt",
                    "digest:sha256",
                ],
            ))
        },
    )
    .expect("the request builds");

    let request_file = dir.path().join("request.pem");
    std::fs::write(&request_file, &request).unwrap();

    // The claim: openssl sees the SAN.
    let dumped = run(
        "openssl",
        &[
            "req",
            "-in",
            request_file.to_str().unwrap(),
            "-noout",
            "-text",
        ],
    );
    let text = String::from_utf8_lossy(&dumped);
    assert!(
        text.contains("ana.silva@example.org"),
        "openssl must see the e-mail in the request:\n{text}"
    );
    assert!(
        text.contains("X509v3 Subject Alternative Name"),
        "and see it as a subject alternative name rather than anywhere else:\n{text}"
    );
    assert!(
        text.contains("email:ana.silva@example.org"),
        "specifically as an rfc822Name — `email:` is how openssl renders that GeneralName \
         variant, and a DNS or URI name here would be silently useless:\n{text}"
    );
    // Spacing around `=` differs between openssl builds, so the check is on the value
    // rather than on the rendering.
    assert!(
        text.contains("Ana Silva"),
        "the subject has to survive too:\n{text}"
    );

    // The other claim: the signature covers the right bytes. `-verify` recomputes it
    // over the request's own CertificationRequestInfo, so this fails if the signature
    // was taken over the wrong span — the mistake a CA reports as "signature failure"
    // with nothing else to go on.
    let verified = Command::new("openssl")
        .args([
            "req",
            "-in",
            request_file.to_str().unwrap(),
            "-noout",
            "-verify",
        ])
        .output()
        .expect("openssl runs");
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&verified.stdout),
        String::from_utf8_lossy(&verified.stderr)
    );
    assert!(
        verified.status.success() && report.to_lowercase().contains("verify ok"),
        "the self-signature over CertificationRequestInfo must verify: {report}"
    );
}
