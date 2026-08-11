//! Hosting the database in a **cloud-sync folder** (OneDrive, Dropbox, Google
//! Drive, iCloud).
//!
//! This is the location the tool would rather not be in, and the one real
//! installations actually chose: a `--diagnose` report showed the database under
//! `~/Library/CloudStorage/OneDrive-…/`. A sync client copies the file underneath
//! a writer and resolves a clash by keeping **both** copies, so the untreated
//! failure mode is two divergent registers of who holds which security token.
//!
//! SQLite cannot coordinate two machines through a sync client — there is no
//! shared lock manager, and the file arrives whole-file, minutes late. So access
//! is made **strictly sequential** instead, by a cooperative protocol that runs
//! outside the database:
//!
//! 1. **Wait for the download.** Before opening, wait until the file has stopped
//!    changing on disk ([`wait_until_settled`]), so the connection is opened on
//!    the version the sync client finished bringing down and not on a half-written
//!    one.
//! 2. **Take a lock file.** `<database>.lock` next to the database, created
//!    exclusively ([`SyncLease::acquire`]), holding *who* has it: host, operator,
//!    pid, when it was taken and when it was last renewed.
//! 3. **Refuse a second workstation.** A lock held by somebody else is a refusal
//!    with their name on it, not a queue.
//! 4. **Release on close.** The connection closes, the file is given time to go
//!    back up, and *then* the lock is removed ([`SyncLease::release`]).
//! 5. **Notice a clash anyway.** [`conflict_copies`] looks for the files a sync
//!    client leaves when it could not decide, because the protocol is cooperative
//!    and the machine that ignores it is the one that matters.
//!
//! What this does **not** do, and must not be read as doing: it is not a
//! distributed lock. A workstation that is offline sees neither the lock nor the
//! data, and a lock created on two machines in the same sync interval is resolved
//! by the sync client like any other clash. It converts the common accident —
//! two operators opening the shared register at the same time — from silent
//! corruption into a refusal that names the other operator. A real network share
//! is still the better answer, and a scheduled backup is still required.
//!
//! Nothing here writes a secret: the lock file holds an operator name, a host
//! name and a pid, which is the same identity the audit trail already records.

use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Appended to the database file name to make the lock file name.
///
/// Appended, not substituted: `keys.sqlite3.lock` cannot collide with a database
/// called `keys.lock`, and it sorts next to the file it belongs to.
pub const LOCK_SUFFIX: &str = ".lock";

/// A lock unrenewed for this long may be taken over by another workstation.
///
/// The window is deliberately wide. A workstation that goes to sleep mid-session
/// stops renewing without releasing, and breaking its lock after a minute would
/// hand the register to a second operator while the first still has it open. The
/// cost of the wide window is that a crashed session blocks the others until it
/// expires — which is why taking over is offered as an explicit operator action
/// rather than waited out.
pub const STALE_AFTER: Duration = Duration::from_secs(15 * 60);

/// How often a held lock is rewritten, so another workstation can tell a live
/// session from an abandoned one.
pub const RENEW_EVERY: Duration = Duration::from_secs(60);

/// How long to wait for the sync client, and how patiently.
///
/// These are the two numbers an operator on a slow link needs to change, so they
/// are overridable from the environment (`$YKDM_SYNC_QUIET_MS`,
/// `$YKDM_SYNC_TIMEOUT_MS`) and documented in `docs/operations.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncPolicy {
    /// The file must be unchanged for this long before it counts as settled.
    pub quiet: Duration,
    /// Give up waiting after this, and say so.
    pub timeout: Duration,
    /// How often to look.
    pub poll: Duration,
}

impl Default for SyncPolicy {
    fn default() -> Self {
        Self {
            quiet: Duration::from_millis(1_500),
            timeout: Duration::from_secs(15),
            poll: Duration::from_millis(250),
        }
    }
}

impl SyncPolicy {
    /// The default, with `$YKDM_SYNC_QUIET_MS` / `$YKDM_SYNC_TIMEOUT_MS` applied.
    pub fn from_env() -> Self {
        let default = Self::default();
        Self {
            quiet: env_millis("YKDM_SYNC_QUIET_MS", default.quiet),
            timeout: env_millis("YKDM_SYNC_TIMEOUT_MS", default.timeout),
            ..default
        }
    }

    /// No waiting at all — for a local file, and for tests.
    pub fn immediate() -> Self {
        Self {
            quiet: Duration::ZERO,
            timeout: Duration::ZERO,
            poll: Duration::from_millis(1),
        }
    }
}

/// Read a duration in milliseconds from the environment, ignoring nonsense.
///
/// A hand-edited value that does not parse falls back to the default rather than
/// refusing to start: the variable tunes a wait, and getting it wrong must not
/// stand between an operator and the register.
fn env_millis(key: &str, default: Duration) -> Duration {
    match std::env::var(key) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(millis) => Duration::from_millis(millis),
            Err(_) => {
                tracing::warn!(event = "sync.policy.ignored", variable = key, value = raw);
                default
            }
        },
        Err(_) => default,
    }
}

/// This process's session id, minted once.
///
/// Identity cannot rest on host plus pid: pids are reused, so a lock left behind
/// by an earlier run could be mistaken for this session's own and silently
/// stolen. A per-process id makes "is this my lock?" exact, and turns a lock from
/// a dead run into what it is — somebody else's, to be waited out or taken over
/// deliberately.
fn session_id() -> Uuid {
    static SESSION: std::sync::OnceLock<Uuid> = std::sync::OnceLock::new();
    *SESSION.get_or_init(Uuid::new_v4)
}

/// Who holds the lock. Recorded in the lock file as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseHolder {
    /// Workstation name, so a refusal can say *which* computer.
    pub host: String,
    /// The operator identity the application records elsewhere.
    pub operator: String,
    pub pid: u32,
    /// The run that took it. Not the operator's business, and the only reliable
    /// answer to "is this lock mine?".
    #[serde(default)]
    pub session: Uuid,
    /// Build that took the lock, useful when two versions are in use.
    pub app_version: String,
    pub acquired_at: DateTime<Utc>,
    /// Rewritten every [`RENEW_EVERY`] while the session is alive.
    pub renewed_at: DateTime<Utc>,
}

impl LeaseHolder {
    /// The holder record for this process, right now.
    pub fn local(operator: &str) -> Self {
        let now = Utc::now();
        Self {
            host: local_host(),
            operator: operator.trim().to_owned(),
            pid: std::process::id(),
            session: session_id(),
            app_version: crate::VERSION.to_owned(),
            acquired_at: now,
            renewed_at: now,
        }
    }

    /// A lock file that exists but cannot be read as a holder record.
    ///
    /// A sync client can present a partially written file, and an operator can
    /// edit one by hand. Either way the lock is *held* — the only thing lost is
    /// the name on it, and the timestamp comes from the file itself so staleness
    /// still works.
    pub fn unknown(seen_at: DateTime<Utc>) -> Self {
        Self {
            host: "(unreadable lock file)".into(),
            operator: "(unknown)".into(),
            pid: 0,
            session: Uuid::nil(),
            app_version: String::new(),
            acquired_at: seen_at,
            renewed_at: seen_at,
        }
    }

    /// Is this the lock *this run* took?
    ///
    /// Deliberately not "this machine": a lock left behind by an earlier run on
    /// this workstation is not ours to reuse, and pid reuse would make that
    /// judgement wrong exactly when it matters.
    pub fn is_local(&self) -> bool {
        self.session != Uuid::nil() && self.session == session_id()
    }

    /// Is this lock held by another run on *this* workstation?
    ///
    /// Worth telling apart in a message: "the register is open in another window
    /// on this computer" is a different instruction from "ask the person at the
    /// other desk".
    pub fn is_same_host(&self) -> bool {
        self.host == local_host()
    }

    /// How long since the holder last said it was alive.
    pub fn silent_for(&self) -> Duration {
        (Utc::now() - self.renewed_at).to_std().unwrap_or_default()
    }

    /// Has the holder been quiet long enough to be taken over?
    pub fn is_stale(&self) -> bool {
        self.silent_for() >= STALE_AFTER
    }
}

impl std::fmt::Display for LeaseHolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} on {} (pid {}), holding since {}, last seen {}s ago",
            self.operator,
            self.host,
            self.pid,
            self.acquired_at.format("%Y-%m-%d %H:%M:%SZ"),
            self.silent_for().as_secs()
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    #[error(
        "the database is in use — {holder}. Wait for that session to close it, or, if that \
         computer is off, take the lock over deliberately."
    )]
    Held {
        /// Boxed to keep the error small: it travels inside
        /// [`crate::store::StoreError`], which every store call returns.
        holder: Box<LeaseHolder>,
        stale: bool,
    },
    #[error("could not take the lock file {path}: {reason}")]
    Io { path: PathBuf, reason: String },
}

/// What waiting for the sync client achieved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// Nothing to wait for — the file is not there yet (a new database).
    NotThere,
    /// The file stopped changing.
    Quiet { waited: Duration },
    /// It was still changing when the timeout ran out. Not fatal, but said out
    /// loud: the sync client is mid-transfer, or something else is writing.
    StillChanging { waited: Duration },
}

impl Settled {
    /// True when the file was still moving under us.
    pub fn is_unsettled(&self) -> bool {
        matches!(self, Settled::StillChanging { .. })
    }

    pub fn describe(&self) -> String {
        match self {
            Settled::NotThere => "no file to wait for".into(),
            Settled::Quiet { waited } => {
                format!("sync settled after {}ms", waited.as_millis())
            }
            Settled::StillChanging { waited } => format!(
                "the file was still changing after {}ms — the sync client may not have finished",
                waited.as_millis()
            ),
        }
    }
}

/// The lock file that belongs to a database file.
pub fn lock_path(database: &Path) -> PathBuf {
    let mut raw = database.as_os_str().to_owned();
    raw.push(LOCK_SUFFIX);
    PathBuf::from(raw)
}

/// `(size, modified)` — enough to tell "the sync client is still writing" from
/// "the file has stopped moving", without reading the contents.
fn fingerprint(path: &Path) -> Option<(u64, Option<std::time::SystemTime>)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

/// Wait until `path` has been unchanged for [`SyncPolicy::quiet`].
///
/// This is the "has the download finished?" step. It is a heuristic — no sync
/// client offers a supported "are you done?" API on every platform this runs on —
/// but it is the one that catches the case that matters: opening the database
/// while the file is being replaced under us.
pub fn wait_until_settled(path: &Path, policy: &SyncPolicy) -> Settled {
    let start = Instant::now();
    let mut last = match fingerprint(path) {
        Some(seen) => seen,
        None => return Settled::NotThere,
    };
    let mut quiet_since = Instant::now();

    loop {
        if quiet_since.elapsed() >= policy.quiet {
            return Settled::Quiet {
                waited: start.elapsed(),
            };
        }
        if start.elapsed() >= policy.timeout {
            return Settled::StillChanging {
                waited: start.elapsed(),
            };
        }
        std::thread::sleep(policy.poll);
        match fingerprint(path) {
            Some(seen) if seen != last => {
                last = seen;
                quiet_since = Instant::now();
            }
            Some(_) => {}
            // Vanished mid-wait: the sync client is replacing it. Keep waiting
            // rather than reporting quiet on a file that is not there.
            None => quiet_since = Instant::now(),
        }
    }
}

/// Markers a sync client leaves in the name of a copy it could not merge.
const CONFLICT_MARKERS: [&str; 6] = [
    "conflicted copy", // Dropbox
    "-conflict",
    " conflict",
    "conflito",  // OneDrive, pt-BR
    "cópia em ", // "cópia em conflito"
    "copy of ",
];

/// Files next to the database that look like a sync client's unresolved clash.
///
/// The check is by name, because that is all a sync conflict leaves behind. A
/// numbered copy (`keys (1).sqlite3`) counts: in a sync folder that is what a
/// second machine's version is renamed to, and it is the exact artefact that
/// means "two divergent registers". Our own backups (`keys.<stamp>.backup.sqlite3`)
/// do not match, and neither does the lock file.
pub fn conflict_copies(database: &Path) -> Vec<PathBuf> {
    let Some(parent) = database.parent() else {
        return Vec::new();
    };
    let Some(name) = database.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };

    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|candidate| is_conflict_copy(name, candidate))
        })
        .map(|entry| entry.path())
        .collect();
    found.sort();
    found
}

/// Is `candidate` a sync client's copy of `database`?
///
/// Split out from [`conflict_copies`] so the naming rules are testable without a
/// directory: the file system part is a `read_dir`, the judgement is here.
pub fn is_conflict_copy(database: &str, candidate: &str) -> bool {
    if candidate == database {
        return false;
    }
    let stem = database.split('.').next().unwrap_or(database);
    if stem.is_empty() || !candidate.starts_with(stem) {
        return false;
    }

    let lowered = candidate.to_ascii_lowercase();
    if CONFLICT_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        return true;
    }

    // `keys (1).sqlite3`, `keys (2).sqlite3` — a numbered duplicate.
    numbered_copy(&lowered)
}

/// Does the name contain a ` (N)` group, the way a sync client numbers a copy?
fn numbered_copy(lowered: &str) -> bool {
    let Some(open) = lowered.find(" (") else {
        return false;
    };
    let rest = &lowered[open + 2..];
    let Some(close) = rest.find(')') else {
        return false;
    };
    let digits = &rest[..close];
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// What the acquisition had to say about the state of the folder.
#[derive(Debug, Clone)]
pub struct LeaseReport {
    /// Whether the file had stopped moving before the connection was opened.
    pub settled: Settled,
    /// Sync conflict copies sitting next to the database.
    pub conflicts: Vec<PathBuf>,
    /// The abandoned holder this lock was taken from, when it was taken over.
    pub took_over: Option<LeaseHolder>,
}

/// What a renewal found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Renewal {
    /// Not time yet.
    NotDue,
    Renewed,
    /// Somebody else's holder record is in our lock file. The session must stop
    /// writing: another workstation believes it has the register.
    Lost(LeaseHolder),
}

/// A held single-writer lock on a database in a sync folder.
///
/// Released explicitly by [`SyncLease::release`], which waits for the upload
/// first. [`Drop`] removes the lock file as a backstop for a panic or a hard
/// quit; it cannot wait for the sync client, which is why the explicit path
/// exists.
pub struct SyncLease {
    database: PathBuf,
    lock: PathBuf,
    holder: LeaseHolder,
    report: LeaseReport,
    /// When the lock file was last written, for [`RENEW_EVERY`].
    written: Instant,
    released: bool,
}

impl SyncLease {
    /// Wait for the sync client, then take the lock — or refuse, naming who has it.
    pub fn acquire(
        database: &Path,
        operator: &str,
        policy: &SyncPolicy,
    ) -> Result<Self, LeaseError> {
        Self::acquire_inner(database, operator, policy, false)
    }

    /// Take the lock even though somebody else's holder record is in it.
    ///
    /// Only ever called from an explicit operator action, because the operator is
    /// the one who can know that the other workstation is off. The previous
    /// holder is returned in [`LeaseReport::took_over`] so the audit entry can
    /// name what was broken.
    pub fn take_over(
        database: &Path,
        operator: &str,
        policy: &SyncPolicy,
    ) -> Result<Self, LeaseError> {
        Self::acquire_inner(database, operator, policy, true)
    }

    fn acquire_inner(
        database: &Path,
        operator: &str,
        policy: &SyncPolicy,
        break_existing: bool,
    ) -> Result<Self, LeaseError> {
        // 1. Let the sync client finish bringing the file down.
        let settled = wait_until_settled(database, policy);
        if settled.is_unsettled() {
            tracing::warn!(
                event = "db.sync.unsettled",
                path = %database.display(),
                detail = settled.describe()
            );
        }

        let lock = lock_path(database);
        let mut took_over = None;

        // 2. A lock that is there is held — by another workstation, by another
        //    window on this one, or by a run that died. None of those is ours to
        //    reuse silently, so all three are refused until the operator says to
        //    break it.
        if let Some(existing) = read_holder(&lock) {
            if !break_existing {
                let stale = existing.is_stale();
                return Err(LeaseError::Held {
                    holder: Box::new(existing),
                    stale,
                });
            }
            tracing::warn!(
                event = "db.lock.taken_over",
                path = %lock.display(),
                previous = %existing,
                silent_for_s = existing.silent_for().as_secs() as i64
            );
            took_over = Some(existing);
            remove_lock(&lock);
        }

        // 3. Create it exclusively, so two processes on this machine cannot both
        //    believe they have it.
        let holder = LeaseHolder::local(operator);
        write_lock(&lock, &holder, true).map_err(|e| match e {
            // Lost the race between the check and the create: another process on
            // this machine got there in between.
            LeaseError::Io { .. } if lock.exists() => {
                let holder = read_holder(&lock).unwrap_or_else(|| LeaseHolder::unknown(Utc::now()));
                let stale = holder.is_stale();
                LeaseError::Held {
                    holder: Box::new(holder),
                    stale,
                }
            }
            other => other,
        })?;

        // 4. Confirm it is still ours after a sync interval. Two machines that
        //    created a lock at the same moment both succeeded locally; the sync
        //    client picks one, and the loser must find out here rather than by
        //    writing to the register.
        if policy.quiet > Duration::ZERO {
            std::thread::sleep(policy.quiet);
            match read_holder(&lock) {
                Some(current) if !current.is_local() => {
                    let stale = current.is_stale();
                    return Err(LeaseError::Held {
                        holder: Box::new(current),
                        stale,
                    });
                }
                _ => {}
            }
        }

        let conflicts = conflict_copies(database);
        if !conflicts.is_empty() {
            tracing::error!(
                event = "db.sync.conflict_copies",
                path = %database.display(),
                count = conflicts.len() as i64,
                detail = "a sync client left copies it could not merge next to the database"
            );
        }

        tracing::info!(
            event = "db.lock.acquired",
            path = %lock.display(),
            holder = %holder
        );

        Ok(Self {
            database: database.to_path_buf(),
            lock,
            holder,
            report: LeaseReport {
                settled,
                conflicts,
                took_over,
            },
            written: Instant::now(),
            released: false,
        })
    }

    pub fn holder(&self) -> &LeaseHolder {
        &self.holder
    }

    pub fn lock_file(&self) -> &Path {
        &self.lock
    }

    pub fn report(&self) -> &LeaseReport {
        &self.report
    }

    /// Rewrite the lock file if it is time, so other workstations can see this
    /// session is alive.
    ///
    /// Returns [`Renewal::Lost`] when the lock now belongs to somebody else, which
    /// the caller must treat as "stop writing and close the database".
    pub fn renew_if_due(&mut self) -> Result<Renewal, LeaseError> {
        if self.written.elapsed() < RENEW_EVERY {
            return Ok(Renewal::NotDue);
        }
        self.renew()
    }

    /// Rewrite the lock file now.
    pub fn renew(&mut self) -> Result<Renewal, LeaseError> {
        if let Some(current) = read_holder(&self.lock)
            && !current.is_local()
        {
            tracing::error!(
                event = "db.lock.lost",
                path = %self.lock.display(),
                holder = %current
            );
            return Ok(Renewal::Lost(current));
        }

        self.holder.renewed_at = Utc::now();
        write_lock(&self.lock, &self.holder, false)?;
        self.written = Instant::now();
        Ok(Renewal::Renewed)
    }

    /// Give the file time to go back up, then remove the lock.
    ///
    /// The order is deliberate and is the one deviation from the obvious reading
    /// of the protocol: removing the lock before the upload finished would invite
    /// the next workstation to start from a file that is still on its way, which
    /// is the race the lock exists to prevent.
    pub fn release(mut self, policy: &SyncPolicy) -> Settled {
        let settled = wait_until_settled(&self.database, policy);
        if settled.is_unsettled() {
            tracing::warn!(
                event = "db.sync.upload_pending",
                path = %self.database.display(),
                detail = settled.describe()
            );
        }
        remove_our_lock(&self.lock);
        self.released = true;
        tracing::info!(event = "db.lock.released", path = %self.lock.display());
        settled
    }
}

impl Drop for SyncLease {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        // A panic or a hard quit still has to leave the register openable.
        remove_our_lock(&self.lock);
        tracing::warn!(
            event = "db.lock.released",
            path = %self.lock.display(),
            detail = "released on drop, without waiting for the sync client to finish uploading"
        );
    }
}

/// Read the holder record, if a lock file is there.
///
/// A lock file that is unreadable or unparseable still means *held*
/// ([`LeaseHolder::unknown`]) — a sync client can show a partial file, and
/// treating that as free is the one interpretation that can corrupt the register.
pub fn read_holder(lock: &Path) -> Option<LeaseHolder> {
    let mut file = std::fs::File::open(lock).ok()?;
    let seen_at = file
        .metadata()
        .and_then(|meta| meta.modified())
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(|_| Utc::now());

    let mut raw = String::new();
    if file.read_to_string(&mut raw).is_err() {
        return Some(LeaseHolder::unknown(seen_at));
    }
    match serde_json::from_str::<LeaseHolder>(&raw) {
        Ok(holder) => Some(holder),
        Err(_) => Some(LeaseHolder::unknown(seen_at)),
    }
}

/// Write the holder record. `exclusive` fails when the file already exists.
fn write_lock(lock: &Path, holder: &LeaseHolder, exclusive: bool) -> Result<(), LeaseError> {
    let body = serde_json::to_string_pretty(holder).map_err(|e| LeaseError::Io {
        path: lock.to_path_buf(),
        reason: e.to_string(),
    })?;

    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if exclusive {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }

    let mut file = options.open(lock).map_err(|e| LeaseError::Io {
        path: lock.to_path_buf(),
        reason: if e.kind() == ErrorKind::AlreadyExists {
            "another session created it first".to_owned()
        } else {
            e.to_string()
        },
    })?;
    file.write_all(body.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| LeaseError::Io {
            path: lock.to_path_buf(),
            reason: e.to_string(),
        })
}

/// Remove the lock file **only if it is still this run's**.
///
/// Releasing after losing the lock is a real sequence: another workstation took it
/// over, this session found out at its next renewal and is closing down. Deleting
/// the file then would delete *their* lock and let a third workstation in — turning
/// one recoverable clash into the two-writer case the whole protocol exists to
/// prevent. A lock that has become unreadable is also left alone, for the same
/// reason: it is not provably ours.
fn remove_our_lock(lock: &Path) {
    match read_holder(lock) {
        Some(holder) if !holder.is_local() => {
            tracing::warn!(
                event = "db.lock.not_ours",
                path = %lock.display(),
                holder = %holder,
                detail = "left in place: this lock belongs to another session now"
            );
        }
        _ => remove_lock(lock),
    }
}

/// Remove the lock file, logging rather than propagating: the caller is on its
/// way out, and a lock that could not be deleted is recoverable (it goes stale)
/// while a panic in `Drop` is not.
fn remove_lock(lock: &Path) {
    match std::fs::remove_file(lock) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => {
            tracing::error!(event = "db.lock.remove.failed", path = %lock.display(), reason = %e)
        }
    }
}

/// This workstation's name, for the lock file and the refusal message.
///
/// Falls back to the environment and then to a placeholder: a nameless host
/// makes the message vaguer, never the protocol weaker — the pid and the
/// timestamps carry the rest.
pub fn local_host() -> String {
    let raw = gethostname::gethostname().to_string_lossy().to_string();
    let trimmed = raw.trim();
    if !trimmed.is_empty() {
        return trimmed.to_owned();
    }
    for key in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(value) = std::env::var(key)
            && !value.trim().is_empty()
        {
            return value.trim().to_owned();
        }
    }
    "unknown-host".to_owned()
}

/// The operator identity to record when nobody said one: the logged-in user.
///
/// Shared with [`crate::settings`], so the name in the lock file is the same name
/// the audit trail records.
pub fn local_operator() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_file_sits_next_to_the_database_and_keeps_its_extension() {
        assert_eq!(
            lock_path(Path::new("/x/OneDrive/keys.sqlite3")),
            PathBuf::from("/x/OneDrive/keys.sqlite3.lock")
        );
    }

    #[test]
    fn an_unparseable_lock_file_still_counts_as_held() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("keys.sqlite3.lock");
        std::fs::write(&lock, b"\x00\x01not json").unwrap();

        let holder = read_holder(&lock).expect("a lock file that is there is held");
        assert!(holder.host.contains("unreadable"));
        assert!(!holder.is_local());
    }

    #[test]
    fn a_numbered_or_marked_copy_is_a_conflict_and_a_backup_is_not() {
        let db = "keys.sqlite3";
        assert!(is_conflict_copy(db, "keys (1).sqlite3"));
        assert!(is_conflict_copy(db, "keys-Conflict.sqlite3"));
        assert!(is_conflict_copy(
            db,
            "keys (felipe's conflicted copy 2026-08-11).sqlite3"
        ));

        assert!(!is_conflict_copy(db, db));
        assert!(!is_conflict_copy(db, "keys.20260811-120000.backup.sqlite3"));
        assert!(!is_conflict_copy(db, "keys.sqlite3.lock"));
        assert!(!is_conflict_copy(db, "other.sqlite3"));
    }

    #[test]
    fn an_absent_file_is_nothing_to_wait_for() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            wait_until_settled(&dir.path().join("nope.sqlite3"), &SyncPolicy::immediate()),
            Settled::NotThere
        );
    }

    #[test]
    fn a_still_file_settles_and_the_wait_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.sqlite3");
        std::fs::write(&path, b"data").unwrap();

        let policy = SyncPolicy {
            quiet: Duration::from_millis(30),
            timeout: Duration::from_millis(400),
            poll: Duration::from_millis(5),
        };
        match wait_until_settled(&path, &policy) {
            Settled::Quiet { waited } => assert!(waited < policy.timeout),
            other => panic!("expected a settled file, got {other:?}"),
        }
    }

    #[test]
    fn a_nonsense_environment_override_is_ignored_rather_than_fatal() {
        // Not the real variable name: the process environment is shared with
        // every other test in this binary.
        assert_eq!(
            env_millis("YKDM_SYNC_QUIET_MS_TEST_ABSENT", Duration::from_secs(7)),
            Duration::from_secs(7)
        );
        // And the shipped defaults are the ones documented in `--help` and in
        // docs/operations.md.
        let default = SyncPolicy::default();
        assert_eq!(default.quiet, Duration::from_millis(1_500));
        assert_eq!(default.timeout, Duration::from_secs(15));
    }

    #[test]
    fn a_refusal_names_the_person_the_computer_and_how_long_ago() {
        let holder = LeaseHolder {
            host: "MAC-RECEPCAO".into(),
            operator: "ana".into(),
            pid: 4242,
            session: Uuid::from_u128(0xA1A1),
            app_version: "0.5.0".into(),
            acquired_at: Utc::now() - chrono::Duration::minutes(20),
            renewed_at: Utc::now() - chrono::Duration::minutes(20),
        };

        let described = holder.to_string();
        for expected in ["ana", "MAC-RECEPCAO", "4242", "holding since"] {
            assert!(described.contains(expected), "{described}");
        }
        // Twenty silent minutes is past the lease, and not this run's lock.
        assert!(holder.is_stale());
        assert!(!holder.is_local());
        assert!(!holder.is_same_host() || holder.host == local_host());
    }

    #[test]
    fn a_local_holder_is_recognised_as_this_run_and_is_not_stale() {
        let mine = LeaseHolder::local("felipe");
        assert!(mine.is_local());
        assert!(mine.is_same_host());
        assert!(!mine.is_stale());
        assert_eq!(mine.app_version, crate::VERSION);
        assert!(
            !local_host().is_empty(),
            "a nameless host makes messages vague"
        );
    }

    #[test]
    fn what_the_wait_achieved_is_reported_in_words() {
        assert!(Settled::NotThere.describe().contains("no file"));
        assert!(
            Settled::Quiet {
                waited: Duration::from_millis(120)
            }
            .describe()
            .contains("120ms")
        );
        let unsettled = Settled::StillChanging {
            waited: Duration::from_millis(15_000),
        };
        assert!(unsettled.is_unsettled());
        assert!(unsettled.describe().contains("sync client"));
    }
}
