//! Behaviour test for the power cycle in front of a FIDO2 reset, when the register
//! cannot be written (`features/key-lifecycle-and-revocation.md` phase 5).
//!
//! The failure this replaces, seen on a share whose SMB session had been torn down
//! under an open register: every write was returning `SQLITE_IOERR`, and the panel
//! asked the operator to pull the key out and plug it back in anyway. Thirty seconds
//! and two hands later the run refused — *the reset could not be recorded, so nothing
//! was written to the key* — which is the right refusal arriving at the worst possible
//! moment, for a fault that was already knowable before anybody touched the key.
//!
//! It was knowable because `begin_power_cycle` writes its own entry first. That entry
//! failed, `record` put `AUDIT FAILURE` in the status line, and the next statement
//! overwrote the status line with *pull the key out*. The refusal was on screen for
//! no frames at all.
//!
//! So: **a register that cannot take the entry that arms a reset does not get to ask
//! for the key.** `device::reset::perform` refuses to write to hardware unless its
//! trail is written (§3), and this is the same rule one step earlier, where it costs
//! the operator nothing.
//!
//! **One test in this file, deliberately** — it drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared by
//! every test in a binary.

use yk_dist_manager::YkDistApp;
use yk_dist_manager::app::Tab;
use yk_dist_manager::device::reset::Applet;
use yk_dist_manager::domain::{SerialSource, YubiKeyRecord};
use yk_dist_manager::store::{Store, StoreConfig};

const SERIAL: u32 = 20_423_633;

#[test]
fn scenario_a_register_that_cannot_record_never_asks_for_the_key() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
    }
    let database = home.path().join("keys.sqlite3");

    // Given a register holding the key the operator wants back at factory default
    {
        let store = Store::create_new(&StoreConfig::new(&database)).unwrap();
        store
            .upsert_key(&YubiKeyRecord::from_serial(
                SERIAL,
                SerialSource::ManualEntry,
            ))
            .unwrap();
        let _ = store.close();
    }

    let mut app = YkDistApp::new(Some(database.clone()));
    assert!(app.store.is_some(), "{:?}", app.db_form.error);

    // And a register this session can no longer write to. Read-only stands in for
    // every way that happens — a share whose session was torn down, a file server
    // that went away, a mount that came back without write access — because what
    // the panel has to do about it is the same in all of them, and this is the one
    // a test can produce without a file server.
    app.store = Some(
        Store::open_read_only(&StoreConfig::new(&database)).expect("the register opens read-only"),
    );

    // When the operator confirms a reset that needs the key power-cycled
    app.tab = Tab::Inventory;
    app.reset.serial = Some(SERIAL);
    app.reset.applets = vec![Applet::Fido2];
    app.reset.typed = SERIAL.to_string();
    app.confirm_key_reset();

    // Then nobody is asked to touch the key: there is no handshake to drive, and
    // nothing is watching the port for a key that has no reason to be pulled out
    assert!(
        app.reset.handshake.is_none(),
        "a register that cannot record the reset must not ask for the key"
    );
    assert!(
        app.reset.presence.is_none(),
        "nothing may poll the port for a power cycle that will not be used"
    );

    // And the operator is told now, in the panel, that the register is the fault —
    // rather than in thirty seconds, with the key in their hand
    let refusal = app
        .reset
        .error
        .clone()
        .expect("the panel says why nothing happened");
    assert!(
        refusal.contains("register"),
        "the register is the fault, and has to be named: {refusal}"
    );
    assert!(
        refusal.contains("nothing"),
        "the operator has to be told the key is untouched: {refusal}"
    );

    // And the status line still carries the audit failure that caused it, instead of
    // the instruction that used to overwrite it one statement later
    assert!(
        app.status.contains("AUDIT FAILURE"),
        "the audit failure may not be overwritten by an instruction: {}",
        app.status
    );
    assert!(
        !app.status.contains("pull the key out"),
        "nobody is being asked to pull anything out: {}",
        app.status
    );
}
