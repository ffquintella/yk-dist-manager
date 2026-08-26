//! Behaviour: forgetting a register the workstation cannot reach any more
//! (`features/database-selection.md`).
//!
//! The case an operator actually meets: the application opens onto "… is not
//! reachable", because `last_database` names a path that is gone. They click
//! *forget* on the row. What must happen is not only that the row disappears —
//! the banner and the path field are still naming that database, and they are the
//! two things on screen that say whether the click did anything at all. If the
//! chooser stays pointed at the forgotten path, forgetting reads as broken even
//! though the list is correct.
//!
//! **One test in this file, deliberately** — it drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared by
//! every test in a binary. `tests/unit_settings.rs` asserts that no test in this
//! repository forgets that redirection: one that did wrote its temporary register
//! into a real operator's recent list, which is the other half of "forget does not
//! hold".

use std::path::Path;

use yk_dist_manager::YkDistApp;
use yk_dist_manager::app::DbRequest;
use yk_dist_manager::settings::AppSettings;
use yk_dist_manager::store::{Store, StoreConfig};
use yk_dist_manager::vault::MemoryVault;

/// Redirect everything this test would otherwise write to the operator's home,
/// including the credential store.
fn scratch(home: &Path) {
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home);
        std::env::set_var("YKDM_SETTINGS", home.join("settings.json"));
        std::env::set_var("YKDM_NO_SAVED_PASSWORD", "1");
    }
}

#[test]
fn scenario_forgetting_an_unreachable_register_moves_the_chooser_off_it() {
    let home = tempfile::tempdir().unwrap();
    scratch(home.path());

    // Given a workstation that remembers two registers: one still here, and one
    // on a share that is not mounted — the second being the one it opened last
    let reachable = home.path().join("keys.sqlite3");
    let _ = Store::create_new(&StoreConfig::new(&reachable).with_operator("felipe"))
        .expect("the register is created")
        .close();
    let gone = home.path().join("not-mounted").join("unit-keys.sqlite3");

    let mut settings = AppSettings::load();
    settings.remember(&reachable);
    settings.remember(&gone);
    settings.save().expect("settings are written");

    // And the application therefore opens onto the chooser, saying so
    let mut app = YkDistApp::with_vault(None, Box::new(MemoryVault::new()));
    assert!(app.store.is_none(), "nothing can be open: the path is gone");
    let banner = app.open_error.clone().expect("the chooser says why");
    assert!(
        banner.contains(&gone.display().to_string()),
        "the banner names the register it could not reach: {banner}"
    );
    assert_eq!(app.db_form.path, gone.display().to_string());

    // When the operator forgets that row
    app.db_request = Some(DbRequest::Forget(gone.clone()));
    app.handle_db_request();

    // Then it is gone from the list, and from what opens at startup
    assert_eq!(
        app.settings.recent_databases,
        vec![reachable.clone()],
        "the row the operator forgot is the only one removed"
    );
    assert_eq!(
        app.settings.last_database, None,
        "and it is no longer what the next launch reaches for"
    );

    // And nothing on screen is still naming it: the chooser has moved on to the
    // register that is left, which is what makes the click visibly do something
    assert!(app.open_error.is_none(), "{:?}", app.open_error);
    assert!(app.db_form.error.is_none(), "{:?}", app.db_form.error);
    assert_eq!(app.db_form.path, reachable.display().to_string());

    // And it holds: what a restart reads is the file on disk, not this session
    let reloaded = AppSettings::load();
    assert_eq!(reloaded.recent_databases, vec![reachable.clone()]);
    assert_eq!(reloaded.last_database, None);
    let raw = std::fs::read_to_string(AppSettings::path()).unwrap();
    assert!(
        !raw.contains("not-mounted"),
        "the forgotten path is out of the settings file: {raw}"
    );

    // And forgetting the last one leaves a chooser that still offers somewhere to
    // go, rather than an empty field
    app.db_request = Some(DbRequest::Forget(reachable.clone()));
    app.handle_db_request();
    assert!(app.settings.recent_databases.is_empty());
    assert!(
        !app.db_form.path.is_empty(),
        "the field falls back to this workstation's default"
    );
}
