//! Does the documented **out-of-band signing** procedure actually work?
//!
//! `features/bootstrap-templates.md` phase 5 puts the private key outside this
//! application on purpose, which makes the signing step somebody else's command
//! line — and a documented command line nobody has run is a guess. This test runs
//! it: `openssl` generates a key, signs the canonical bytes the application
//! exports, and the signature is fed back through [`verify`].
//!
//! **Ignored by default**, like the hardware tests, because it shells out to
//! `openssl` and a build machine need not have one:
//!
//! ```bash
//! cargo test --test interop_template_signing -- --ignored --nocapture
//! ```
//!
//! If this fails, the runbook in `docs/operations.md` is wrong and somebody in a
//! unit is about to find out the hard way.

use std::path::Path;
use std::process::Command;

use yk_dist_manager::template::signing::{ALGORITHM, canonical_bytes, verify};
use yk_dist_manager::template::{BootstrapTemplate, TemplateKey, TemplateSignature, Trust};

/// Run a command, returning its stdout and failing the test with its stderr.
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

/// The last 32 bytes of an Ed25519 `SubjectPublicKeyInfo` are the raw key.
///
/// The DER is 44 bytes: a 12-byte header naming the algorithm, then the key. Taking
/// the tail is what the runbook's `tail -c 32` does, and this asserts the shape
/// rather than trusting it.
fn raw_public_key(der: &[u8]) -> Vec<u8> {
    assert_eq!(der.len(), 44, "an Ed25519 SPKI is 44 DER bytes");
    der[12..].to_vec()
}

#[test]
#[ignore = "shells out to openssl; run explicitly"]
fn the_documented_openssl_procedure_produces_a_signature_this_build_accepts() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("template-key.pem");
    let canonical = dir.path().join("org-standard-v2.canonical");
    let signature = dir.path().join("org-standard-v2.sig");

    // Given the canonical bytes of a procedure, as `Export` writes them beside the
    // JSON
    let template = BootstrapTemplate::builtin()
        .into_iter()
        .next()
        .expect("a built-in procedure");
    std::fs::write(&canonical, canonical_bytes(&template)).unwrap();

    // And a signing key held by whoever approves procedures — here, this test
    run(
        "openssl",
        &["genpkey", "-algorithm", "ed25519", "-out", path(&key)],
    );

    // When they sign those bytes, exactly as docs/operations.md says
    run(
        "openssl",
        &[
            "pkeyutl",
            "-sign",
            "-inkey",
            path(&key),
            "-rawin",
            "-in",
            path(&canonical),
            "-out",
            path(&signature),
        ],
    );
    let signature_hex = hex::encode(std::fs::read(&signature).unwrap());

    // And the public half is published as hex
    let der = run(
        "openssl",
        &["pkey", "-in", path(&key), "-pubout", "-outform", "DER"],
    );
    let public_key_hex = hex::encode(raw_public_key(&der));
    assert_eq!(public_key_hex.len(), 64, "the runbook says 64 characters");
    assert_eq!(signature_hex.len(), 128, "and 128 for the signature");

    // Then this build accepts it: the two halves of the loop agree, and the
    // application never saw the private key
    let mut signed = template.clone();
    signed.signature = Some(TemplateSignature {
        key_id: "esi-templates-2026".into(),
        algorithm: ALGORITHM.into(),
        signature: signature_hex.clone(),
    });
    let keys = vec![TemplateKey {
        id: "esi-templates-2026".into(),
        public_key: public_key_hex.clone(),
        comment: "openssl, in this test".into(),
    }];

    assert_eq!(
        verify(&signed, &keys),
        Trust::Signed {
            key_id: "esi-templates-2026".into()
        },
        "the documented openssl procedure must produce a signature this build accepts"
    );

    // And it still accepts it after the export/import round trip, which renumbers
    // the version — the reason the version is not part of the canonical bytes
    let renumbered = signed.as_version("7");
    assert!(verify(&renumbered, &keys).is_verified());

    // And one changed parameter breaks it, so the loop is a control and not a
    // ceremony
    let mut tampered = signed.clone();
    tampered
        .steps
        .iter_mut()
        .find(|s| s.id == "piv-csr")
        .expect("the standard procedure requests a certificate")
        .params
        .insert("san_email".into(), "attacker@example.org".into());
    assert!(!verify(&tampered, &keys).is_verified());

    println!("openssl interop verified");
    println!("  public key : {public_key_hex}");
    println!("  signature  : {signature_hex}");
}

fn path(p: &Path) -> &str {
    p.to_str().expect("a temporary path is UTF-8")
}
