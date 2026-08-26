//! Behaviour test for the sealed-envelope slip, saved from the show-once panel
//! (`features/secrets-custody.md` phase 5, `features/gui-bootstrap-wizard.md`
//! phase 9).
//!
//! The renderer was built first and reachable from nowhere: a key that had to be
//! posted left the operator copying a PIN off a panel by hand onto whatever paper
//! was nearest. This is the wiring, told as the story it belongs to — the run has
//! finished, the secrets are on screen, and the key is going in an envelope rather
//! than across a desk.
//!
//! Two things it pins down beyond "a file appears". The slip is only obtainable
//! **while the panel holds the secrets**, because nothing keeps a copy; and
//! producing one is **recorded before the bytes reach the disk**, because a live
//! PIN written to a file that no trail mentions is the audit finding this register
//! exists to prevent.
//!
//! **One test in this file, deliberately** — it drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared by
//! every test in a binary.

use yk_dist_manager::YkDistApp;
use yk_dist_manager::domain::{
    BootstrapRun, Holder, RunStatus, SerialSource, StepKind, StepOutcome, StepStatus, YubiKeyRecord,
};
use yk_dist_manager::secret::{Secret, SecretKind, ShowOnce};
use yk_dist_manager::store::{Store, StoreConfig};

const SERIAL: u32 = 20_423_633;

/// The run the standard procedure leaves behind: a FIDO2 PIN, the forced change,
/// and the PIV pair.
fn completed_run(holder: &Holder) -> BootstrapRun {
    let step = |id: &str, kind: StepKind| StepOutcome {
        step_id: id.to_owned(),
        kind,
        status: StepStatus::Done,
        started_at: Some(chrono::Utc::now()),
        finished_at: Some(chrono::Utc::now()),
        detail: "[native] applied".into(),
    };
    let mut run = BootstrapRun::new(
        SERIAL,
        Some(holder.id),
        "org-standard",
        "2",
        "felipe",
        vec![
            step("fido2-pin", StepKind::Fido2Pin),
            step("fido2-force-pin-change", StepKind::Fido2ForcePinChange),
            step("piv-pin-puk", StepKind::PivPinPuk),
        ],
    );
    run.settle();
    assert_eq!(run.status, RunStatus::Completed);
    run
}

/// The panel that run would have left on screen.
fn panel() -> ShowOnce {
    ShowOnce::new(vec![
        Secret::generate(SecretKind::Fido2Pin, 8).unwrap(),
        Secret::generate(SecretKind::PivPin, 8).unwrap(),
        Secret::generate(SecretKind::PivPuk, 8).unwrap(),
        // Protected onto the key under the PIN, so it never travels.
        Secret::generate(SecretKind::PivManagementKey, 0).unwrap(),
    ])
}

fn events(app: &YkDistApp, event: &str) -> Vec<String> {
    app.store
        .as_ref()
        .expect("a register is open")
        .audit_entries(200)
        .expect("the trail reads back")
        .into_iter()
        .filter(|entry| entry.event == event)
        .map(|entry| entry.details)
        .collect()
}

#[test]
fn scenario_a_posted_key_gets_a_sealed_slip_and_the_trail_says_so() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
    }
    let database = home.path().join("keys.sqlite3");

    // Given a register with a key, the person it is for, and a run that finished
    {
        let store = Store::create_new(&StoreConfig::new(&database)).unwrap();
        store
            .upsert_key(&YubiKeyRecord::from_serial(
                SERIAL,
                SerialSource::ManualEntry,
            ))
            .unwrap();
        store
            .insert_holder(&Holder::new("Ana Silva", "ana.silva@example.org", "ESI", "1").unwrap())
            .unwrap();
        let _ = store.close();
    }

    let mut app = YkDistApp::new(Some(database.clone()));
    assert!(app.store.is_some(), "{:?}", app.db_form.error);
    let holder = app.holders.first().cloned().expect("the holder is loaded");
    let run = completed_run(&holder);
    app.store.as_ref().unwrap().insert_run(&run).unwrap();

    // And the secrets that run generated, on screen and nowhere else
    app.wizard.serial = SERIAL.to_string();
    app.wizard.run = Some(run.clone());
    app.wizard.secrets = Some(panel());

    // When the operator saves the slip to a path they chose
    let slip = home.path().join("slip.pdf");
    assert!(app.write_transport_slip(&slip), "{:?}", app.wizard.error);

    // Then it is a printable document carrying every secret the holder has to be
    // given — read off the panel rather than compared to a literal, so no
    // credential enters this repository (`AGENTS.md` §4)
    let bytes = std::fs::read(&slip).expect("the slip is on disk");
    assert!(bytes.starts_with(b"%PDF-"));
    let rendered = String::from_utf8_lossy(&bytes).into_owned();
    let panel = app.wizard.secrets.as_ref().expect("still on screen");
    for secret in panel.for_the_holder() {
        assert!(
            rendered.contains(secret.expose()),
            "the {} is missing, so the holder cannot use the key",
            secret.kind().label()
        );
    }
    assert!(
        !rendered.contains(SecretKind::PivManagementKey.label()),
        "the management key is protected onto the key itself and must not travel"
    );
    assert!(
        rendered.contains("Ana Silva") && rendered.contains("org-standard"),
        "the slip says whose key it is and which procedure prepared it"
    );
    // PIV has no force-change flag at any firmware level, so a slip carrying a PIV
    // PIN says plainly that nothing but the holder will change it.
    assert!(rendered.contains("NOT force you"));

    // And the operator is told where it went and what to do with it
    let notice = app
        .wizard
        .slip_notice
        .clone()
        .expect("the panel says what happened");
    assert!(notice.contains("slip.pdf") && notice.contains("delete"));

    // And the trail carries which secrets left the tool and where they went, and
    // no value of any of them
    let saved = events(&app, "secret.slip.saved");
    assert_eq!(saved.len(), 1, "one slip, one entry: {saved:?}");
    assert!(
        saved[0].contains("carried=fido2-pin,piv-pin,piv-puk")
            && saved[0].contains("slip.pdf")
            && saved[0].contains("format=pdf"),
        "the entry names what was carried and where: {}",
        saved[0]
    );
    for secret in app
        .wizard
        .secrets
        .as_ref()
        .expect("still on screen")
        .entries()
    {
        assert!(
            !saved[0].contains(secret.expose()),
            "a trail entry must never carry a secret value: {}",
            saved[0]
        );
    }

    // When the operator dismisses the panel, as the procedure tells them to
    app.dismiss_secrets();

    // Then there is no second slip: nothing kept a copy, and the refusal says so
    // rather than writing a blank document
    let second = home.path().join("second.pdf");
    assert!(!app.write_transport_slip(&second));
    let refusal = app.wizard.error.clone().expect("the operator is told why");
    assert!(
        refusal.contains("dismissed"),
        "the refusal explains that nothing keeps a copy: {refusal}"
    );
    assert!(!second.exists(), "and nothing was written");
    assert_eq!(
        events(&app, "secret.slip.saved").len(),
        1,
        "nor recorded a slip that does not exist"
    );
    assert!(app.wizard.slip_notice.is_none(), "the notice went with it");
}
