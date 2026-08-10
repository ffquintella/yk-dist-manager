//! Unit tests for the hash-chained audit trail (file sink and chain checks).

use yk_dist_manager::audit::{AuditLog, GENESIS, verify};

fn log_in(dir: &tempfile::TempDir) -> AuditLog {
    AuditLog::open(dir.path().join("audit.jsonl")).expect("opens")
}

#[test]
fn first_entry_links_to_genesis() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = log_in(&dir);
    let entry = log
        .append("felipe", "key.added", "serial:20423633", "")
        .unwrap();
    assert_eq!(entry.seq, 1);
    assert_eq!(entry.prev_hash, GENESIS);
    assert_eq!(entry.hash.len(), 64);
}

#[test]
fn entries_chain_and_verify() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = log_in(&dir);
    let first = log.append("felipe", "key.added", "serial:1", "").unwrap();
    let second = log
        .append("felipe", "key.distributed", "serial:1", "to=ana")
        .unwrap();

    assert_eq!(second.prev_hash, first.hash);
    assert_eq!(log.verify().unwrap(), 2);
}

#[test]
fn reopening_continues_the_chain() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("audit.jsonl");
    {
        let mut log = AuditLog::open(&path).unwrap();
        log.append("felipe", "a", "t", "").unwrap();
    }
    let mut reopened = AuditLog::open(&path).unwrap();
    let entry = reopened.append("felipe", "b", "t", "").unwrap();
    assert_eq!(entry.seq, 2, "sequence must not restart");
    assert_eq!(reopened.verify().unwrap(), 2);
}

#[test]
fn editing_an_entry_breaks_the_chain() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = log_in(&dir);
    log.append("felipe", "key.added", "serial:1", "").unwrap();
    log.append("felipe", "key.distributed", "serial:1", "to=ana")
        .unwrap();

    let mut entries = log.entries().unwrap();
    entries[1].details = "to=someone-else".into();

    let err = verify(&entries).expect_err("tampering must be detected");
    assert!(
        err.to_string().contains("does not match its hash"),
        "got: {err}"
    );
}

#[test]
fn deleting_an_entry_breaks_the_chain() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = log_in(&dir);
    for n in 0..3 {
        log.append("felipe", "event", &format!("t{n}"), "").unwrap();
    }
    let mut entries = log.entries().unwrap();
    entries.remove(1);
    assert!(verify(&entries).is_err(), "a gap must be detected");
}

#[test]
fn empty_chain_is_valid() {
    let dir = tempfile::tempdir().unwrap();
    let log = log_in(&dir);
    assert_eq!(log.verify().unwrap(), 0);
}
