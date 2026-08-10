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
