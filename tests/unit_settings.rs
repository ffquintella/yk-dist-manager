//! Unit tests for the settings file — the thing that remembers which database to
//! open.
//!
//! One test owns the on-disk round trip, because `$YKDM_SETTINGS` is
//! process-global and these tests share a process.

use std::path::{Path, PathBuf};

use yk_dist_manager::settings::{AppSettings, MAX_RECENT};

#[test]
fn the_settings_file_round_trips_and_survives_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    unsafe { std::env::set_var("YKDM_SETTINGS", &path) };

    // A missing file is first run, not an error.
    let fresh = AppSettings::load();
    assert!(fresh.recent_databases.is_empty());
    assert!(
        !fresh.operator.is_empty(),
        "the operator falls back to the OS user"
    );

    // Save, reload, and the choices come back.
    let mut settings = fresh;
    settings.remember(Path::new("/Volumes/ti-share/keys.sqlite3"));
    settings.remember(Path::new("/Users/felipe/local.sqlite3"));
    settings.operator = "felipe".into();
    settings.org = "FGV".into();
    settings.save().expect("saves");

    let reloaded = AppSettings::load();
    assert_eq!(
        reloaded.last_database.as_deref(),
        Some(Path::new("/Users/felipe/local.sqlite3")),
        "the newest is the one reopened at startup"
    );
    assert_eq!(reloaded.recent_databases.len(), 2);
    assert_eq!(reloaded.operator, "felipe");
    assert_eq!(reloaded.org, "FGV");

    // A password must never end up here: the file sits next to the database, so
    // storing one would defeat encrypting it.
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        !raw.to_lowercase().contains("password"),
        "settings.json must have no password field: {raw}"
    );

    // Corruption degrades to defaults rather than stopping the operator working.
    std::fs::write(&path, "{ this is not json").unwrap();
    let recovered = AppSettings::load();
    assert!(recovered.recent_databases.is_empty());

    // Duplicates and blanks in a hand-edited file are normalised away.
    std::fs::write(
        &path,
        r#"{"recent_databases":["/a.sqlite3","/a.sqlite3","/b.sqlite3"],"operator":"  "}"#,
    )
    .unwrap();
    let normalised = AppSettings::load();
    assert_eq!(
        normalised.recent_databases,
        vec![PathBuf::from("/a.sqlite3"), PathBuf::from("/b.sqlite3")]
    );
    assert!(!normalised.operator.trim().is_empty());

    unsafe { std::env::remove_var("YKDM_SETTINGS") };
}

#[test]
fn availability_is_reported_per_entry_without_dropping_anything() {
    let dir = tempfile::tempdir().unwrap();
    let present = dir.path().join("here.sqlite3");
    std::fs::write(&present, b"").unwrap();

    let mut settings = AppSettings::default();
    settings.remember(Path::new("/Volumes/never-mounted/keys.sqlite3"));
    settings.remember(&present);

    let listed = settings.recent_with_availability();
    assert_eq!(listed.len(), 2, "an unreachable share stays listed");
    assert_eq!(listed[0], (present.clone(), true));
    assert_eq!(
        listed[1],
        (PathBuf::from("/Volumes/never-mounted/keys.sqlite3"), false)
    );
}

#[test]
fn the_recent_list_never_exceeds_its_cap_or_repeats_an_entry() {
    let mut settings = AppSettings::default();
    for n in 0..(MAX_RECENT * 2) {
        settings.remember(Path::new(&format!("/db-{n}.sqlite3")));
    }
    settings.remember(Path::new("/db-0.sqlite3"));

    assert_eq!(settings.recent_databases.len(), MAX_RECENT);
    assert_eq!(
        settings.recent_databases[0],
        PathBuf::from("/db-0.sqlite3"),
        "reopening an old database brings it back to the front"
    );
    let unique: std::collections::BTreeSet<_> = settings.recent_databases.iter().collect();
    assert_eq!(unique.len(), settings.recent_databases.len());
}
