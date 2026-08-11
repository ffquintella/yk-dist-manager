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

// ------------------------------------------------------------------ filtering

mod filtering {
    use chrono::{Duration, Utc};
    use yk_dist_manager::audit::{AuditEntry, AuditFilter};

    fn entry(seq: u64, actor: &str, event: &str, target: &str, days_ago: i64) -> AuditEntry {
        let mut entry = AuditEntry {
            seq,
            at: Utc::now() - Duration::days(days_ago),
            actor: actor.into(),
            event: event.into(),
            target: target.into(),
            details: String::new(),
            prev_hash: yk_dist_manager::audit::GENESIS.into(),
            hash: String::new(),
        };
        entry.hash = entry.compute_hash();
        entry
    }

    fn sample() -> Vec<AuditEntry> {
        vec![
            entry(1, "ana", "key.added", "serial:20423633", 10),
            entry(2, "ana", "key.distributed", "serial:20423633", 5),
            entry(3, "bruno", "template.changed", "org-standard v2", 3),
            entry(4, "bruno", "key.added", "serial:20423634", 1),
        ]
    }

    #[test]
    fn an_empty_filter_shows_everything() {
        let filter = AuditFilter::default();
        assert!(filter.is_empty());
        assert_eq!(filter.apply(&sample()).len(), 4);
    }

    #[test]
    fn a_serial_is_found_by_typing_the_number_alone() {
        // The target is stored `serial:20423633`; an operator types the digits.
        let filter = AuditFilter {
            target: "20423633".into(),
            ..Default::default()
        };
        assert_eq!(filter.apply(&sample()).len(), 2);
    }

    #[test]
    fn an_event_prefix_selects_a_whole_family() {
        let filter = AuditFilter {
            event: "key.".into(),
            ..Default::default()
        };
        assert_eq!(filter.apply(&sample()).len(), 3);
    }

    #[test]
    fn matching_ignores_case() {
        let filter = AuditFilter {
            actor: "ANA".into(),
            ..Default::default()
        };
        assert_eq!(filter.apply(&sample()).len(), 2);
    }

    #[test]
    fn a_date_range_is_inclusive_at_both_ends() {
        let entries = sample();
        let (from, until) = (entries[1].at, entries[2].at);
        let filter = AuditFilter {
            from: Some(from),
            until: Some(until),
            ..Default::default()
        };
        let matched = filter.apply(&entries);
        assert_eq!(matched.len(), 2, "both bounds are inclusive");
        assert_eq!(matched[0].seq, 2);
        assert_eq!(matched[1].seq, 3);
    }

    #[test]
    fn filters_combine_with_and_not_or() {
        let filter = AuditFilter {
            actor: "bruno".into(),
            event: "key.".into(),
            ..Default::default()
        };
        let entries = sample();
        let matched = filter.apply(&entries);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].seq, 4);
    }

    #[test]
    fn a_filtered_screen_says_it_is_filtered() {
        // A filtered list that looks like the whole trail is how somebody
        // concludes an event never happened.
        let filter = AuditFilter {
            actor: "ana".into(),
            ..Default::default()
        };
        let description = filter.describe(2, 4);
        assert!(description.contains("2 of 4"), "{description}");
        assert!(description.contains("actor~ana"), "{description}");

        assert_eq!(AuditFilter::default().describe(4, 4), "4 entries");
    }
}

// --------------------------------------------------------------------- mirror

mod mirror {
    use yk_dist_manager::audit::{AuditLog, MirrorStatus, compare_with_mirror};

    fn chain(
        dir: &tempfile::TempDir,
        name: &str,
        events: &[&str],
    ) -> Vec<yk_dist_manager::audit::AuditEntry> {
        let mut log = AuditLog::open(dir.path().join(name)).unwrap();
        for event in events {
            log.append("ana", event, "serial:1", "").unwrap();
        }
        log.entries().unwrap()
    }

    #[test]
    fn two_identical_chains_are_in_sync() {
        let dir = tempfile::tempdir().unwrap();
        let a = chain(&dir, "a.jsonl", &["key.added", "key.distributed"]);
        let b = chain(&dir, "b.jsonl", &["key.added", "key.distributed"]);
        // Hashes cover the timestamp, so two independently written chains differ.
        // Compare a chain with itself, which is what the mirror actually is.
        assert_eq!(
            compare_with_mirror(&a, &a),
            MirrorStatus::InSync { entries: 2 }
        );
        assert!(matches!(
            compare_with_mirror(&a, &b),
            MirrorStatus::Diverged { seq: 1 }
        ));
    }

    #[test]
    fn a_mirror_that_missed_an_entry_is_reported_as_behind() {
        let dir = tempfile::tempdir().unwrap();
        let database = chain(&dir, "db.jsonl", &["key.added", "key.distributed"]);
        let mirror = database[..1].to_vec();

        let status = compare_with_mirror(&database, &mirror);
        assert_eq!(
            status,
            MirrorStatus::Behind {
                database: 2,
                mirror: 1
            }
        );
        assert!(status.is_alert());
        assert!(
            status.describe().contains("BEHIND"),
            "{}",
            status.describe()
        );
    }

    #[test]
    fn a_rewritten_entry_is_reported_as_divergence_naming_the_entry() {
        // This is the case the mirror exists for: the database triggers refuse
        // every ordinary edit, so a chain that changed anyway was rebuilt — and
        // a rebuild cannot reach a copy on storage the operator cannot rewrite.
        let dir = tempfile::tempdir().unwrap();
        let database = chain(&dir, "db.jsonl", &["key.added", "key.distributed"]);
        let mut mirror = database.clone();
        mirror[1].details = "tampered".into();
        mirror[1].hash = mirror[1].compute_hash();

        assert_eq!(
            compare_with_mirror(&database, &mirror),
            MirrorStatus::Diverged { seq: 2 }
        );
    }

    #[test]
    fn no_mirror_configured_is_not_an_alert() {
        // Whether segregated audit storage is required here is an ESI decision
        // the roadmap records as open. Until it is answered, absence is normal.
        assert!(!MirrorStatus::NotConfigured.is_alert());
        assert!(!MirrorStatus::InSync { entries: 3 }.is_alert());
    }
}
