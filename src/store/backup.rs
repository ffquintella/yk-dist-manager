//! Automatic backups of the register, with rotation.
//!
//! Every other storage document in this repository ends with "and a scheduled
//! backup is still required", and until now that was advice to an operator
//! rather than something the tool did. This module is the tool doing it.
//!
//! Three rules shape the code:
//!
//! 1. **A backup is a `VACUUM INTO` copy, not a file copy.** SQLite's own
//!    documentation is explicit that copying a database file while a
//!    transaction is in progress is a way to corrupt it, and on a share or in a
//!    sync folder there is always another process that might be mid-write.
//!    `VACUUM INTO` produces a consistent snapshot from inside the connection.
//! 2. **Rotation never deletes a file it does not recognise.** Pruning walks
//!    only names that match [`backup_name`] exactly, parses the timestamp out of
//!    each, and leaves everything else alone. A directory next to the register
//!    holds the register, its journal, its lock file and the sync client's
//!    conflict copies — deleting the wrong one of those is unrecoverable.
//! 3. **The clock is a parameter.** [`Plan::decide`] takes `now`, so the "is a
//!    backup due?" decision is a pure function a test can drive across a week
//!    without waiting one.
//!
//! The name is `<stem>.<YYYYMMDD-HHMMSS>.backup.sqlite3`, which is the shape the
//! Settings screen's manual backup already wrote, and which
//! [`crate::store::cloud::is_conflict_copy`] already knows is ours rather than a
//! sync client's.

use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};

/// The timestamp format inside a backup filename.
///
/// Sortable as text, no separators a file system objects to, and second
/// resolution — two backups in the same second would be the same backup.
const STAMP: &str = "%Y%m%d-%H%M%S";

/// The suffix that marks one of ours.
const SUFFIX: &str = "backup.sqlite3";

/// How often to take one, and how many to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupPolicy {
    /// Minimum age of the newest backup before another is taken.
    pub every: chrono::Duration,
    /// How many to retain. The newest `keep` survive a prune; older ones go.
    pub keep: usize,
    /// Take one before this session's first write, whatever the interval says.
    ///
    /// The cheapest answer to a fork that has already happened in a sync folder
    /// (`features/cloud-sync-hosting.md` phase 7): if this session is about to
    /// write over a register that another workstation also edited, the state
    /// before the write is the only copy of the losing side.
    pub before_first_write: bool,
}

impl Default for BackupPolicy {
    /// Daily, keep a week, and one before the first write of a session.
    ///
    /// A week of dailies is roughly 7 × the register's size, which for a unit
    /// handing out a few hundred keys is a few megabytes plus whatever signed
    /// scans it holds — cheap against losing the record of who carries what.
    fn default() -> Self {
        Self {
            every: chrono::Duration::days(1),
            keep: 7,
            before_first_write: true,
        }
    }
}

impl BackupPolicy {
    /// Never take one automatically. For a test, a diagnosis, or an operator who
    /// has their own backup arrangement and does not want a second one.
    pub fn disabled() -> Self {
        Self {
            every: chrono::Duration::zero(),
            keep: 0,
            before_first_write: false,
        }
    }

    pub fn is_disabled(&self) -> bool {
        self.keep == 0
    }
}

/// One backup file found next to the register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    pub path: PathBuf,
    pub taken_at: DateTime<Utc>,
}

/// What a prune-and-take would do, decided before anything touches the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Where the new copy goes, or `None` when one is not due.
    pub take: Option<PathBuf>,
    /// Backups to delete, oldest first.
    pub prune: Vec<PathBuf>,
}

impl Plan {
    /// Decide what to do for `database` at `now`.
    ///
    /// Pure: it reads the directory listing it is given rather than the disk, so
    /// the whole policy is testable without a file system or a clock.
    pub fn decide(
        database: &Path,
        existing: &[Backup],
        policy: &BackupPolicy,
        now: DateTime<Utc>,
        forced: bool,
    ) -> Self {
        if policy.is_disabled() {
            return Self {
                take: None,
                prune: Vec::new(),
            };
        }

        let mut sorted: Vec<&Backup> = existing.iter().collect();
        sorted.sort_by_key(|backup| backup.taken_at);

        let due = forced
            || match sorted.last() {
                // `>=` and not `>`: with a zero interval every check is due,
                // which is what a caller asking for "always" means.
                Some(newest) => now - newest.taken_at >= policy.every,
                None => true,
            };

        let take = due.then(|| database.with_file_name(backup_name(database, now)));

        // Prune against the count *after* this backup lands, so keep=7 means
        // seven files on disk when the call returns and not eight.
        let after = sorted.len() + usize::from(take.is_some());
        let excess = after.saturating_sub(policy.keep);
        let prune = sorted
            .iter()
            .take(excess)
            .map(|backup| backup.path.clone())
            .collect();

        Self { take, prune }
    }
}

/// The filename a backup of `database` taken at `at` gets.
pub fn backup_name(database: &Path, at: DateTime<Utc>) -> String {
    let stem = database
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "database".to_owned());
    format!("{stem}.{}.{SUFFIX}", at.format(STAMP))
}

/// Is `name` a backup of the database at `database`, and when was it taken?
///
/// Deliberately strict. Everything this returns `Some` for is a deletion
/// candidate, so the parse is the safety mechanism: an unexpected name is left
/// alone rather than guessed at.
pub fn parse_backup_name(database: &Path, name: &str) -> Option<DateTime<Utc>> {
    let stem = database.file_stem()?.to_string_lossy().into_owned();
    let rest = name.strip_prefix(&format!("{stem}."))?;
    let stamp = rest.strip_suffix(&format!(".{SUFFIX}"))?;
    let naive = NaiveDateTime::parse_from_str(stamp, STAMP).ok()?;
    Utc.from_local_datetime(&naive).single()
}

/// Every backup of `database` sitting next to it, oldest first.
///
/// An unreadable directory is not an error: it means "no backups found", and the
/// caller's next move is to take one, which will fail loudly with a better
/// message than this could give.
pub fn existing(database: &Path) -> Vec<Backup> {
    let Some(dir) = database.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut found: Vec<Backup> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let taken_at = parse_backup_name(database, &name.to_string_lossy())?;
            Some(Backup {
                path: entry.path(),
                taken_at,
            })
        })
        .collect();
    found.sort_by_key(|backup| backup.taken_at);
    found
}

/// What a backup run actually did, for the status line and the audit entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub taken: Option<PathBuf>,
    pub pruned: Vec<PathBuf>,
}

impl Outcome {
    pub fn did_nothing(&self) -> bool {
        self.taken.is_none() && self.pruned.is_empty()
    }

    /// One secret-free line for the audit trail.
    pub fn detail(&self) -> String {
        match &self.taken {
            Some(path) => format!(
                "target={} pruned={}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                self.pruned.len()
            ),
            None => format!("target=none pruned={}", self.pruned.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn backup(path: &str, taken: &str) -> Backup {
        Backup {
            path: PathBuf::from(path),
            taken_at: at(taken),
        }
    }

    #[test]
    fn a_backup_name_round_trips() {
        let db = Path::new("/share/ti/keys.sqlite3");
        let when = at("2026-08-11T12:30:45+00:00");
        let name = backup_name(db, when);
        assert_eq!(name, "keys.20260811-123045.backup.sqlite3");
        assert_eq!(parse_backup_name(db, &name), Some(when));
    }

    #[test]
    fn only_our_own_names_are_deletion_candidates() {
        let db = Path::new("/share/ti/keys.sqlite3");
        // The register itself, its sidecars, a sync client's conflict copy, and
        // a backup of a *different* database in the same folder. Deleting any of
        // these would be unrecoverable, so none may parse.
        for name in [
            "keys.sqlite3",
            "keys.sqlite3-journal",
            "keys.sqlite3.lock",
            "keys-DESKTOP-7 (conflicted copy).sqlite3",
            "other.20260811-123045.backup.sqlite3",
            "keys.notatimestamp.backup.sqlite3",
            "keys.20260811-123045.backup.sqlite3.tmp",
        ] {
            assert_eq!(
                parse_backup_name(db, name),
                None,
                "{name} must not be treated as one of our backups"
            );
        }
    }

    #[test]
    fn the_first_backup_is_always_due() {
        let db = Path::new("/share/keys.sqlite3");
        let plan = Plan::decide(
            db,
            &[],
            &BackupPolicy::default(),
            at("2026-08-11T09:00:00+00:00"),
            false,
        );
        assert!(plan.take.is_some());
        assert!(plan.prune.is_empty());
    }

    #[test]
    fn a_backup_is_not_due_again_within_the_interval() {
        let db = Path::new("/share/keys.sqlite3");
        let existing = [backup(
            "/share/keys.20260811-090000.backup.sqlite3",
            "2026-08-11T09:00:00+00:00",
        )];

        // Four hours later, with a daily policy: nothing to do.
        let plan = Plan::decide(
            db,
            &existing,
            &BackupPolicy::default(),
            at("2026-08-11T13:00:00+00:00"),
            false,
        );
        assert_eq!(plan.take, None);

        // The next day: due.
        let plan = Plan::decide(
            db,
            &existing,
            &BackupPolicy::default(),
            at("2026-08-12T09:00:01+00:00"),
            false,
        );
        assert!(plan.take.is_some());
    }

    #[test]
    fn forcing_takes_one_whatever_the_interval_says() {
        // This is the cloud-sync "before the first write" case: the interval has
        // not elapsed, but the register is about to be written over.
        let db = Path::new("/share/keys.sqlite3");
        let existing = [backup(
            "/share/keys.20260811-090000.backup.sqlite3",
            "2026-08-11T09:00:00+00:00",
        )];
        let plan = Plan::decide(
            db,
            &existing,
            &BackupPolicy::default(),
            at("2026-08-11T09:00:30+00:00"),
            true,
        );
        assert!(plan.take.is_some(), "a forced backup ignores the schedule");
    }

    #[test]
    fn rotation_keeps_the_newest_and_counts_the_one_about_to_land() {
        let db = Path::new("/share/keys.sqlite3");
        let existing: Vec<Backup> = (1..=7)
            .map(|day| {
                backup(
                    &format!("/share/keys.202608{day:02}-090000.backup.sqlite3"),
                    &format!("2026-08-{day:02}T09:00:00+00:00"),
                )
            })
            .collect();

        let policy = BackupPolicy {
            keep: 7,
            ..BackupPolicy::default()
        };
        let plan = Plan::decide(
            db,
            &existing,
            &policy,
            at("2026-08-08T09:00:00+00:00"),
            false,
        );

        assert!(plan.take.is_some());
        assert_eq!(
            plan.prune,
            vec![PathBuf::from("/share/keys.20260801-090000.backup.sqlite3")],
            "exactly one goes, so seven remain once the new one lands"
        );
    }

    #[test]
    fn a_disabled_policy_takes_nothing_and_deletes_nothing() {
        let db = Path::new("/share/keys.sqlite3");
        let existing: Vec<Backup> = (1..=9)
            .map(|day| {
                backup(
                    &format!("/share/keys.202608{day:02}-090000.backup.sqlite3"),
                    &format!("2026-08-{day:02}T09:00:00+00:00"),
                )
            })
            .collect();

        // Even forced, and even with more files than any `keep` would allow:
        // an operator who turned this off keeps their files.
        let plan = Plan::decide(
            db,
            &existing,
            &BackupPolicy::disabled(),
            at("2026-09-01T09:00:00+00:00"),
            true,
        );
        assert_eq!(plan.take, None);
        assert!(plan.prune.is_empty());
    }

    #[test]
    fn existing_backups_are_found_and_ordered_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("keys.sqlite3");
        std::fs::write(&db, b"not really a database").unwrap();

        for stamp in ["20260811-090000", "20260809-090000", "20260810-090000"] {
            std::fs::write(
                dir.path().join(format!("keys.{stamp}.backup.sqlite3")),
                b"x",
            )
            .unwrap();
        }
        // Two files that must be ignored entirely.
        std::fs::write(dir.path().join("keys.sqlite3-journal"), b"x").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();

        let found = existing(&db);
        assert_eq!(found.len(), 3, "only the three backups");
        assert_eq!(found[0].taken_at, at("2026-08-09T09:00:00+00:00"));
        assert_eq!(found[2].taken_at, at("2026-08-11T09:00:00+00:00"));
    }

    #[test]
    fn an_outcome_reports_itself_without_a_path_to_a_secret() {
        let outcome = Outcome {
            taken: Some(PathBuf::from("/share/keys.20260811-090000.backup.sqlite3")),
            pruned: vec![PathBuf::from("/share/keys.20260801-090000.backup.sqlite3")],
        };
        assert_eq!(
            outcome.detail(),
            "target=keys.20260811-090000.backup.sqlite3 pruned=1"
        );
        assert!(!outcome.did_nothing());
        assert!(Outcome::default().did_nothing());
    }
}
