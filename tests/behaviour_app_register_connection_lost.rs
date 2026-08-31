//! Behaviour test for a register whose *connection* dies while its file stays put
//! (`features/smb-share-hosting.md` phase 9).
//!
//! Phase 9 already watches for the mount point disappearing, which is the visible way
//! a file server goes away and the only way a `ShareConnection` this session opened
//! can. The invisible way is a share the operating system mounted: it stays mounted
//! and the path keeps resolving, while the SMB session behind the descriptors this
//! process already holds is torn down — by a workstation that slept, a link that
//! flapped, a file server that restarted.
//!
//! What that looked like before this: every write returning `disk I/O error`, an
//! inventory that stayed empty because nothing could be saved, and a factory reset
//! refusing at the last moment because its trail could not be written. All of it
//! true, none of it pointing at the register — and no way back short of quitting the
//! application, because `tick_share_health`'s `is_file` says yes to a mount that is
//! still there.
//!
//! So a failure that says *the connection is dead* rather than *this operation went
//! wrong* is told apart from every other database error and acted on: let go of the
//! register — never close it politely, the connection is what stopped answering —
//! and open the file again, once, immediately. A connection that has already been
//! re-established is the common case.
//!
//! **One test in this file, deliberately** — it drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared by
//! every test in a binary.

use std::path::Path;

use yk_dist_manager::YkDistApp;
use yk_dist_manager::domain::{SerialSource, YubiKeyRecord};
use yk_dist_manager::store::{Store, StoreConfig};

const SERIAL: u32 = 20_423_633;

/// What SQLite says, and what the operator saw on the screen that started this.
const REASON: &str = "database error: disk I/O error";

/// The register is reached through a mount point, and taken out of reach by taking
/// that mount point away — never by moving the file, which is both the wrong story
/// (the register keeps existing on the server) and impossible on Windows while this
/// process holds the connection open, the very state this scenario is about.
///
/// Same pair of helpers as `behaviour_app_share_dropped`, for the same reason.
fn mount(server_side: &Path, at: &Path) {
    link(server_side, at).expect("the mount point is made");
    assert!(at.is_dir(), "the mount point leads to the share");
}

fn unmount(at: &Path) {
    unlink(at).expect("the mount point goes away");
    assert!(!at.exists(), "the mount point is gone");
}

#[cfg(unix)]
fn link(server_side: &Path, at: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(server_side, at)
}

#[cfg(unix)]
fn unlink(at: &Path) -> std::io::Result<()> {
    std::fs::remove_file(at)
}

/// A junction, not a symlink: `mklink /J` needs no privilege, while a directory
/// symlink needs `SeCreateSymbolicLinkPrivilege` or developer mode — which a build
/// agent may not have. A junction is also what a mapped share's mount point is.
#[cfg(windows)]
fn link(server_side: &Path, at: &Path) -> std::io::Result<()> {
    let out = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(at)
        .arg(server_side)
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "mklink /J failed: {}{}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim(),
        )))
    }
}

/// `remove_file` refuses a junction; removing the directory entry is what takes a
/// reparse point away, and it leaves the directory it points at — and the file this
/// process still has open under it — alone.
#[cfg(windows)]
fn unlink(at: &Path) -> std::io::Result<()> {
    std::fs::remove_dir(at)
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
fn scenario_a_register_whose_connection_died_is_let_go_of_and_opened_again() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
    }
    // The register lives on the "file server"; this workstation reaches it through a
    // mount point, which is what the operating system mounted the share as.
    let server_side = home.path().join("server-side-share");
    let root = home.path().join("mounted-share");
    std::fs::create_dir_all(&server_side).unwrap();
    mount(&server_side, &root);
    let database = root.join("keys.sqlite3");

    // Given a register with history on it, open and being worked in
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
    app.refresh();
    assert_eq!(app.keys.len(), 1, "the register's history is on screen");

    // When the connection stops answering and the file is genuinely out of reach —
    // the share was not back after all
    unmount(&root);
    assert!(!database.is_file(), "the register is not reachable");
    app.handle_register_connection_lost(REASON.to_owned());

    // Then the register is let go of rather than held open, and the operator is told
    // where it stands: the file is on the file server, it is intact, and nothing was
    // written while the connection was out
    assert!(
        app.store.is_none(),
        "a register that cannot be opened must not be held open"
    );
    let said = app.open_error.clone().expect("the operator is told");
    assert!(
        said.contains("intact"),
        "the operator has to be told the register survived: {said}"
    );
    // And it is offered back where every other register is offered: reopening it is
    // the whole fix, so the path is already in the box
    assert_eq!(app.db_form.path, database.display().to_string());
    // Nothing tried to write a closing entry over the connection that stopped
    // answering, which is what the polite close would have done
    assert!(
        !app.status.contains("AUDIT FAILURE"),
        "letting go is not a failure to audit: {}",
        app.status
    );

    // When the file is where it was — the ordinary case, a session torn down under a
    // mount that never moved
    mount(&server_side, &root);
    assert!(database.is_file(), "the file server answers again");
    app.handle_register_connection_lost(REASON.to_owned());

    // Then the register is open again, without the operator doing anything: only this
    // session's handle on it was ever dead
    assert!(
        app.store.is_some(),
        "a connection that came back is the common case and costs one open: {:?}",
        app.open_error
    );
    assert_eq!(
        app.keys.len(),
        1,
        "the history comes back with the register — nothing was lost"
    );
    assert!(
        app.status.contains("reopened"),
        "the operator is told the register came back: {}",
        app.status
    );

    // And the round trip is on the register that came back, which is the only place
    // it can be written: the gap itself has no entry and cannot have one
    let reopened = events(&app, "db.reopened");
    assert_eq!(reopened.len(), 1, "{reopened:?}");
    assert!(
        reopened[0].contains("disk I/O error"),
        "the entry carries what actually failed: {}",
        reopened[0]
    );

    // When it drops again straight afterwards — a mount that answers `open` and then
    // fails everything, which is the shape that would otherwise be abandoned and
    // reopened on every frame, writing an entry each time
    app.handle_register_connection_lost(REASON.to_owned());

    // Then it is *not* reopened a second time. The register is let go of, the operator
    // is told it is intact and left in charge of when to try again.
    assert!(
        app.store.is_none(),
        "a register that will not stay open must not be reopened at the operator on a loop"
    );
    let said = app.open_error.clone().expect("the operator is told");
    assert!(
        said.contains("intact"),
        "still intact, and still worth saying: {said}"
    );
    assert!(
        app.status.contains("keeps dropping"),
        "the second drop is a different story from the first: {}",
        app.status
    );
}
