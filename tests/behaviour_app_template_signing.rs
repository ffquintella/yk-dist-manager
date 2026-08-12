//! Behaviour test for the application's half of template files and signatures
//! (`features/bootstrap-templates.md` phases 4 and 5): a procedure arrives as a
//! file, the operator sees what it would do before storing it, the import is
//! audited, and a deployment that requires signatures refuses to run an unsigned
//! procedure.
//!
//! **One test in this file, deliberately** — it drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared by
//! every test in a binary.

use yk_dist_manager::YkDistApp;
use yk_dist_manager::template::signing::{ALGORITHM, canonical_bytes};
use yk_dist_manager::template::{BootstrapTemplate, TemplateFile, TemplateSignature, Trust};

/// Sign a procedure with a key this test holds. The private half exists for the
/// length of this call, protects nothing, and never goes near the application —
/// which is the arrangement the feature is built on.
fn signed(template: &BootstrapTemplate, key_id: &str, seed: [u8; 32]) -> BootstrapTemplate {
    let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
    let signature = ed25519_dalek::Signer::sign(&signing, &canonical_bytes(template));
    let mut out = template.clone();
    out.signature = Some(TemplateSignature {
        key_id: key_id.into(),
        algorithm: ALGORITHM.into(),
        signature: hex::encode(signature.to_bytes()),
    });
    out
}

#[test]
fn scenario_a_signed_procedure_is_imported_from_a_file_and_gates_a_run() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
    }

    // Given an open register, and a signed procedure sitting in a file — the way a
    // procedure actually arrives: by mail, from another unit or from whoever owns
    // the organisation's standard
    let mut app = YkDistApp::new(Some(home.path().join("keys.sqlite3")));
    assert!(app.store.is_some(), "{:?}", app.db_form.error);

    let mut procedure = BootstrapTemplate::builtin()
        .into_iter()
        .find(|t| t.id == "fido-only")
        .expect("a built-in to start from")
        .duplicated_as("unit-standard", "Unit standard");
    procedure.description = "The procedure this unit was given".into();
    let procedure = signed(&procedure, "esi-templates-2026", [11u8; 32]);

    let path = home.path().join("unit-standard-v1.json");
    std::fs::write(
        &path,
        TemplateFile::of(&procedure, chrono::Utc::now()).to_json(),
    )
    .unwrap();

    // And a deployment that has not been told whose key to trust yet
    assert!(app.settings.template_keys.is_empty());

    // When the operator reads the file
    app.template_editor.file_path = path.display().to_string();
    app.read_template_file(&path);

    // Then nothing is stored — reading is a preview — and the preview says the
    // signature is by a key this deployment does not have, which is *not* the same
    // as saying it is signed
    let pending = app
        .template_editor
        .pending_import
        .as_ref()
        .expect("the file was read");
    assert_eq!(
        pending.trust,
        Trust::UnknownKey {
            key_id: "esi-templates-2026".into()
        }
    );
    assert!(!pending.trust.is_verified());
    assert!(
        pending.against.is_none(),
        "this register has no template of that id yet"
    );
    assert!(
        !app.template_catalogue
            .iter()
            .any(|s| s.template.id == "unit-standard"),
        "reading a file must not store anything"
    );

    // When the operator adds the key that signed it, from Settings
    app.key_form.id = "esi-templates-2026".into();
    app.key_form.public_key = hex::encode(
        ed25519_dalek::SigningKey::from_bytes(&[11u8; 32])
            .verifying_key()
            .to_bytes(),
    );
    app.key_form.comment = "the organisation's template key".into();
    app.add_template_key();
    assert!(app.key_form.error.is_none(), "{:?}", app.key_form.error);
    assert_eq!(app.settings.template_keys.len(), 1);

    // And reads the file again, then stores it
    app.read_template_file(&path);
    assert!(
        app.template_editor
            .pending_import
            .as_ref()
            .unwrap()
            .trust
            .is_verified(),
        "with the key trusted, the same file now verifies"
    );
    app.apply_template_import();

    // Then it is on record, verified, under a version this register assigned…
    let stored = app
        .template_catalogue
        .iter()
        .find(|s| s.template.id == "unit-standard")
        .expect("the import stored it")
        .template
        .clone();
    assert_eq!(stored.version, "1");
    assert!(app.template_trust(&stored).is_verified());
    assert!(app.template_editor.pending_import.is_none());

    // …and the import is in the audit trail, naming where it came from and that
    // the signature verified — neither of which is recoverable from the body
    let entry = app
        .audit_view
        .iter()
        .find(|e| e.event == "template.imported")
        .expect("an import is a state change");
    assert_eq!(entry.target, "template:unit-standard");
    assert!(
        entry.details.contains("signature=verified"),
        "{}",
        entry.details
    );
    assert!(
        entry.details.contains("key=esi-templates-2026"),
        "{}",
        entry.details
    );
    assert!(
        entry.details.contains(&path.display().to_string()),
        "{}",
        entry.details
    );
    assert!(
        !entry.details.contains("{{"),
        "an audit entry must not quote the procedure itself: {}",
        entry.details
    );

    // When the deployment starts requiring signatures
    app.settings.templates_must_be_signed = true;

    // Then the signed procedure may still be run…
    assert!(app.template_run_permission(&stored).is_ok());

    // …and the unsigned built-ins may not, with a refusal that says why rather
    // than a button that does nothing
    let builtin = app
        .templates
        .iter()
        .find(|t| t.signature.is_none())
        .cloned()
        .expect("the shipped procedures are unsigned");
    let refusal = app
        .template_run_permission(&builtin)
        .expect_err("an unsigned procedure must be refused when signatures are required");
    assert!(refusal.contains("unsigned"), "{refusal}");
    assert!(refusal.contains(&builtin.id), "{refusal}");
    assert!(!app.pilot_mode());

    // And with the requirement off — pilot mode — the same procedure is allowed,
    // which is the state a fresh install is in and the reason it is visible on
    // screen rather than silent
    app.settings.templates_must_be_signed = false;
    assert!(app.pilot_mode());
    match app.template_run_permission(&builtin) {
        Ok(trust) => assert_eq!(trust, Trust::Unsigned),
        Err(e) => panic!("pilot mode must allow an unsigned procedure: {e}"),
    }

    // And a key that cannot verify anything is refused as it is typed, rather than
    // becoming a trust store that reports every template as altered
    app.key_form.id = "typo".into();
    app.key_form.public_key = "not-a-key".into();
    app.add_template_key();
    assert!(app.key_form.error.is_some());
    assert_eq!(app.settings.template_keys.len(), 1, "nothing was added");
}
