//! Behaviour: generating a report and exporting it
//! (`features/reports-and-export.md` phases 1, 2, 6 and 7).
//!
//! Three things this covers that no unit test can: the report is built from a
//! real register rather than a fixture, the file that lands on disk has the rows
//! the screen showed, and the export writes `export.taken` — which is the half of
//! the operation the norm cares about, since the file itself leaves the
//! application's protection the moment it is written.
//!
//! **One test per scenario, one register per test, and one home for the binary.**
//! `YkDistApp` reads `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process
//! environment is shared by every test in a binary — so the redirection happens
//! once, in [`home`], and each test opens its own database file underneath it.
//! That is safe here for a reason worth stating rather than assuming: nothing in
//! these tests writes the settings file. `AppSettings::save` is called from the
//! paint pass, to remember a window size, and no test here paints.

use yk_dist_manager::YkDistApp;
use yk_dist_manager::domain::{
    DeliveryMethod, DistributionRecord, Holder, KeyStatus, SerialSource, YubiKeyRecord,
};
use yk_dist_manager::report::{ReportKind, export::Format};
use yk_dist_manager::store::{Store, StoreConfig};

/// The one temporary home this binary uses, created on first use.
///
/// A `OnceLock` rather than a per-test directory because the redirection is
/// process-wide: two tests setting the same variables to *different* values
/// would race, and setting them to the same value from one initialiser cannot.
/// It is deliberately never dropped — the process ends and the temporary
/// directory goes with it.
fn home() -> &'static std::path::Path {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::tempdir().unwrap();
        // SAFETY: written exactly once, before any test reads them, by the thread
        // that wins `get_or_init`; every other thread blocks until it finishes.
        unsafe {
            std::env::set_var("YKDM_DATA_DIR", home.path());
            std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
        }
        home
    })
    .path()
}

fn handover(key: &YubiKeyRecord, holder: &Holder, days: i64) -> DistributionRecord {
    DistributionRecord {
        id: uuid::Uuid::new_v4(),
        key_id: key.id,
        key_serial: key.serial,
        holder_id: holder.id,
        holder_display: holder.display(),
        distributed_at: chrono::Utc::now() - chrono::Duration::days(days),
        distributed_by: "felipe".into(),
        method: DeliveryMethod::InPerson,
        receipt_ref: "processo 2026/114".into(),
        bootstrap_run_id: None,
        returned_at: None,
        returned_to: None,
        notes: String::new(),
    }
}

/// A register with one key in stock and one in Ana's hands.
fn register(path: &std::path::Path) {
    let store = Store::create_new(&StoreConfig::new(path)).unwrap();

    let mut held = YubiKeyRecord::from_serial(20_423_633, SerialSource::Device);
    held.model = "YubiKey 5 NFC".into();
    store.upsert_key(&held).unwrap();

    let mut stock = YubiKeyRecord::from_serial(31_000_001, SerialSource::Device);
    stock.model = "YubiKey 5C NFC".into();
    store.upsert_key(&stock).unwrap();

    let holder = Holder::new("Ana Silva", "ana.silva@example.org", "ESI", "1").unwrap();
    store.insert_holder(&holder).unwrap();
    store.insert_distribution(&handover(&held, &holder, 10)).unwrap();
    // The lifecycle is asked first, exactly as the Distribution screen asks it.
    store
        .set_key_status(held.serial, KeyStatus::Bootstrapped)
        .unwrap();
    store
        .set_key_status(held.serial, KeyStatus::Distributed)
        .unwrap();

    let _ = store.close();
}

fn exports(app: &YkDistApp) -> Vec<String> {
    app.store
        .as_ref()
        .expect("a register is open")
        .audit_entries(200)
        .expect("the trail reads back")
        .into_iter()
        .filter(|entry| entry.event == "export.taken")
        .map(|entry| entry.details)
        .collect()
}

#[test]
fn scenario_the_custody_report_is_exported_and_the_export_is_audited() {
    let home = home();
    let database = home.join("keys.sqlite3");

    // Given a register with one key in somebody's hands
    register(&database);
    let mut app = YkDistApp::new(Some(database.clone()));
    assert!(app.store.is_some(), "{:?}", app.db_form.error);

    // When the operator generates the custody report
    app.reports.kind = ReportKind::Custody;
    app.generate_report();

    // Then it names the one open hand-over, and says when it was made
    let report = app.reports.current.clone().expect("a report");
    assert_eq!(report.rows.len(), 1, "{:?}", report.rows);
    assert_eq!(report.rows[0][0], "20423633");
    assert_eq!(report.rows[0][1], "Ana Silva");
    assert_eq!(report.rows[0][5], "10", "days held");
    assert!(report.provenance().contains("Custody"), "{}", report.provenance());

    // When it is exported as CSV
    let target = home.join("custody.csv");
    app.reports.format = Format::Csv;
    assert!(app.write_report(&target), "{:?}", app.reports.error);

    // Then the file is on disk, its rows match what was on screen, and it says
    // what it is without anybody having to remember
    let written = std::fs::read_to_string(&target).expect("the export exists");
    let lines: Vec<&str> = written.lines().collect();
    assert!(lines[0].starts_with("# Custody"), "{}", lines[0]);
    let data_rows = written
        .lines()
        .skip_while(|line| line.starts_with('#') || line.trim().is_empty())
        .skip(1) // the header
        .filter(|line| !line.trim().is_empty())
        .count();
    assert_eq!(data_rows, report.rows.len());
    assert!(written.contains("ana.silva@example.org"), "{written}");

    // And the trail records that a list of people left the application
    let taken = exports(&app);
    assert_eq!(taken.len(), 1, "{taken:?}");
    assert!(taken[0].contains("report=custody"), "{}", taken[0]);
    assert!(taken[0].contains("format=csv"), "{}", taken[0]);
    assert!(taken[0].contains("rows=1"), "{}", taken[0]);
    assert!(taken[0].contains("custody.csv"), "{}", taken[0]);
}

#[test]
fn scenario_no_export_contains_a_secret() {
    let home = home();
    let database = home.join("secrets.sqlite3");
    register(&database);

    let mut app = YkDistApp::new(Some(database.clone()));

    // The sweep AGENTS.md §4 asks for, applied to the artefact that leaves the
    // machine. There is nothing secret to find — that is what
    // `features/secrets-custody.md` is for — and this is what says so about the
    // export rather than about the record it was built from.
    for kind in ReportKind::ALL {
        app.reports.kind = kind;
        app.generate_report();
        let report = app.reports.current.clone().expect("a report");
        for format in Format::available_for(kind) {
            let rendered = yk_dist_manager::report::export::render(&report, *format);
            // A PDF is compressed text, so this reads the bytes rather than a
            // string — which is the right check anyway: what matters is that the
            // sequence is not in the file, whatever encoded it.
            let lowered = String::from_utf8_lossy(&rendered).to_lowercase();
            for forbidden in ["pin=", "puk=", "management_key=", "access_code=", "password"] {
                assert!(
                    !lowered.contains(forbidden),
                    "{kind:?}/{format:?} export contains `{forbidden}`"
                );
            }
        }
    }
}

#[test]
fn scenario_the_audit_extract_carries_a_verification_statement() {
    let home = home();
    let database = home.join("extract.sqlite3");
    register(&database);

    let mut app = YkDistApp::new(Some(database.clone()));

    // Given the trail this session has already written
    app.reports.kind = ReportKind::AuditExtract;
    app.generate_report();

    let report = app.reports.current.clone().expect("an extract");
    assert!(!report.rows.is_empty(), "the register audits opening itself");
    let statement = report.notes.first().expect("a statement");
    assert!(statement.contains("verified at export time"), "{statement}");
    assert!(statement.contains("chain head"), "{statement}");

    // And narrowing it by actor narrows the rows without changing the statement's
    // subject: the chain it was cut from is the whole chain
    let everything = report.rows.len();
    app.reports.audit_filter.actor = "nobody-by-this-name".into();
    app.generate_report();
    let narrowed = app.reports.current.clone().expect("an extract");
    assert!(narrowed.rows.is_empty(), "{:?}", narrowed.rows);
    assert!(everything > 0);
    assert!(
        narrowed.notes[0].contains("no entries in range"),
        "{}",
        narrowed.notes[0]
    );
}

#[test]
fn scenario_the_bundle_writes_every_report_from_one_moment_and_audits_each_file() {
    let home = home();
    let database = home.join("bundle.sqlite3");
    register(&database);

    let mut app = YkDistApp::new(Some(database.clone()));
    let into = home.join("bundle-out");
    std::fs::create_dir_all(&into).unwrap();

    // When the operator asks for everything at once
    let directory = app.export_bundle(&into).expect("a folder");

    // Then every file the bundle promises is there, plus the manifest
    for (kind, format) in yk_dist_manager::report::bundle::CONTENTS {
        let expected = directory.join(format!(
            "{}-{}.{}",
            kind.slug(),
            chrono::Utc::now().format("%Y-%m-%d"),
            format.extension()
        ));
        assert!(expected.exists(), "{} is missing", expected.display());
    }
    let manifest = std::fs::read_to_string(
        directory.join(yk_dist_manager::report::bundle::MANIFEST),
    )
    .expect("a manifest");
    assert!(manifest.contains("custody-"), "{manifest}");
    assert!(manifest.contains("export.taken"), "{manifest}");

    // And every one of them is in the trail: nine files, nine entries, plus the
    // one that says they were taken together
    let taken = exports(&app);
    assert_eq!(taken.len(), yk_dist_manager::report::bundle::CONTENTS.len());
    let bundled: Vec<String> = app
        .store
        .as_ref()
        .unwrap()
        .audit_entries(200)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.event == "export.bundle")
        .map(|entry| entry.details)
        .collect();
    assert_eq!(bundled.len(), 1, "{bundled:?}");
    assert!(bundled[0].contains("files=9"), "{}", bundled[0]);
}

#[test]
fn scenario_exporting_before_generating_is_refused_rather_than_writing_an_empty_file() {
    let home = home();
    let database = home.join("empty.sqlite3");
    register(&database);

    let mut app = YkDistApp::new(Some(database.clone()));
    let target = home.join("nothing.csv");

    assert!(!app.write_report(&target));
    assert!(!target.exists(), "no file, and no audit entry claiming one");
    assert!(exports(&app).is_empty());
    assert!(
        app.reports
            .error
            .as_deref()
            .is_some_and(|e| e.contains("generate a report")),
        "{:?}",
        app.reports.error
    );
}
