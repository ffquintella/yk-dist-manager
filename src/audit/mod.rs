//! Append-only, hash-chained audit trail.
//!
//! NRM §5.3.1 requires an audit trail that nobody can alter or delete, stored
//! apart from the operational data, and optimised for insertion. This module
//! provides the application half: an append-only JSONL file where each entry
//! carries the hash of the previous one, so any insertion, deletion or edit is
//! detectable by [`verify`].
//!
//! The chain is *tamper-evident*, not tamper-proof — enforcement by storage
//! permissions (or a segregated database instance) is tracked in
//! `features/audit-trail.md`.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The all-zero hash that opens every chain.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub seq: u64,
    pub at: DateTime<Utc>,
    /// Operator credential that performed the action.
    pub actor: String,
    /// Dotted event name, e.g. `key.distributed`, `bootstrap.step.done`.
    pub event: String,
    /// Entity affected, e.g. `serial:20423633`.
    pub target: String,
    /// Context. Never a PIN, PUK, management key or access code.
    pub details: String,
    pub prev_hash: String,
    pub hash: String,
}

impl AuditEntry {
    /// Canonical byte string that the hash covers. Field order is part of the
    /// format: changing it invalidates every existing chain.
    fn payload(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}",
            self.seq,
            self.at.to_rfc3339(),
            self.actor,
            self.event,
            self.target,
            self.details,
            self.prev_hash
        )
    }

    /// Hash the entry given its already-populated `prev_hash`.
    pub fn compute_hash(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.payload().as_bytes());
        hex::encode(hasher.finalize())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("audit i/o error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("audit entry {seq} is not valid JSON: {source}")]
    Decode {
        seq: u64,
        #[source]
        source: serde_json::Error,
    },
    #[error("audit chain broken at entry {seq}: {reason}")]
    Broken { seq: u64, reason: String },
}

/// Handle over the audit file. Cheap to construct; every append is a fresh
/// open-append-flush so a crash cannot lose an acknowledged entry.
pub struct AuditLog {
    path: PathBuf,
    last_hash: String,
    next_seq: u64,
}

impl AuditLog {
    /// Open (or create) the log, reading the chain head so appends continue it.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, AuditError> {
        let path = path.into();
        let entries = read_all(&path)?;
        let (last_hash, next_seq) = match entries.last() {
            Some(entry) => (entry.hash.clone(), entry.seq + 1),
            None => (GENESIS.to_owned(), 1),
        };
        Ok(Self {
            path,
            last_hash,
            next_seq,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one entry and return it.
    pub fn append(
        &mut self,
        actor: &str,
        event: &str,
        target: &str,
        details: &str,
    ) -> Result<AuditEntry, AuditError> {
        let mut entry = AuditEntry {
            seq: self.next_seq,
            at: Utc::now(),
            actor: actor.to_owned(),
            event: event.to_owned(),
            target: target.to_owned(),
            details: details.to_owned(),
            prev_hash: self.last_hash.clone(),
            hash: String::new(),
        };
        entry.hash = entry.compute_hash();

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| AuditError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|source| AuditError::Io {
                path: self.path.clone(),
                source,
            })?;

        let line = serde_json::to_string(&entry).expect("audit entry is serialisable");
        writeln!(file, "{line}")
            .and_then(|_| file.flush())
            .map_err(|source| AuditError::Io {
                path: self.path.clone(),
                source,
            })?;

        self.last_hash = entry.hash.clone();
        self.next_seq += 1;
        Ok(entry)
    }

    /// Append an entry that was built and hashed somewhere else, verbatim.
    ///
    /// This is what makes the log usable as a **mirror** of the database's
    /// chain rather than a second chain about the same events. Re-deriving the
    /// entry here would give it this machine's clock and this file's sequence,
    /// so its hash would differ from the database's for every entry — and the
    /// comparison that catches a rebuilt chain
    /// ([`compare_with_mirror`]) would be comparing two things that were never
    /// meant to be equal.
    ///
    /// Nothing is recomputed and nothing is checked: the caller owns the chain,
    /// and a mirror that "corrected" what it was given would destroy the very
    /// evidence it exists to preserve.
    pub fn append_existing(&mut self, entry: &AuditEntry) -> Result<(), AuditError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| AuditError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|source| AuditError::Io {
                path: self.path.clone(),
                source,
            })?;

        let line = serde_json::to_string(entry).expect("audit entry is serialisable");
        writeln!(file, "{line}")
            .and_then(|_| file.flush())
            .map_err(|source| AuditError::Io {
                path: self.path.clone(),
                source,
            })?;

        self.last_hash = entry.hash.clone();
        self.next_seq = entry.seq + 1;
        Ok(())
    }

    /// Every entry, oldest first.
    pub fn entries(&self) -> Result<Vec<AuditEntry>, AuditError> {
        read_all(&self.path)
    }

    /// Verify the whole chain on disk.
    pub fn verify(&self) -> Result<usize, AuditError> {
        let entries = read_all(&self.path)?;
        verify(&entries)?;
        Ok(entries.len())
    }
}

fn read_all(path: &Path) -> Result<Vec<AuditEntry>, AuditError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = std::fs::File::open(path).map_err(|source| AuditError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let mut entries = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|source| AuditError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: AuditEntry =
            serde_json::from_str(&line).map_err(|source| AuditError::Decode {
                seq: index as u64 + 1,
                source,
            })?;
        entries.push(entry);
    }
    Ok(entries)
}

/// Which entries the Audit screen should show.
///
/// A flat list of the newest 500 entries answers "what happened recently" and
/// nothing else. The questions an audit actually asks are "everything that
/// touched serial 20423633", "everything Ana did", "every template change in
/// June" — so the filter is part of the feature, not a convenience.
///
/// Lives here rather than in the paint code because it is the part worth
/// testing: `src/ui/` is outside the coverage gate precisely because painting is
/// not tested, and a filter that silently drops entries would be the wrong thing
/// to leave untested (`AGENTS.md` §4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditFilter {
    /// Substring of the event name, e.g. `template.` for every template event.
    pub event: String,
    /// Substring of the actor.
    pub actor: String,
    /// Substring of the target, which is where a serial appears
    /// (`serial:20423633`).
    pub target: String,
    /// Inclusive lower bound.
    pub from: Option<DateTime<Utc>>,
    /// Inclusive upper bound.
    pub until: Option<DateTime<Utc>>,
}

impl AuditFilter {
    pub fn is_empty(&self) -> bool {
        self.event.trim().is_empty()
            && self.actor.trim().is_empty()
            && self.target.trim().is_empty()
            && self.from.is_none()
            && self.until.is_none()
    }

    /// Does this entry pass?
    ///
    /// Matching is case-insensitive substring, because an operator at a desk
    /// types `ana` and means `Ana Silva`, and types `20423633` and means
    /// `serial:20423633`.
    pub fn matches(&self, entry: &AuditEntry) -> bool {
        fn contains(haystack: &str, needle: &str) -> bool {
            needle.trim().is_empty()
                || haystack
                    .to_lowercase()
                    .contains(needle.trim().to_lowercase().as_str())
        }

        contains(&entry.event, &self.event)
            && contains(&entry.actor, &self.actor)
            && contains(&entry.target, &self.target)
            && self.from.is_none_or(|from| entry.at >= from)
            && self.until.is_none_or(|until| entry.at <= until)
    }

    /// Apply to a slice, keeping the order it was given in.
    pub fn apply<'a>(&self, entries: &'a [AuditEntry]) -> Vec<&'a AuditEntry> {
        entries.iter().filter(|e| self.matches(e)).collect()
    }

    /// One line describing what is being shown, so a filtered screen never looks
    /// like the whole trail.
    pub fn describe(&self, shown: usize, total: usize) -> String {
        if self.is_empty() {
            return format!("{shown} entries");
        }
        let mut parts = Vec::new();
        for (label, value) in [
            ("event", &self.event),
            ("actor", &self.actor),
            ("target", &self.target),
        ] {
            if !value.trim().is_empty() {
                parts.push(format!("{label}~{}", value.trim()));
            }
        }
        if let Some(from) = self.from {
            parts.push(format!("from {}", from.format("%Y-%m-%d")));
        }
        if let Some(until) = self.until {
            parts.push(format!("until {}", until.format("%Y-%m-%d")));
        }
        format!("{shown} of {total} entries — {}", parts.join(", "))
    }
}

/// What comparing the database's chain with its mirror found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorStatus {
    /// No mirror is configured. Not an error: the mirror is optional until the
    /// ESI decides whether segregated audit storage is required here.
    NotConfigured,
    /// Both chains agree, at this many entries.
    InSync { entries: usize },
    /// The mirror is behind — which is what a failed append looks like, and is
    /// worth an alert rather than a silent gap.
    Behind { database: usize, mirror: usize },
    /// Same length, different content. This is the interesting one: it means one
    /// of the two chains was rewritten.
    Diverged { seq: u64 },
    /// The mirror could not be read at all.
    Unreadable { reason: String },
}

impl MirrorStatus {
    /// Does this need to be in front of the operator?
    pub fn is_alert(&self) -> bool {
        matches!(
            self,
            MirrorStatus::Behind { .. }
                | MirrorStatus::Diverged { .. }
                | MirrorStatus::Unreadable { .. }
        )
    }

    pub fn describe(&self) -> String {
        match self {
            MirrorStatus::NotConfigured => "no audit mirror configured".into(),
            MirrorStatus::InSync { entries } => {
                format!("audit mirror in sync ({entries} entries)")
            }
            MirrorStatus::Behind { database, mirror } => format!(
                "AUDIT MIRROR BEHIND: the database has {database} entries, the mirror {mirror} — \
                 entries were not written to the segregated copy"
            ),
            MirrorStatus::Diverged { seq } => format!(
                "AUDIT MIRROR DIVERGED at entry {seq}: the database and the mirror disagree, which \
                 means one of them was rewritten"
            ),
            MirrorStatus::Unreadable { reason } => {
                format!("AUDIT MIRROR UNREADABLE: {reason}")
            }
        }
    }
}

/// Compare a database chain with its mirror.
///
/// The mirror's value is exactly this comparison: a chain rebuilt in the
/// database no longer matches a copy on storage the operator cannot rewrite, so
/// a tamper that defeats the triggers still shows up here. Both chains are
/// assumed already verified individually — this answers "do they agree", not "is
/// each one internally consistent".
pub fn compare_with_mirror(database: &[AuditEntry], mirror: &[AuditEntry]) -> MirrorStatus {
    // Compare the overlap first: a divergence matters more than a gap, because a
    // short mirror is a failed write and a differing one is a rewrite.
    for (a, b) in database.iter().zip(mirror.iter()) {
        if a.hash != b.hash {
            return MirrorStatus::Diverged { seq: a.seq };
        }
    }
    match database.len().cmp(&mirror.len()) {
        std::cmp::Ordering::Equal => MirrorStatus::InSync {
            entries: database.len(),
        },
        std::cmp::Ordering::Greater => MirrorStatus::Behind {
            database: database.len(),
            mirror: mirror.len(),
        },
        // A mirror ahead of the database means the database lost entries — the
        // same alert, read from the other side.
        std::cmp::Ordering::Less => MirrorStatus::Behind {
            database: database.len(),
            mirror: mirror.len(),
        },
    }
}

/// Check sequence continuity, `prev_hash` linkage and each entry's own hash.
pub fn verify(entries: &[AuditEntry]) -> Result<(), AuditError> {
    let mut expected_prev = GENESIS.to_owned();

    for (index, entry) in entries.iter().enumerate() {
        let expected_seq = index as u64 + 1;
        if entry.seq != expected_seq {
            return Err(AuditError::Broken {
                seq: entry.seq,
                reason: format!("expected sequence {expected_seq}"),
            });
        }
        if entry.prev_hash != expected_prev {
            return Err(AuditError::Broken {
                seq: entry.seq,
                reason: "previous-hash link does not match".into(),
            });
        }
        if entry.compute_hash() != entry.hash {
            return Err(AuditError::Broken {
                seq: entry.seq,
                reason: "entry content does not match its hash".into(),
            });
        }
        expected_prev = entry.hash.clone();
    }

    Ok(())
}
