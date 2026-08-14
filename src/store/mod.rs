//! Persistence: **one SQLite file**, optionally password-protected, able to
//! live on a network share.
//!
//! Design constraints, in the order they shape the code:
//!
//! 1. **Single file.** Everything — inventory, holders, distributions, bootstrap
//!    runs, templates, audit — lives in one `.sqlite3` file. Copying that file
//!    is a complete backup; SQLite is compiled in (`bundled`), so a workstation
//!    needs nothing installed.
//! 2. **Network share.** SQLite's WAL mode requires shared memory and does not
//!    work over SMB/NFS, so a share-hosted database runs in rollback-journal
//!    mode with `synchronous=FULL` and a generous busy timeout. See
//!    [`Location`] and `features/storage-sqlite-single-file.md`.
//! 3. **Optional password.** With the `encrypted-db` feature the file is
//!    SQLCipher-encrypted and `PRAGMA key` is applied before any other
//!    statement. Without a password the file is a plain SQLite database.
//! 4. **Append-only audit.** The `audit` table rejects `UPDATE` and `DELETE`
//!    through `BEFORE` triggers, so immutability is a database restriction and
//!    not an application convention (NRM §5.3.1).
//! 5. **The share can be connected from here.** "Put it on a network share" was
//!    advice nobody could act on while the application could only open a path that
//!    somebody else had mounted. [`smb`] connects the share itself — as the
//!    signed-in user, as a guest, or as a named account whose password is typed and
//!    dropped — and hands back a local path this module then treats as any other
//!    share. See `features/smb-share-hosting.md`.
//! 6. **Cloud-sync folder.** A database under OneDrive (or Dropbox, or Google
//!    Drive) cannot rely on SQLite's locks at all, because the other workstation
//!    is not sharing a file system with this one — it is receiving whole-file
//!    copies, minutes late. Such a database is made strictly sequential instead,
//!    by the lock-file protocol in [`cloud`]: wait for the download, take
//!    `<database>.lock`, refuse a second workstation by name, and release only
//!    after the upload. See `features/cloud-sync-hosting.md`.

pub mod backup;
pub mod cloud;
pub mod import;
pub mod smb;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

pub use backup::{BackupPolicy, Outcome as BackupOutcome};
pub use cloud::{LeaseError, LeaseHolder, Renewal, Settled, SyncLease, SyncPolicy};

use crate::audit::{self, AuditEntry, GENESIS};
use crate::domain::lifecycle::{
    Dependency, IncidentKind, KeyIncident, Remediation, RemediationKind, RmaCase, Sanitisation,
};
use crate::domain::{AttachedDocument, DocumentKind};
use crate::domain::{
    BootstrapRun, DeliveryMethod, DistributionRecord, Holder, KeyStatus, SerialSource, StepKind,
    StepOutcome, StepStatus, YubiKeyRecord,
};
use crate::template::{BootstrapTemplate, StoredTemplate};
use crate::term::TermTemplate;

/// Current schema version, tracked in `PRAGMA user_version`.
pub const SCHEMA_VERSION: i64 = 6;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(rusqlite::Error),
    #[error(
        "this register is open for reading only — another workstation holds the single-writer \
         lock. Close it there, or take the lock over, before recording anything"
    )]
    ReadOnly,
    #[error(
        "this register is at schema v{found} and this build needs v{supported}; migrating it \
         needs write access, so it cannot be opened read-only"
    )]
    MigrationNeedsWriteAccess { found: i64, supported: i64 },
    #[error("database is encrypted or corrupt — a password is required")]
    PasswordRequired,
    #[error(
        "this build has no encryption support: rebuild with `--features encrypted-db` to use a \
         password-protected database"
    )]
    EncryptionUnavailable,
    #[error("database schema version {found} is newer than this build supports ({supported})")]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("invalid stored value in column `{column}`: {value}")]
    Decode { column: &'static str, value: String },
    #[error("illegal status transition: {from} -> {to}")]
    Transition { from: String, to: String },
    /// A returned key still carrying a previous holder's credentials may not be
    /// reissued (`features/key-lifecycle-and-revocation.md` phase 6).
    ///
    /// The message is the domain's, not this variant's: [`Sanitisation::refusal`]
    /// names the applets and the action, because a refusal that only says *not
    /// sanitised* leaves the operator to guess which applet and how.
    #[error("{reason}")]
    NotSanitised { serial: u32, reason: String },
    #[error("serial {serial} has history and cannot be removed: {reason}")]
    HasHistory { serial: u32, reason: String },
    #[error("{id} version {version} cannot be removed: {reason}")]
    TemplateInUse {
        id: String,
        version: String,
        reason: String,
    },
    #[error("the template was refused: {0}")]
    Template(#[from] crate::template::TemplateError),
    #[error("record not found: {0}")]
    NotFound(String),
    #[error("no database file at {0} — choose an existing one, or create a new one")]
    Missing(PathBuf),
    #[error("a file already exists at {0} — open it instead of creating it")]
    AlreadyExists(PathBuf),
    #[error("serialisation error: {0}")]
    Json(#[from] serde_json::Error),
    /// The single-writer lock on a cloud-hosted database could not be taken.
    ///
    /// Transparent on purpose: the message an operator needs is the one
    /// [`LeaseError`] writes, naming the workstation that has the register.
    #[error(transparent)]
    Lease(#[from] LeaseError),
    #[error("the term template was refused: {0}")]
    Term(#[from] crate::term::TermError),
    #[error("that password is not usable: {0}")]
    WeakPassword(String),
    #[error(
        "the password change failed while {stage}, and the register was left as it was: {reason}"
    )]
    Rekey { stage: &'static str, reason: String },
}

pub type Result<T> = std::result::Result<T, StoreError>;

impl StoreError {
    /// Does this refusal mean "the password was wrong"?
    ///
    /// What the unlock throttle counts (`features/db-password-and-encryption.md`
    /// phase 3), and it must be exactly this one variant. A missing file, an
    /// unreachable share, a lock held by another workstation and a build without
    /// SQLCipher are all failures to open that no amount of retyping fixes, and
    /// counting them would slow an operator down for something that is not a
    /// guess.
    ///
    /// [`Self::PasswordRequired`] is also what a genuinely corrupt file produces
    /// — SQLCipher cannot tell "wrong key" from "not a database", which is why
    /// the message says both. Throttling that case costs nothing: the file is not
    /// going to open on the fourth attempt either.
    pub fn is_wrong_password(&self) -> bool {
        matches!(self, StoreError::PasswordRequired)
    }
}

/// Turn SQLite's own refusal to write a read-only connection into the message an
/// operator can act on.
///
/// Written by hand rather than derived with `#[from]` so this one translation
/// happens on **every** path. That matters: read-only mode is enforced by the
/// connection flag, not by a check in each of the twenty-odd methods that write
/// — the same reasoning as the audit table's triggers, where immutability is a
/// property of the database rather than a promise from the application. A guard
/// per method could be forgotten by the next mutation added; a connection opened
/// without write permission cannot be.
impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        match &error {
            rusqlite::Error::SqliteFailure(e, _)
                if e.code == rusqlite::ErrorCode::ReadOnly
                    || e.code == rusqlite::ErrorCode::CannotOpen =>
            {
                StoreError::ReadOnly
            }
            _ => StoreError::Sqlite(error),
        }
    }
}

/// Where the database file lives, which decides the locking strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// Local disk: WAL, fast and crash-safe.
    LocalDisk,
    /// SMB/NFS/AFP share: rollback journal, full sync, long busy timeout.
    NetworkShare,
    /// A synchronising folder (OneDrive, Dropbox, Google Drive, iCloud).
    ///
    /// The share's pragmas, **plus** the single-writer lock file from [`cloud`]:
    /// on a share the two workstations at least share a lock manager, and here
    /// they do not, so sequencing has to be arranged outside the database.
    CloudSync,
}

/// Path fragments that mean a cloud-sync folder.
///
/// A synchronising folder is the **worst** place for a SQLite database: the sync
/// client copies the file underneath the writer, and a conflict produces a second
/// file rather than a merge — so the resolution of a clash is "two divergent
/// registers of who holds which security token". WAL makes it worse again, because
/// the `-wal` and `-shm` sidecars are synchronised independently of the database.
const CLOUD_SYNC_MARKERS: [&str; 8] = [
    "/CloudStorage/", // macOS File Provider: OneDrive, Google Drive, Box, Dropbox
    "OneDrive",
    "Dropbox",
    "Google Drive",
    "GoogleDrive",
    "iCloud Drive",
    "Mobile Documents", // iCloud's on-disk name
    "pCloud",
];

/// Does this path look like it is inside a cloud-sync folder?
///
/// Case-insensitive and deliberately eager: a false positive costs a slower journal
/// mode and a warning, while a false negative risks the dataset.
pub fn looks_like_cloud_sync(path: &Path) -> bool {
    let raw = path.to_string_lossy().to_ascii_lowercase();
    CLOUD_SYNC_MARKERS
        .iter()
        .any(|marker| raw.contains(&marker.to_ascii_lowercase()))
}

impl Location {
    /// Heuristic guess from the path. Always overridable in Settings, because
    /// no heuristic can be right everywhere.
    pub fn detect(path: &Path) -> Self {
        let raw = path.to_string_lossy();
        let looks_remote = raw.starts_with("\\\\")            // Windows UNC
            || raw.starts_with("//")                          // POSIX UNC-ish
            || raw.starts_with("/Volumes/")                   // macOS mounted share
            || raw.starts_with("/mnt/")
            || raw.starts_with("/net/")
            || raw.starts_with("/media/");

        // A cloud-sync folder gets its own classification, ahead of the share
        // check: it needs the share's pragmas (WAL's shared-memory sidecars cannot
        // survive a sync client) *and* the lock-file protocol, because the other
        // workstation is not sharing a file system with this one.
        if looks_like_cloud_sync(path) {
            Location::CloudSync
        } else if looks_remote {
            Location::NetworkShare
        } else {
            Location::LocalDisk
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Location::LocalDisk => "local disk (WAL)",
            Location::NetworkShare => "network share (rollback journal)",
            Location::CloudSync => "cloud-sync folder (rollback journal, single-writer lock)",
        }
    }

    /// Does a database here need the [`cloud`] lock file?
    pub fn requires_lease(&self) -> bool {
        matches!(self, Location::CloudSync)
    }
}

/// How to open the database.
#[derive(Debug, Clone)]
pub struct StoreConfig {
    pub path: PathBuf,
    /// `None` = unencrypted file.
    pub password: Option<String>,
    pub location: Location,
    /// Take the single-writer lock when the location needs one
    /// ([`Location::requires_lease`]).
    ///
    /// On by default, and off only for a deliberate look at a file nobody is
    /// working in — a diagnosis, a test, a copy being inspected.
    pub lease: bool,
    /// Break a lock whose holder has gone quiet.
    ///
    /// Never on by default: only the operator can know that the other workstation
    /// is switched off rather than mid-hand-over, so this is set by an explicit
    /// "take over" action and audited when it happens.
    pub take_over_stale_lease: bool,
    /// Recorded in the lock file so a refusal can name a person, not a pid.
    pub operator: String,
    /// How long to wait for the sync client, before opening and after closing.
    pub sync: SyncPolicy,
    /// How often to copy the register, and how many copies to keep.
    pub backup: BackupPolicy,
    /// Where the segregated append-only copy of the audit trail is written.
    ///
    /// `None` means no mirror, which is the default and is not an error: whether
    /// segregated audit storage is *required* here is an ESI decision that the
    /// roadmap records as open. The mechanism exists so that the answer "yes"
    /// costs a settings change rather than a release.
    pub audit_mirror: Option<PathBuf>,
}

impl StoreConfig {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let location = Location::detect(&path);
        Self {
            path,
            password: None,
            location,
            lease: true,
            take_over_stale_lease: false,
            operator: cloud::local_operator(),
            sync: SyncPolicy::from_env(),
            backup: BackupPolicy::default(),
            audit_mirror: None,
        }
    }

    pub fn with_password(mut self, password: Option<String>) -> Self {
        self.password = password.filter(|p| !p.is_empty());
        self
    }

    /// State the location instead of letting [`Location::detect`] guess it.
    ///
    /// [`Location::detect`] reads a *path*, which is all there is to go on when an
    /// operator typed one. A caller that **knows** — [`smb::ShareConnection`] has
    /// just connected an SMB share, so the file is on a network filesystem whatever
    /// mount point the operating system happened to choose — should say so, because
    /// the journal mode has to follow the fact and not the spelling.
    pub fn with_location(mut self, location: Location) -> Self {
        self.location = location;
        self
    }

    /// Record who is opening it, for the lock file.
    pub fn with_operator(mut self, operator: &str) -> Self {
        let operator = operator.trim();
        if !operator.is_empty() {
            self.operator = operator.to_owned();
        }
        self
    }

    /// How patiently to wait for the sync client.
    pub fn with_sync_policy(mut self, sync: SyncPolicy) -> Self {
        self.sync = sync;
        self
    }

    /// How often to copy the register, and how many copies to keep.
    pub fn with_backup_policy(mut self, backup: BackupPolicy) -> Self {
        self.backup = backup;
        self
    }

    /// Mirror every audit entry to a second, append-only location.
    pub fn with_audit_mirror(mut self, path: Option<PathBuf>) -> Self {
        self.audit_mirror = path;
        self
    }

    /// Open without taking the single-writer lock.
    pub fn without_lease(mut self) -> Self {
        self.lease = false;
        self
    }

    /// Break an abandoned lock. An explicit operator decision, never a default.
    pub fn taking_over_stale_lease(mut self) -> Self {
        self.take_over_stale_lease = true;
        self
    }

    /// Will this open take the lock file?
    pub fn requires_lease(&self) -> bool {
        self.lease && self.location.requires_lease()
    }
}

/// Open handle to the single-file database.
pub struct Store {
    conn: Connection,
    path: PathBuf,
    location: Location,
    encrypted: bool,
    /// Held for the whole session when the database is in a sync folder.
    ///
    /// The GUI keeps one connection open from the moment a database is chosen, so
    /// the lock is held for the session rather than per write: "strictly
    /// sequential" here means one *workstation* at a time, which is what the sync
    /// client's whole-file, minutes-late copying can support.
    lease: Option<SyncLease>,
    sync: SyncPolicy,
    backup: BackupPolicy,
    /// Opened with `SQLITE_OPEN_READ_ONLY`; nothing can be recorded.
    read_only: bool,
    /// The segregated append-only copy of the trail, when one is configured.
    ///
    /// `RefCell` because appending advances the log's chain head, and every
    /// `Store` mutation takes `&self` — the alternative would be `&mut self` on
    /// every method that writes an audit entry, which is all of them.
    mirror: Option<std::cell::RefCell<crate::audit::AuditLog>>,
    /// Whether the chain verified when this register was opened.
    chain_status: ChainStatus,
}

/// The result of verifying the audit chain when the register was opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainStatus {
    /// Verified, with the number of entries checked.
    Verified { entries: usize },
    /// The chain is broken. Carries the reason, which names the entry.
    Broken { reason: String },
    /// Not checked — a read-only look, or a register too large to verify at open.
    NotChecked,
}

impl ChainStatus {
    pub fn is_broken(&self) -> bool {
        matches!(self, ChainStatus::Broken { .. })
    }
}

/// What [`Store::import_template`] did.
///
/// Two outcomes rather than one, because "stored as version 4" and "you already
/// have this" want different sentences on screen — and because the second is the
/// common case when a file goes round a unit by mail and gets imported twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateImport {
    /// Stored under a version this database assigned. `previous` is the newest
    /// version of that id before the import, or `None` for a new template.
    Stored {
        /// Boxed because it is much the larger of the two variants — a whole
        /// procedure against a version string — and every caller matches on the
        /// enum by reference anyway.
        template: Box<BootstrapTemplate>,
        previous: Option<String>,
    },
    /// A version of this id already describes exactly this procedure, with the
    /// same signature. Nothing was written.
    AlreadyPresent {
        version: String,
        /// It is on record but withdrawn from the wizard — worth saying, or the
        /// operator concludes the import worked and then cannot find it.
        retired: bool,
    },
}

impl TemplateImport {
    /// The sentence for the status line.
    pub fn describe(&self) -> String {
        match self {
            TemplateImport::Stored {
                template,
                previous: Some(previous),
            } => format!(
                "`{}` imported as version {} ({} step(s)) — version {previous} stays on record",
                template.id,
                template.version,
                template.steps.len()
            ),
            TemplateImport::Stored {
                template,
                previous: None,
            } => format!(
                "`{}` imported as version {} ({} step(s)) — a template this register did not have",
                template.id,
                template.version,
                template.steps.len()
            ),
            TemplateImport::AlreadyPresent {
                version,
                retired: false,
            } => format!("already on record as version {version} — nothing to import"),
            TemplateImport::AlreadyPresent {
                version,
                retired: true,
            } => format!(
                "already on record as version {version}, which is retired — nothing was imported. \
                 Reinstate that version to offer it in the wizard again"
            ),
        }
    }
}

impl Store {
    /// `$YKDM_DB`, else the per-user data directory.
    pub fn default_path() -> PathBuf {
        if let Ok(explicit) = std::env::var("YKDM_DB")
            && !explicit.trim().is_empty()
        {
            return PathBuf::from(explicit);
        }
        crate::paths::data_dir().join(crate::paths::DEFAULT_DATABASE_NAME)
    }

    /// Open an existing database, or create one if the file is not there.
    ///
    /// Used for the default path on first launch, and by tests. An operator
    /// choosing a path should use [`Store::open_existing`] or
    /// [`Store::create_new`] instead, so a typo cannot silently produce a second,
    /// empty database that looks like data loss.
    pub fn open(config: &StoreConfig) -> Result<Self> {
        if let Some(parent) = config.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // The lock comes first, and so does waiting for the sync client: both
        // decide whether this process may open the file at all.
        let lease = Self::acquire_lease(config)?;
        let conn = Connection::open(&config.path)?;
        Self::finish_open(conn, config.path.clone(), config, lease)
    }

    /// Take the single-writer lock, when the location calls for one.
    ///
    /// Returns `Ok(None)` for a local file or a real share, where SQLite's own
    /// locking is the right mechanism and a lock file would only be a second,
    /// weaker one.
    fn acquire_lease(config: &StoreConfig) -> Result<Option<SyncLease>> {
        if !config.requires_lease() {
            return Ok(None);
        }
        let lease = if config.take_over_stale_lease {
            SyncLease::take_over(&config.path, &config.operator, &config.sync)?
        } else {
            SyncLease::acquire(&config.path, &config.operator, &config.sync)?
        };
        Ok(Some(lease))
    }

    /// Open a database that must already exist.
    pub fn open_existing(config: &StoreConfig) -> Result<Self> {
        if !config.path.is_file() {
            return Err(StoreError::Missing(config.path.clone()));
        }
        Self::open(config)
    }

    /// Create a database, refusing to touch an existing file.
    ///
    /// The refusal matters: "create" must never open somebody else's dataset,
    /// and must never be a way to append to a file the operator believed was new.
    pub fn create_new(config: &StoreConfig) -> Result<Self> {
        if config.path.exists() {
            return Err(StoreError::AlreadyExists(config.path.clone()));
        }
        // The policy belongs here as well as in [`Self::change_password`], and for
        // the same reason it is checked before anything moves there: this is the
        // other moment a password is *chosen*. Enforcing it only in the meter
        // would leave the floor as advice the GUI happens to give — every other
        // caller (a test, a future CLI) could create a register keyed on `a`, and
        // the file would then be un-rekeyable without also changing its password.
        //
        // Only when the build can encrypt at all: without the feature the
        // password is refused a moment later by `apply_key` with an error that
        // names the flag to rebuild with, which is the more useful answer than
        // grading a password this build cannot use.
        if cfg!(feature = "encrypted-db")
            && let Some(password) = &config.password
        {
            let assessment = crate::password::assess(password);
            if !assessment.is_acceptable() {
                return Err(StoreError::WeakPassword(assessment.summary()));
            }
        }
        let store = Self::open(config)?;
        store.seed_builtin_templates()?;
        Ok(store)
    }

    /// Open a register for **reading only**, taking no lock.
    ///
    /// The second half of the cloud-sync answer: until now a workstation that
    /// found the lock held could do nothing at all, and "who has serial 20423633?"
    /// is a question worth answering while somebody else is mid-hand-over. This
    /// opens the file with `SQLITE_OPEN_READ_ONLY`, so the refusal to write comes
    /// from SQLite rather than from a check this code could forget to make, and
    /// takes no lease — there is nothing to sequence when nothing is written.
    ///
    /// Two things are deliberately refused rather than worked around:
    ///
    /// * **A migration.** Bringing an old file up to the current schema is a
    ///   write. A read-only session says so and names the version, instead of
    ///   opening a file whose rows it would misread.
    /// * **The journal-mode pragma.** It writes too, so the pragmas are left as
    ///   they are; a reader does not need them, having no journal to write.
    pub fn open_read_only(config: &StoreConfig) -> Result<Self> {
        use rusqlite::OpenFlags;

        if !config.path.is_file() {
            return Err(StoreError::Missing(config.path.clone()));
        }

        let conn = Connection::open_with_flags(
            &config.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;

        if let Some(password) = &config.password {
            apply_key(&conn, password)?;
        }
        match conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
            row.get::<_, i64>(0)
        }) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _)) if is_encryption_error(e.extended_code) => {
                return Err(StoreError::PasswordRequired);
            }
            Err(e) => return Err(e.into()),
        }

        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        if version < SCHEMA_VERSION {
            return Err(StoreError::MigrationNeedsWriteAccess {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }

        conn.pragma_update(None, "foreign_keys", "ON")?;

        tracing::info!(
            event = "db.opened.read_only",
            path = %config.path.display(),
            detail = "no single-writer lock taken; nothing can be recorded from this session"
        );

        Ok(Self {
            conn,
            path: config.path.clone(),
            location: config.location,
            encrypted: config.password.is_some(),
            lease: None,
            sync: config.sync,
            backup: BackupPolicy::disabled(),
            read_only: true,
            // A reader writes no entries, so it mirrors none. It could still
            // *compare* the two, but the mirror is the writing session's
            // obligation and opening it here would be a second handle on a file
            // another workstation is appending to.
            mirror: None,
            chain_status: ChainStatus::NotChecked,
        })
    }

    /// In-memory database, for tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let config = StoreConfig::new(":memory:").without_lease();
        Self::finish_open(conn, config.path.clone(), &config, None)
    }

    fn finish_open(
        conn: Connection,
        path: PathBuf,
        config: &StoreConfig,
        lease: Option<SyncLease>,
    ) -> Result<Self> {
        // The key must be applied before anything else touches the file.
        if let Some(password) = &config.password {
            apply_key(&conn, password)?;
        }

        // First real read: this is where a wrong or missing password surfaces.
        match conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
            row.get::<_, i64>(0)
        }) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _)) if is_encryption_error(e.extended_code) => {
                return Err(StoreError::PasswordRequired);
            }
            Err(e) => return Err(StoreError::Sqlite(e)),
        }

        let mut store = Self {
            conn,
            path,
            location: config.location,
            encrypted: config.password.is_some(),
            lease,
            sync: config.sync,
            backup: config.backup,
            read_only: false,
            mirror: open_mirror(config.audit_mirror.as_deref()),
            chain_status: ChainStatus::NotChecked,
        };
        store.apply_pragmas()?;
        store.migrate()?;
        store.backup_on_open();
        store.chain_status = store.verify_chain_on_open();

        if store.on_cloud_sync() {
            tracing::warn!(
                event = "db.cloud_sync.detected",
                path = %store.path.display(),
                locked = store.lease.is_some(),
                detail = "a sync client can copy the file mid-write and resolves a clash by \
                          keeping both copies; the single-writer lock sequences the operators \
                          that cooperate, but a network share or a local file with a scheduled \
                          backup is still the better place"
            );
        }

        Ok(store)
    }

    fn apply_pragmas(&self) -> Result<()> {
        self.conn.pragma_update(None, "foreign_keys", "ON")?;
        match self.location {
            Location::LocalDisk => {
                // WAL needs mmap'd shared memory — local files only.
                self.set_journal_mode("WAL")?;
                self.conn.pragma_update(None, "synchronous", "NORMAL")?;
                self.conn.busy_timeout(std::time::Duration::from_secs(5))?;
            }
            // A sync folder gets the share's pragmas for the same reason plus one
            // more: rollback journal leaves the database file complete on disk
            // between commits, which is the only state a sync client can usefully
            // upload. WAL would hand it three files that must arrive in step.
            Location::NetworkShare | Location::CloudSync => {
                // Rollback journal is the only mode that survives SMB/NFS.
                self.set_journal_mode("DELETE")?;
                self.conn.pragma_update(None, "synchronous", "FULL")?;
                self.conn.busy_timeout(std::time::Duration::from_secs(20))?;
            }
        }
        Ok(())
    }

    /// `PRAGMA journal_mode` returns the resulting mode as a row, so it cannot
    /// go through `pragma_update`.
    fn set_journal_mode(&self, mode: &str) -> Result<()> {
        let applied: String =
            self.conn
                .query_row(&format!("PRAGMA journal_mode={mode}"), [], |row| row.get(0))?;
        if !applied.eq_ignore_ascii_case(mode) {
            tracing::warn!(
                event = "db.journal_mode.unexpected",
                requested = mode,
                applied = applied
            );
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn location(&self) -> Location {
        self.location
    }

    pub fn is_encrypted(&self) -> bool {
        self.encrypted
    }

    /// True when this session can read the register but not record anything.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// One line for the GUI status bar.
    pub fn describe(&self) -> String {
        format!(
            "{} — {}{}{}",
            self.path.display(),
            self.location.label(),
            if self.encrypted {
                ", password-protected"
            } else {
                ", no password"
            },
            match &self.lease {
                Some(_) => " — single-writer lock held",
                // Read-only is stated before the warning, because it is the
                // reason there is no lock and it is what the operator needs to
                // know before they try to record a hand-over.
                None if self.read_only => " — READ ONLY, nothing can be recorded",
                None if self.on_cloud_sync() =>
                    " — WARNING: cloud-sync folder, no single-writer lock",
                None => "",
            }
        )
    }

    /// True when the database file sits in a synchronising folder.
    ///
    /// Surfaced rather than silently tolerated: a sync client resolves a clash by
    /// keeping both files, so two operators can end up with divergent registers and
    /// no merge. The lock file ([`cloud`]) sequences the operators who cooperate;
    /// the recommendation is still a real network share, or a local file with a
    /// scheduled copy.
    pub fn on_cloud_sync(&self) -> bool {
        self.location == Location::CloudSync || looks_like_cloud_sync(&self.path)
    }

    /// The lock this session holds, when the location called for one.
    pub fn lease(&self) -> Option<&SyncLease> {
        self.lease.as_ref()
    }

    /// Sync conflict copies found next to the database when it was opened.
    ///
    /// Not re-scanned per call: this is what the folder looked like at the moment
    /// the register was opened, which is the fact a report wants.
    pub fn conflict_copies(&self) -> &[PathBuf] {
        match &self.lease {
            Some(lease) => &lease.report().conflicts,
            None => &[],
        }
    }

    /// Keep the lock file fresh, so another workstation can tell this session
    /// apart from an abandoned one. Cheap, and safe to call every frame.
    ///
    /// [`Renewal::Lost`] means another workstation has taken the lock: the caller
    /// must stop writing and close the database.
    pub fn renew_lease(&mut self) -> Result<Renewal> {
        match &mut self.lease {
            Some(lease) => Ok(lease.renew_if_due()?),
            None => Ok(Renewal::NotDue),
        }
    }

    /// Close the connection, wait for the sync client, then release the lock.
    ///
    /// The last two steps are the second half of the cloud-sync protocol, and
    /// they only happen on this path — dropping a [`Store`] releases the lock
    /// without waiting for the upload, which is a backstop and not the intended
    /// exit. Returns what the wait achieved, when there was one to do.
    ///
    /// Infallible on purpose: a failure to close the connection is logged loudly,
    /// because the one thing that must still happen is releasing the lock.
    pub fn close(self) -> Option<Settled> {
        let Self {
            conn,
            path,
            lease,
            sync,
            ..
        } = self;

        if let Err((_, e)) = conn.close() {
            tracing::error!(event = "db.close.failed", path = %path.display(), reason = %e);
        }
        lease.map(|lease| lease.release(&sync))
    }

    // ---------------------------------------------------------------- schema

    fn migrate(&self) -> Result<()> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;

        if version > SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        if version == SCHEMA_VERSION {
            return Ok(());
        }

        if version < 1 {
            self.conn.execute_batch(SCHEMA_V1)?;
        }
        if version < 2 {
            self.conn.execute_batch(MIGRATE_V2)?;
        }
        if version < 3 {
            self.conn.execute_batch(MIGRATE_V3)?;
        }
        if version < 4 {
            self.conn.execute_batch(MIGRATE_V4)?;
        }
        if version < 5 {
            // Three steps, in this order: make the table, move the blob into it,
            // then drop the blob. A backfill that ran after the drop would have
            // nothing to read, and one that ran before the create would have
            // nowhere to write.
            self.conn.execute_batch(MIGRATE_V5_CREATE)?;
            self.backfill_run_steps()?;
            self.conn.execute_batch(MIGRATE_V5_DROP)?;
        }
        if version < 6 {
            self.conn.execute_batch(MIGRATE_V6)?;
        }

        self.conn
            .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tracing::info!(event = "db.migrated", from = version, to = SCHEMA_VERSION);
        Ok(())
    }

    /// Turn every `bootstrap_runs.steps` JSON blob into rows (schema v5).
    ///
    /// Done in Rust so the enum mapping is [`StepKind::slug`] itself rather than
    /// a copy of it written in SQL. A blob that cannot be parsed is **not** fatal:
    /// it is logged and skipped, leaving that run with no step rows. Refusing to
    /// open the register because one historical run has an unreadable step list
    /// would trade a partial record for no record at all — and the run row, which
    /// carries the template, the operator and the outcome, survives either way.
    fn backfill_run_steps(&self) -> Result<()> {
        let mut stmt = self.conn.prepare("SELECT id, steps FROM bootstrap_runs")?;
        let blobs = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut moved = 0usize;
        for (run_id, blob) in blobs {
            let steps: Vec<StepOutcome> = match serde_json::from_str(&blob) {
                Ok(steps) => steps,
                Err(e) => {
                    tracing::error!(
                        event = "db.migrate.step_blob_unreadable",
                        run = %run_id,
                        reason = %e,
                        detail = "the run keeps its record; its step list could not be recovered"
                    );
                    continue;
                }
            };
            self.write_run_steps(&run_id, &steps)?;
            moved += steps.len();
        }
        tracing::info!(event = "db.migrate.run_steps", steps = moved);
        Ok(())
    }

    /// Replace a run's step rows. The delete makes it idempotent, which is what
    /// lets [`Store::insert_run`] stay an upsert.
    fn write_run_steps(&self, run_id: &str, steps: &[StepOutcome]) -> Result<()> {
        self.conn.execute(
            "DELETE FROM bootstrap_run_steps WHERE run_id = ?1",
            params![run_id],
        )?;
        let mut insert = self.conn.prepare(
            "INSERT INTO bootstrap_run_steps
                 (run_id, position, step_id, kind, status, started_at, finished_at, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for (position, step) in steps.iter().enumerate() {
            insert.execute(params![
                run_id,
                position as i64,
                step.step_id,
                step_kind_str(step.kind),
                step_status_str(step.status),
                step.started_at.map(|at| at.to_rfc3339()),
                step.finished_at.map(|at| at.to_rfc3339()),
                step.detail,
            ])?;
        }
        Ok(())
    }

    /// Change the database password, or set one on a plain file.
    ///
    /// `features/db-password-and-encryption.md` phases 2 and 5 — one operation,
    /// because they are the same one: **export the whole database into a new file
    /// under a different key, prove the copy is good, and only then swap.**
    ///
    /// ## Why not `PRAGMA rekey`
    ///
    /// SQLCipher can re-encrypt in place, and on a share that is the riskiest
    /// operation this tool could perform: it rewrites every page of a file that a
    /// sync client may be copying and another workstation may be about to open,
    /// with no intermediate state that is a valid database. An interruption
    /// half-way leaves a file that is neither the old key nor the new one. The
    /// spec calls this out and this method is the alternative it names.
    ///
    /// ## The order, and why each step is where it is
    ///
    /// 1. **Check the new password** against the policy, before anything moves.
    /// 2. **Take a backup.** The one operation that rewrites the whole register
    ///    is the one that most needs a copy of what it started from.
    /// 3. **Audit the change into the *source*.** It has to be written before the
    ///    export, so the export carries it: an entry written afterwards would go
    ///    into a file that is about to be replaced.
    /// 4. **Export** via `sqlcipher_export` into a new file opened with the new
    ///    key, then carry `user_version` across by hand — `sqlcipher_export`
    ///    copies schema and rows, not pragmas, and a database that arrived with
    ///    `user_version = 0` would be migrated from scratch on the next open.
    /// 5. **Verify the copy**: it opens under the new password, its integrity
    ///    check passes, its schema version matches, and its audit chain verifies.
    ///    A copy that fails any of these is deleted and the original is untouched.
    /// 6. **Swap**, keeping the original under a `.replaced` name until the new
    ///    file is in place.
    ///
    /// Consumes the `Store`, because after the swap this handle points at a file
    /// that is no longer the register. The caller reopens at the returned path
    /// with the new password.
    pub fn change_password(
        self,
        operator: &str,
        new_password: Option<&str>,
    ) -> Result<std::path::PathBuf> {
        if !cfg!(feature = "encrypted-db") {
            return Err(StoreError::EncryptionUnavailable);
        }
        if self.read_only {
            return Err(StoreError::ReadOnly);
        }

        // 1. Policy first: refusing after the backup and the export would be
        //    doing all the work to arrive at "no".
        if let Some(password) = new_password {
            let assessment = crate::password::assess(password);
            if !assessment.is_acceptable() {
                return Err(StoreError::WeakPassword(assessment.summary()));
            }
        }

        let path = self.path.clone();
        let was_encrypted = self.encrypted;
        let schema: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;

        // 2. A copy of what we started from.
        let backup = self.backup_now(Utc::now())?;

        // 3. Audited into the source, so the export carries the entry.
        let detail = format!(
            "from={} to={} schema={schema}",
            if was_encrypted { "encrypted" } else { "plain" },
            if new_password.is_some() {
                "encrypted"
            } else {
                "plain"
            }
        );
        let event = if was_encrypted {
            "db.password.changed"
        } else {
            "db.encrypted"
        };
        self.append_audit(operator, event, &path.display().to_string(), &detail)?;

        // 4. Export into a new file under the new key.
        let target = path.with_file_name(format!(
            "{}.rekey-in-progress.sqlite3",
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "database".into())
        ));
        let _ = std::fs::remove_file(&target);

        if let Err(e) = export_to(&self.conn, &target, new_password, schema) {
            let _ = std::fs::remove_file(&target);
            return Err(e);
        }

        // 5. Prove the copy before trusting it. Done on a *separate* connection,
        //    so what is checked is the file as it will be reopened rather than
        //    the handle that wrote it.
        if let Err(e) = verify_rekeyed(&target, new_password, schema) {
            let _ = std::fs::remove_file(&target);
            return Err(e);
        }

        // 6. Swap. The connection closes first — on Windows a file cannot be
        //    replaced while it is open, and everywhere else a stale handle
        //    writing into a replaced file is a way to lose the swap.
        let sync = self.sync;
        let lease = self.lease;
        if let Err((_, e)) = self.conn.close() {
            let _ = std::fs::remove_file(&target);
            return Err(StoreError::Sqlite(e));
        }

        let replaced = path.with_extension("replaced");
        std::fs::rename(&path, &replaced).map_err(|e| StoreError::Rekey {
            stage: "moving the original aside",
            reason: e.to_string(),
        })?;
        if let Err(e) = std::fs::rename(&target, &path) {
            // Put the original back. Failing here would leave no register at
            // all, which is worse than a failed password change.
            let _ = std::fs::rename(&replaced, &path);
            return Err(StoreError::Rekey {
                stage: "moving the new file into place",
                reason: e.to_string(),
            });
        }
        let _ = std::fs::remove_file(&replaced);

        // The lock is released last, after the file it guards is the new one.
        if let Some(lease) = lease {
            lease.release(&sync);
        }

        tracing::info!(
            event = "db.rekeyed",
            path = %path.display(),
            encrypted = new_password.is_some(),
            backup = ?backup.taken,
        );
        Ok(path)
    }

    /// `PRAGMA integrity_check` — offered in Settings, and worth running after
    /// any share hiccup.
    pub fn integrity_check(&self) -> Result<String> {
        Ok(self
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))?)
    }

    /// Consistent single-file backup, even while other operators are connected.
    pub fn backup_to(&self, target: &Path) -> Result<()> {
        self.conn
            .execute("VACUUM INTO ?1", params![target.to_string_lossy()])?;
        Ok(())
    }

    /// The backup schedule this session is running under.
    pub fn backup_policy(&self) -> BackupPolicy {
        self.backup
    }

    /// Take a backup if the schedule says one is due, and prune the old ones.
    ///
    /// `now` is a parameter so the decision is testable without waiting a day;
    /// callers in the application pass `Utc::now()`.
    pub fn backup_if_due(&self, now: DateTime<Utc>) -> Result<BackupOutcome> {
        self.run_backup(now, false)
    }

    /// Take one whatever the schedule says.
    pub fn backup_now(&self, now: DateTime<Utc>) -> Result<BackupOutcome> {
        self.run_backup(now, true)
    }

    fn run_backup(&self, now: DateTime<Utc>, forced: bool) -> Result<BackupOutcome> {
        // An in-memory database has nothing to copy and no directory to copy it
        // into; asking is a programming error rather than an operator one, so it
        // is a quiet no-op instead of a refusal.
        if self.path.as_os_str() == ":memory:" {
            return Ok(BackupOutcome::default());
        }

        let existing = backup::existing(&self.path);
        let plan = backup::Plan::decide(&self.path, &existing, &self.backup, now, forced);

        let mut outcome = BackupOutcome::default();
        if let Some(target) = &plan.take {
            // The stamp has second resolution, so two opens inside one second
            // want the same filename — and `VACUUM INTO` refuses to write over
            // an existing file. A copy of this second is already on disk, which
            // is what was being asked for, so this is "done" rather than an
            // error. Reported at debug because it is normal on a quick restart.
            if target.exists() {
                tracing::debug!(
                    event = "db.backup.already_taken",
                    path = %target.display()
                );
            } else {
                self.backup_to(target)?;
                outcome.taken = Some(target.clone());
            }
        }

        // Pruning failures are logged, not propagated: a backup that was taken
        // is worth more than a tidy directory, and the alternative is reporting
        // "backup failed" for a copy that is sitting on disk.
        for stale in plan.prune {
            match std::fs::remove_file(&stale) {
                Ok(()) => outcome.pruned.push(stale),
                Err(e) => tracing::warn!(
                    event = "db.backup.prune_failed",
                    path = %stale.display(),
                    reason = %e
                ),
            }
        }
        Ok(outcome)
    }

    /// The snapshot a cloud-hosted register gets before this session can write.
    ///
    /// Taken at open rather than at the first write, which is earlier than
    /// `features/cloud-sync-hosting.md` phase 7 asks for and strictly safer: by
    /// the time a write happens the pre-write state is already on disk, and
    /// there is no bookkeeping to get wrong about whether this session has
    /// written yet.
    ///
    /// Only for [`Location::CloudSync`], because it answers a failure that only
    /// happens there: a sync client resolved a clash by keeping both copies, and
    /// whichever side this workstation is about to overwrite has no other copy.
    /// A failure is logged, never fatal — refusing to open the register because
    /// a backup could not be written would be a worse outcome than the risk it
    /// guards.
    fn backup_on_open(&self) {
        if self.location != Location::CloudSync || !self.backup.before_first_write {
            return;
        }
        match self.backup_now(Utc::now()) {
            Ok(outcome) if outcome.did_nothing() => {}
            Ok(outcome) => tracing::info!(
                event = "db.backup.before_first_write",
                detail = outcome.detail()
            ),
            Err(e) => tracing::error!(
                event = "db.backup.before_first_write.failed",
                path = %self.path.display(),
                reason = %e,
                detail = "the register opened, but this session's writes are not protected by a \
                          pre-write copy"
            ),
        }
    }

    // ------------------------------------------------------------------ keys

    /// Insert or refresh a key by serial (the natural key).
    pub fn upsert_key(&self, record: &YubiKeyRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO keys (id, serial, model, firmware, form_factor, fips, applications,
                               status, batch, notes, serial_source, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(serial) DO UPDATE SET
                 model = excluded.model,
                 firmware = excluded.firmware,
                 form_factor = excluded.form_factor,
                 fips = excluded.fips,
                 applications = excluded.applications,
                 batch = excluded.batch,
                 notes = excluded.notes,
                 -- Provenance only ever improves: a device read upgrades a
                 -- scanned or typed serial, and a later scan never downgrades a
                 -- serial we have verified against the hardware.
                 serial_source = CASE
                     WHEN excluded.serial_source = 'device' THEN 'device'
                     ELSE keys.serial_source
                 END,
                 updated_at = excluded.updated_at",
            params![
                record.id.to_string(),
                record.serial,
                record.model,
                record.firmware,
                record.form_factor,
                record.fips as i64,
                serde_json::to_string(&record.applications)?,
                key_status_str(record.status),
                record.batch,
                record.notes,
                serial_source_str(record.serial_source),
                record.created_at.to_rfc3339(),
                record.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn keys(&self) -> Result<Vec<YubiKeyRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, serial, model, firmware, form_factor, fips, applications, status,
                    batch, notes, created_at, updated_at, serial_source
             FROM keys ORDER BY serial",
        )?;
        let rows = stmt.query_map([], row_to_key)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }

    pub fn key_by_serial(&self, serial: u32) -> Result<Option<YubiKeyRecord>> {
        let found = self
            .conn
            .query_row(
                "SELECT id, serial, model, firmware, form_factor, fips, applications, status,
                        batch, notes, created_at, updated_at, serial_source
                 FROM keys WHERE serial = ?1",
                params![serial],
                row_to_key,
            )
            .optional()?;
        match found {
            Some(res) => Ok(Some(res?)),
            None => Ok(None),
        }
    }

    /// Move a key through its lifecycle, refusing transitions the domain
    /// forbids instead of silently applying them.
    ///
    /// Two refusals, not one. The domain's transition table is the first
    /// ([`KeyStatus::can_transition_to`]), and the second is the **sanitised
    /// gate** (`features/key-lifecycle-and-revocation.md` phase 6): a key that
    /// still carries what a bootstrap put on it may not return to stock or be
    /// prepared for somebody else. That rule cannot live in the transition table,
    /// because it is not a fact about the two statuses — it is a fact about this
    /// key's runs and resets, which is why it is enforced here where the record is
    /// readable. See [`Self::reissue_refusal`] for exactly which moves it covers.
    pub fn set_key_status(&self, serial: u32, next: KeyStatus) -> Result<()> {
        let current = self
            .key_by_serial(serial)?
            .ok_or_else(|| StoreError::NotFound(format!("serial {serial}")))?;
        if current.status != next && !current.status.can_transition_to(next) {
            return Err(StoreError::Transition {
                from: current.status.label().into(),
                to: next.label().into(),
            });
        }
        if let Some(refusal) = self.reissue_refusal(serial, current.status, next)? {
            return Err(refusal);
        }
        self.conn.execute(
            "UPDATE keys SET status = ?1, updated_at = ?2 WHERE serial = ?3",
            params![key_status_str(next), Utc::now().to_rfc3339(), serial],
        )?;
        Ok(())
    }

    /// Is this move a **reissue** of a key that has not been sanitised?
    ///
    /// `None` when the move is allowed. Which moves are covered, and why each:
    ///
    /// * **into `In stock`** — from anywhere. A key in stock is a key the next
    ///   procedure may pick up, and the previous holder's certificate is still in
    ///   slot 9c until somebody resets it.
    /// * **into `Bootstrapped`, except from `In stock`** — because that one
    ///   exception *is* the bootstrap: the run that has just written the
    ///   credentials is what moves the key, and gating it would refuse every key
    ///   the tool has just prepared. Every other route into `Bootstrapped` —
    ///   `Returned → Bootstrapped` is the one that exists — is a second procedure
    ///   on a key that has already been through one.
    ///
    /// Not covered: `Lost`, `Returned` and `Retired`. A key coming back, going
    /// missing or leaving service is not being handed to anybody, and refusing to
    /// record what has happened to a key would be the register declining to hold a
    /// fact — the opposite of the point.
    fn reissue_refusal(
        &self,
        serial: u32,
        current: KeyStatus,
        next: KeyStatus,
    ) -> Result<Option<StoreError>> {
        if current == next {
            return Ok(None);
        }
        let is_reissue = match next {
            KeyStatus::InStock => true,
            KeyStatus::Bootstrapped => current != KeyStatus::InStock,
            _ => false,
        };
        if !is_reissue {
            return Ok(None);
        }
        let state = self.sanitisation_for(serial)?;
        if state.is_clear() {
            return Ok(None);
        }
        Ok(Some(StoreError::NotSanitised {
            serial,
            reason: state.refusal(serial),
        }))
    }

    /// Replace a key's observation, leaving every other field alone.
    ///
    /// The bound on the text belongs to the domain
    /// ([`crate::domain::optional_note`]); this is the write, and it refuses a
    /// serial that is not in the inventory rather than updating nothing and
    /// reporting success.
    pub fn set_key_notes(&self, serial: u32, notes: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE keys SET notes = ?1, updated_at = ?2 WHERE serial = ?3",
            params![notes, Utc::now().to_rfc3339(), serial],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("serial {serial}")));
        }
        Ok(())
    }

    /// How much history refers to this serial: `(distributions, bootstrap runs)`.
    ///
    /// What makes a key removable. Used by [`Self::delete_key`] to refuse, and by
    /// the Inventory screen to say *why* before the operator clicks.
    pub fn key_history_counts(&self, serial: u32) -> Result<(usize, usize)> {
        let distributions: i64 = self.conn.query_row(
            "SELECT count(*) FROM distributions WHERE key_serial = ?1",
            params![serial],
            |row| row.get(0),
        )?;
        let runs: i64 = self.conn.query_row(
            "SELECT count(*) FROM bootstrap_runs WHERE key_serial = ?1",
            params![serial],
            |row| row.get(0),
        )?;
        Ok((distributions as usize, runs as usize))
    }

    /// Delete an inventory row, and return the record that was removed.
    ///
    /// This exists for the **intake mistake** — a mis-typed serial, a label
    /// scanned twice, a shipment recorded against the wrong unit — and for
    /// nothing else. A key that has been handed over or bootstrapped is refused
    /// with [`StoreError::HasHistory`]: retirement is the lifecycle exit and
    /// `Retired` keeps the record (see
    /// `features/key-lifecycle-and-revocation.md`), because a distribution or a
    /// bootstrap run that pointed at a serial nobody can look up is not a
    /// register.
    ///
    /// The removal itself stays in the audit trail, which no code path can edit,
    /// so "this serial was recorded and then removed, by whom and when" survives
    /// the row.
    pub fn delete_key(&self, serial: u32) -> Result<YubiKeyRecord> {
        let record = self
            .key_by_serial(serial)?
            .ok_or_else(|| StoreError::NotFound(format!("serial {serial}")))?;

        let (distributions, runs) = self.key_history_counts(serial)?;
        if distributions > 0 || runs > 0 {
            return Err(StoreError::HasHistory {
                serial,
                reason: format!(
                    "{distributions} hand-over(s) and {runs} bootstrap run(s) refer to it — \
                     retire the key instead, which keeps the record"
                ),
            });
        }

        self.conn
            .execute("DELETE FROM keys WHERE serial = ?1", params![serial])?;
        Ok(record)
    }

    // --------------------------------------------------------------- holders

    pub fn insert_holder(&self, holder: &Holder) -> Result<()> {
        self.conn.execute(
            "INSERT INTO holders (id, full_name, email, unit, registration, identification_number,
                                  phone, address, active, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(email) DO UPDATE SET
                 full_name = excluded.full_name,
                 unit = excluded.unit,
                 registration = excluded.registration,
                 -- An optional field is only ever filled in, never blanked by a
                 -- re-registration that omitted it.
                 identification_number = CASE
                     WHEN excluded.identification_number <> '' THEN excluded.identification_number
                     ELSE holders.identification_number
                 END,
                 phone = CASE
                     WHEN excluded.phone <> '' THEN excluded.phone ELSE holders.phone
                 END,
                 address = CASE
                     WHEN excluded.address <> '' THEN excluded.address ELSE holders.address
                 END,
                 active = excluded.active",
            params![
                holder.id.to_string(),
                holder.full_name,
                holder.email,
                holder.unit,
                holder.registration,
                holder.identification_number,
                holder.phone,
                holder.address,
                holder.active as i64,
                holder.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn holders(&self) -> Result<Vec<Holder>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, full_name, email, unit, registration, active, created_at,
                    identification_number, phone, address
             FROM holders ORDER BY full_name",
        )?;
        let rows = stmt.query_map([], row_to_holder)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }

    // ---------------------------------------------------------- distribution

    pub fn insert_distribution(&self, record: &DistributionRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO distributions (id, key_id, key_serial, holder_id, holder_display,
                                        distributed_at, distributed_by, method, receipt_ref,
                                        bootstrap_run_id, returned_at, returned_to, notes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                record.id.to_string(),
                record.key_id.to_string(),
                record.key_serial,
                record.holder_id.to_string(),
                record.holder_display,
                record.distributed_at.to_rfc3339(),
                record.distributed_by,
                delivery_str(record.method),
                record.receipt_ref,
                record.bootstrap_run_id.map(|id| id.to_string()),
                record.returned_at.map(|at| at.to_rfc3339()),
                record.returned_to,
                record.notes,
            ],
        )?;
        Ok(())
    }

    pub fn distributions(&self) -> Result<Vec<DistributionRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, key_id, key_serial, holder_id, holder_display, distributed_at,
                    distributed_by, method, receipt_ref, bootstrap_run_id, returned_at,
                    returned_to, notes
             FROM distributions ORDER BY distributed_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_distribution)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }

    /// Close an open distribution. History is never rewritten: only the return
    /// fields of the matching record are filled in.
    pub fn mark_returned(&self, id: Uuid, received_by: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE distributions SET returned_at = ?1, returned_to = ?2
             WHERE id = ?3 AND returned_at IS NULL",
            params![Utc::now().to_rfc3339(), received_by, id.to_string()],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("open distribution {id}")));
        }
        Ok(())
    }

    /// Record the unit's own reference for a hand-over's signed term.
    ///
    /// `features/receipts-and-terms.md` phase 4. The reference could only be typed
    /// while *recording* the hand-over, which is the wrong moment for the case that
    /// matters: a posted key's term comes back days later, and the operator had
    /// nowhere to put its reference. An operator who cannot record what they have in
    /// their hand writes it in a spreadsheet, and the register stops being the
    /// answer.
    ///
    /// Deliberately allowed on a **returned** hand-over too: a term that arrives
    /// after the key came back still closes the gap in the record for the period it
    /// was held.
    pub fn set_receipt_ref(&self, id: Uuid, reference: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE distributions SET receipt_ref = ?1 WHERE id = ?2",
            params![reference.trim(), id.to_string()],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("distribution {id}")));
        }
        Ok(())
    }

    // ---------------------------------------------------------- bootstrap runs

    /// Write a run and its steps.
    ///
    /// One transaction, because a run row whose step rows did not land would
    /// claim a procedure ran with nothing to say what it did. The steps are
    /// replaced wholesale rather than merged: the caller owns the outcome list,
    /// and a step that vanished from it was deselected, not forgotten.
    pub fn insert_run(&self, run: &BootstrapRun) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let id = run.id.to_string();
        tx.execute(
            "INSERT INTO bootstrap_runs (id, key_serial, holder_id, template_id, template_version,
                                         operator, started_at, finished_at, status, custody)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                 finished_at = excluded.finished_at,
                 status = excluded.status,
                 custody = excluded.custody",
            params![
                id,
                run.key_serial,
                run.holder_id.map(|id| id.to_string()),
                run.template_id,
                run.template_version,
                run.operator,
                run.started_at.to_rfc3339(),
                run.finished_at.map(|at| at.to_rfc3339()),
                serde_json::to_string(&run.status)?,
                run.custody,
            ],
        )?;
        self.write_run_steps(&id, &run.steps)?;
        tx.commit()?;
        Ok(())
    }

    pub fn runs(&self) -> Result<Vec<BootstrapRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, key_serial, holder_id, template_id, template_version, operator,
                    started_at, finished_at, status, custody
             FROM bootstrap_runs ORDER BY started_at DESC",
        )?;
        let mut runs = stmt
            .query_map([], row_to_run)?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()?;

        // One query for every run's steps rather than one per run: the Bootstrap
        // screen reads this on every refresh, and a query per run would make the
        // cost of opening the screen grow with the history.
        let mut by_run = self.all_run_steps()?;
        for run in &mut runs {
            run.steps = by_run.remove(&run.id).unwrap_or_default();
        }
        Ok(runs)
    }

    /// Every run's steps, in template order, keyed by run.
    fn all_run_steps(&self) -> Result<std::collections::HashMap<Uuid, Vec<StepOutcome>>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, step_id, kind, status, started_at, finished_at, detail
             FROM bootstrap_run_steps ORDER BY run_id, position",
        )?;
        let rows = stmt
            .query_map([], row_to_run_step)?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut by_run: std::collections::HashMap<Uuid, Vec<StepOutcome>> =
            std::collections::HashMap::new();
        for row in rows {
            let (run_id, step) = row?;
            by_run.entry(run_id).or_default().push(step);
        }
        Ok(by_run)
    }

    /// How many runs recorded each `(step kind, status)` pair.
    ///
    /// The question this exists to answer is "what has actually been applied to
    /// the keys we handed out" — which before schema v5 meant parsing every run's
    /// JSON blob in Rust, and is now an aggregate the database does.
    pub fn step_outcome_tally(
        &self,
    ) -> Result<std::collections::BTreeMap<(StepKind, StepStatus), usize>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, status, count(*) FROM bootstrap_run_steps GROUP BY kind, status",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut tally = std::collections::BTreeMap::new();
        for (kind, status, count) in rows {
            tally.insert(
                (step_kind_from(&kind)?, step_status_from(&status)?),
                count as usize,
            );
        }
        Ok(tally)
    }

    // ------------------------------------------------------------- lifecycle

    /// Every run against one key, newest first, with its steps.
    ///
    /// The lifecycle reads a single key's history — what was put on it, and which
    /// applets were written to — so it asks for that key rather than filtering
    /// [`Self::runs`] in Rust: an incident on one serial should not cost a read of
    /// every run the unit has ever performed.
    pub fn runs_for(&self, serial: u32) -> Result<Vec<BootstrapRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, key_serial, holder_id, template_id, template_version, operator,
                    started_at, finished_at, status, custody
             FROM bootstrap_runs WHERE key_serial = ?1 ORDER BY started_at DESC",
        )?;
        let mut runs = stmt
            .query_map(params![serial], row_to_run)?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()?;

        let mut steps = self.all_run_steps()?;
        for run in &mut runs {
            run.steps = steps.remove(&run.id).unwrap_or_default();
        }
        Ok(runs)
    }

    /// Record a loss report **and** move the key to `Lost`, or neither.
    ///
    /// One method because they are one operation, and the ordering is the one
    /// `features/distribution-records.md` was corrected into: **the lifecycle is
    /// asked first.** A report written against a key the lifecycle will not move —
    /// a retired key, say — would leave the register holding an incident for a key
    /// whose status contradicts it, with nothing the operator could do about
    /// either half. So the transition is checked, then the report is written, then
    /// the status is set, all in one transaction.
    ///
    /// The audit entry is the caller's: this is the store, and
    /// `YkDistApp::report_incident` writes `key.reported_lost` with
    /// [`KeyIncident::audit_detail`].
    pub fn report_incident(&self, incident: &KeyIncident) -> Result<()> {
        let current = self
            .key_by_serial(incident.key_serial)?
            .ok_or_else(|| StoreError::NotFound(format!("serial {}", incident.key_serial)))?;
        if current.status != KeyStatus::Lost && !current.status.can_transition_to(KeyStatus::Lost) {
            return Err(StoreError::Transition {
                from: current.status.label().into(),
                to: KeyStatus::Lost.label().into(),
            });
        }

        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO key_incidents (id, key_serial, kind, reported_at, reported_by,
                                        holder_display, circumstances, recorded_at, recorded_by,
                                        closed_at, closing_note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, '')",
            params![
                incident.id.to_string(),
                incident.key_serial,
                incident_kind_str(incident.kind),
                incident.reported_at.to_rfc3339(),
                incident.reported_by,
                incident.holder_display,
                incident.circumstances,
                incident.recorded_at.to_rfc3339(),
                incident.recorded_by,
            ],
        )?;
        tx.execute(
            "UPDATE keys SET status = ?1, updated_at = ?2 WHERE serial = ?3",
            params![
                key_status_str(KeyStatus::Lost),
                Utc::now().to_rfc3339(),
                incident.key_serial
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Every incident on record for one key, newest report first.
    pub fn incidents_for(&self, serial: u32) -> Result<Vec<KeyIncident>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, key_serial, kind, reported_at, reported_by, holder_display, circumstances,
                    recorded_at, recorded_by, closed_at, closing_note
             FROM key_incidents WHERE key_serial = ?1 ORDER BY reported_at DESC",
        )?;
        let rows = stmt.query_map(params![serial], row_to_incident)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }

    /// Every open incident, for the banner that says an incident is outstanding.
    pub fn open_incidents(&self) -> Result<Vec<KeyIncident>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, key_serial, kind, reported_at, reported_by, holder_display, circumstances,
                    recorded_at, recorded_by, closed_at, closing_note
             FROM key_incidents WHERE closed_at IS NULL ORDER BY reported_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_incident)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }

    /// Close an incident, keeping the note that says on what basis.
    ///
    /// Closing is a claim that nothing is outstanding, which is why the caller has
    /// to have looked: [`Self::outstanding_for`] is what the screen shows beside
    /// the button, and a closing note is how a *waived* obligation stays visible
    /// rather than disappearing. History is not rewritten — the report's own fields
    /// are untouched.
    pub fn close_incident(&self, id: Uuid, note: &str) -> Result<KeyIncident> {
        let changed = self.conn.execute(
            "UPDATE key_incidents SET closed_at = ?1, closing_note = ?2
             WHERE id = ?3 AND closed_at IS NULL",
            params![Utc::now().to_rfc3339(), note.trim(), id.to_string()],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("open incident {id}")));
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, key_serial, kind, reported_at, reported_by, holder_display, circumstances,
                    recorded_at, recorded_by, closed_at, closing_note
             FROM key_incidents WHERE id = ?1",
        )?;
        stmt.query_row(params![id.to_string()], row_to_incident)?
    }

    /// Record that one obligation has been met.
    pub fn insert_remediation(&self, remediation: &Remediation) -> Result<()> {
        self.conn.execute(
            "INSERT INTO key_remediations (id, key_serial, incident_id, kind, subject, reference,
                                           reason, recorded_at, recorded_by, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                remediation.id.to_string(),
                remediation.key_serial,
                remediation.incident_id.map(|id| id.to_string()),
                remediation_kind_str(remediation.kind),
                remediation.subject,
                remediation.reference,
                remediation.reason,
                remediation.recorded_at.to_rfc3339(),
                remediation.recorded_by,
                remediation.detail,
            ],
        )?;
        Ok(())
    }

    /// Everything that has been done about this key, newest first.
    pub fn remediations_for(&self, serial: u32) -> Result<Vec<Remediation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, key_serial, incident_id, kind, subject, reference, reason, recorded_at,
                    recorded_by, detail
             FROM key_remediations WHERE key_serial = ?1 ORDER BY recorded_at DESC",
        )?;
        let rows = stmt.query_map(params![serial], row_to_remediation)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }

    /// Every remediation on record, newest first.
    ///
    /// The whole-register read the reports need
    /// (`features/reports-and-export.md`): the reconciliation report asks whether
    /// each returned key has been sanitised, and answering that one serial at a
    /// time would be a query per key on a register that may hold hundreds.
    pub fn remediations(&self) -> Result<Vec<Remediation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, key_serial, incident_id, kind, subject, reference, reason, recorded_at,
                    recorded_by, detail
             FROM key_remediations ORDER BY recorded_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_remediation)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }

    /// What this key was carrying, read off its runs.
    pub fn dependencies_for(&self, serial: u32) -> Result<Vec<Dependency>> {
        Ok(crate::domain::lifecycle::dependencies(
            &self.runs_for(serial)?,
        ))
    }

    /// What is still owed after an incident: the certificates nobody has revoked
    /// and the credentials nobody has removed.
    pub fn outstanding_for(&self, serial: u32) -> Result<Vec<Dependency>> {
        let dependencies = self.dependencies_for(serial)?;
        let remediations = self.remediations_for(serial)?;
        Ok(
            crate::domain::lifecycle::outstanding(&dependencies, &remediations)
                .into_iter()
                .cloned()
                .collect(),
        )
    }

    /// Which applets still carry what a run put on them
    /// (`features/key-lifecycle-and-revocation.md` phase 6).
    pub fn sanitisation_for(&self, serial: u32) -> Result<Sanitisation> {
        Ok(crate::domain::lifecycle::sanitisation(
            &self.runs_for(serial)?,
            &self.remediations_for(serial)?,
        ))
    }

    /// Open an RMA case: the key has physically left for the supplier.
    pub fn insert_rma(&self, case: &RmaCase) -> Result<()> {
        if self.key_by_serial(case.key_serial)?.is_none() {
            return Err(StoreError::NotFound(format!("serial {}", case.key_serial)));
        }
        self.conn.execute(
            "INSERT INTO key_rma (id, key_serial, reference, sent_at, sent_by, fault,
                                  replacement_serial, replaced_at, closed_at, notes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, NULL, ?7)",
            params![
                case.id.to_string(),
                case.key_serial,
                case.reference,
                case.sent_at.to_rfc3339(),
                case.sent_by,
                case.fault,
                case.notes,
            ],
        )?;
        Ok(())
    }

    /// Link the replacement the supplier sent back.
    ///
    /// The replacement must be **in the inventory already**, and that refusal is
    /// the point of the method rather than a formality: a case pointing at a serial
    /// nobody has recorded is the same broken reference `delete_key` refuses to
    /// create, and the replacement is a new key that has to be intaken like any
    /// other — read from the hardware, provenance recorded — before it can be said
    /// to have replaced anything.
    pub fn link_rma_replacement(&self, id: Uuid, replacement: u32) -> Result<RmaCase> {
        let case = self.rma_case(id)?;
        if case.replacement_serial.is_some() {
            return Err(StoreError::NotFound(format!(
                "RMA case {id} already has a replacement recorded"
            )));
        }
        if replacement == case.key_serial {
            return Err(StoreError::NotFound(format!(
                "serial {replacement} cannot replace itself"
            )));
        }
        if self.key_by_serial(replacement)?.is_none() {
            return Err(StoreError::NotFound(format!(
                "serial {replacement} is not in the inventory — record the replacement key first, \
                 from the hardware, and then link it"
            )));
        }
        self.conn.execute(
            "UPDATE key_rma SET replacement_serial = ?1, replaced_at = ?2 WHERE id = ?3",
            params![replacement, Utc::now().to_rfc3339(), id.to_string()],
        )?;
        self.rma_case(id)
    }

    /// Close a case that is not producing a replacement — a refusal, a refund, a
    /// key written off.
    pub fn close_rma(&self, id: Uuid, note: &str) -> Result<RmaCase> {
        let changed = self.conn.execute(
            "UPDATE key_rma SET closed_at = ?1, notes = ?2
             WHERE id = ?3 AND closed_at IS NULL AND replacement_serial IS NULL",
            params![Utc::now().to_rfc3339(), note.trim(), id.to_string()],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("open RMA case {id}")));
        }
        self.rma_case(id)
    }

    pub fn rma_case(&self, id: Uuid) -> Result<RmaCase> {
        let mut stmt = self.conn.prepare(
            "SELECT id, key_serial, reference, sent_at, sent_by, fault, replacement_serial,
                    replaced_at, closed_at, notes
             FROM key_rma WHERE id = ?1",
        )?;
        stmt.query_row(params![id.to_string()], row_to_rma)?
    }

    /// Every RMA case for one key, newest first.
    pub fn rma_cases_for(&self, serial: u32) -> Result<Vec<RmaCase>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, key_serial, reference, sent_at, sent_by, fault, replacement_serial,
                    replaced_at, closed_at, notes
             FROM key_rma WHERE key_serial = ?1 ORDER BY sent_at DESC",
        )?;
        let rows = stmt.query_map(params![serial], row_to_rma)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }

    // ---------------------------------------------------------------- import

    /// What the register already holds, for [`import::plan`] to compare against.
    ///
    /// Two sets rather than the full records: the planner only needs to answer
    /// "is this new?", and loading every key and holder to answer it would make
    /// previewing a 2000-row spreadsheet quadratic.
    pub fn existing_for_import(&self) -> Result<import::Existing> {
        let mut serials = self.conn.prepare("SELECT serial FROM keys")?;
        let serials = serials
            .query_map([], |row| row.get::<_, u32>(0))?
            .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?;

        let mut emails = self.conn.prepare("SELECT email FROM holders")?;
        let emails = emails
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?;

        Ok(import::Existing { serials, emails })
    }

    /// Apply a plan the operator has seen and accepted.
    ///
    /// One transaction: a spreadsheet half-imported is worse than one not
    /// imported, because the operator cannot tell which half. Refused rows were
    /// already refused at plan time and are counted, not retried — the operator
    /// fixes the spreadsheet and imports again, and the rows that did land are
    /// upserts, so importing twice is safe.
    ///
    /// Does **not** write audit entries: the caller does, because it knows the
    /// operator. See `AGENTS.md` §3.
    pub fn apply_import(&self, plan: &import::ImportPlan) -> Result<import::ImportOutcome> {
        use import::RowPlan;

        let tx = self.conn.unchecked_transaction()?;
        let mut outcome = import::ImportOutcome::default();

        for row in &plan.rows {
            match &row.plan {
                RowPlan::Refused { .. } => {
                    outcome.refused += 1;
                    continue;
                }
                RowPlan::NewKey { .. } => outcome.keys_added += 1,
                RowPlan::KnownKey { .. } => outcome.keys_refreshed += 1,
                RowPlan::NewHolder { .. } => outcome.holders_added += 1,
                RowPlan::KnownHolder { .. } => outcome.holders_refreshed += 1,
                RowPlan::NewKeyAndHolder { .. } => {
                    outcome.keys_added += 1;
                    outcome.holders_added += 1;
                }
            }

            // `upsert_key` protects provenance itself: a serial already verified
            // by a device read is not downgraded to `manual-entry` by a
            // spreadsheet claiming the same key.
            if let Some(key) = &row.key {
                self.upsert_key(key)?;
            }
            if let Some(holder) = &row.holder {
                self.insert_holder(holder)?;
            }
        }

        tx.commit()?;
        Ok(outcome)
    }

    // ------------------------------------------------------------- templates

    pub fn upsert_template(&self, template: &BootstrapTemplate) -> Result<()> {
        self.conn.execute(
            "INSERT INTO templates (id, version, name, body, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id, version) DO UPDATE SET
                 name = excluded.name,
                 body = excluded.body,
                 updated_at = excluded.updated_at",
            params![
                template.id,
                template.version,
                template.name,
                serde_json::to_string(template)?,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Templates the wizard may offer: everything that has not been retired.
    pub fn templates(&self) -> Result<Vec<BootstrapTemplate>> {
        let mut stmt = self
            .conn
            .prepare("SELECT body FROM templates WHERE retired_at IS NULL ORDER BY id, version")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for body in rows {
            out.push(serde_json::from_str(&body?)?);
        }
        Ok(out)
    }

    /// Every template version on record, retired ones included, each with the
    /// number of bootstrap runs that recorded it.
    ///
    /// The run count comes from the same query on purpose: the Templates screen
    /// uses it to say *why* a version cannot be deleted before the operator
    /// clicks, and a count fetched separately would be a second truth.
    pub fn template_catalogue(&self) -> Result<Vec<StoredTemplate>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.body, t.retired_at, t.updated_at,
                    (SELECT count(*) FROM bootstrap_runs r
                      WHERE r.template_id = t.id AND r.template_version = t.version)
             FROM templates t ORDER BY t.id, t.version",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (body, retired_at, updated_at, runs) = row?;
            out.push(StoredTemplate {
                template: serde_json::from_str(&body)?,
                retired_at,
                runs: runs.max(0) as usize,
                updated_at,
            });
        }
        Ok(out)
    }

    /// Versions on record for one template id, retired ones included.
    ///
    /// Retired versions count: the numbering must never reuse a version a run
    /// might refer to.
    pub fn template_versions(&self, id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT version FROM templates WHERE id = ?1 ORDER BY version")?;
        let rows = stmt.query_map(params![id], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Store an edited template **as a new version**, and return what was stored.
    ///
    /// This is the only write the editor performs, and it never overwrites. A
    /// bootstrap run records the `(template_id, template_version)` it applied, so
    /// the version on record has to stay exactly as it was — the newest version is
    /// what the wizard offers next (`template::latest_per_id`). The draft's own
    /// version is ignored: the number comes from what the database already holds,
    /// so two operators editing the same template cannot both produce "version 2".
    ///
    /// The template is checked first ([`BootstrapTemplate::check`]), which plans it
    /// against a sample context — nothing that cannot be planned reaches the
    /// database.
    pub fn save_template_version(&self, draft: &BootstrapTemplate) -> Result<BootstrapTemplate> {
        draft.check()?;
        let trimmed = draft.as_version(&draft.version);
        let existing = self.template_versions(&trimmed.id)?;
        let stored = trimmed.as_version(&crate::versioning::next_version(&existing));
        self.upsert_template(&stored)?;
        Ok(stored)
    }

    /// One template version, with its state and run count.
    pub fn stored_template(&self, id: &str, version: &str) -> Result<StoredTemplate> {
        self.template_catalogue()?
            .into_iter()
            .find(|stored| stored.template.id == id && stored.template.version == version)
            .ok_or_else(|| StoreError::NotFound(format!("template {id} version {version}")))
    }

    /// Withdraw a version from the wizard, keeping it on record.
    ///
    /// The row stays, so a run that applied it can still be explained, and
    /// [`Self::seed_builtin_templates`] will not resurrect it — seeding asks
    /// whether the `(id, version)` exists, not whether it is in use.
    pub fn retire_template(&self, id: &str, version: &str) -> Result<StoredTemplate> {
        let stored = self.stored_template(id, version)?;
        self.conn.execute(
            "UPDATE templates SET retired_at = ?1 WHERE id = ?2 AND version = ?3
                                   AND retired_at IS NULL",
            params![Utc::now().to_rfc3339(), id, version],
        )?;
        Ok(stored)
    }

    /// Put a retired version back in use.
    pub fn reinstate_template(&self, id: &str, version: &str) -> Result<StoredTemplate> {
        let stored = self.stored_template(id, version)?;
        self.conn.execute(
            "UPDATE templates SET retired_at = NULL WHERE id = ?1 AND version = ?2",
            params![id, version],
        )?;
        Ok(stored)
    }

    /// Delete a template version outright, for one typed by mistake.
    ///
    /// Refused when a bootstrap run recorded it, and refused for a version this
    /// build ships — that one would be re-created the next time the database is
    /// opened, so deleting it would only look like it worked. Both refusals name
    /// retirement, which is the operation that does what was asked. See
    /// [`StoredTemplate::removal_refusal`].
    pub fn delete_template(&self, id: &str, version: &str) -> Result<StoredTemplate> {
        let stored = self.stored_template(id, version)?;
        if let Some(reason) = stored.removal_refusal() {
            return Err(StoreError::TemplateInUse {
                id: id.to_owned(),
                version: version.to_owned(),
                reason,
            });
        }
        self.conn.execute(
            "DELETE FROM templates WHERE id = ?1 AND version = ?2",
            params![id, version],
        )?;
        Ok(stored)
    }

    /// Store a template that arrived in a file, as a new version of its id.
    ///
    /// `features/bootstrap-templates.md` phase 4. Three rules, and each of them is
    /// a decision this feature had already made for the editor:
    ///
    /// 1. **The gate applies.** [`BootstrapTemplate::check`] runs — again, having
    ///    already run when the file was read — because nothing reaches the
    ///    database without being planned against sample data, whatever door it
    ///    came in by.
    /// 2. **The receiving database numbers the version.** The file's version is
    ///    information, not an instruction: two units both calling their procedure
    ///    "version 2" is the normal case, and honouring an incoming number would
    ///    either collide with a different local procedure of that number or
    ///    silently redefine what "v2" means in this register.
    /// 3. **An import that changes nothing stores nothing.** If a version of this
    ///    id already has the same canonical bytes *and* the same signature, the
    ///    import is reported as already present. Importing the same file twice is
    ///    something an operator will do — from a mail thread, then from a share —
    ///    and answering it with two identical versions would leave the catalogue
    ///    growing a row per attempt.
    pub fn import_template(&self, incoming: &BootstrapTemplate) -> Result<TemplateImport> {
        incoming.check()?;

        let bytes = crate::template::signing::canonical_bytes(incoming);
        for stored in self.template_catalogue()? {
            if stored.template.id != incoming.id {
                continue;
            }
            if crate::template::signing::canonical_bytes(&stored.template) == bytes
                && stored.template.signature == incoming.signature
            {
                return Ok(TemplateImport::AlreadyPresent {
                    retired: stored.is_retired(),
                    version: stored.template.version,
                });
            }
        }

        let existing = self.template_versions(&incoming.id)?;
        let previous = existing
            .iter()
            .max_by(|a, b| {
                crate::versioning::version_order(a).cmp(&crate::versioning::version_order(b))
            })
            .cloned();
        let stored = incoming.as_version(&crate::versioning::next_version(&existing));
        self.upsert_template(&stored)?;
        Ok(TemplateImport::Stored {
            template: Box::new(stored),
            previous,
        })
    }

    /// Make sure the built-in templates exist, without clobbering edits to a
    /// template of the same id *and* version.
    pub fn seed_builtin_templates(&self) -> Result<usize> {
        let mut inserted = 0;
        for template in BootstrapTemplate::builtin() {
            let exists: i64 = self.conn.query_row(
                "SELECT count(*) FROM templates WHERE id = ?1 AND version = ?2",
                params![template.id, template.version],
                |row| row.get(0),
            )?;
            if exists == 0 {
                self.upsert_template(&template)?;
                inserted += 1;
            }
        }
        Ok(inserted)
    }

    // --------------------------------------------------------- term templates

    pub fn upsert_term_template(&self, template: &TermTemplate) -> Result<()> {
        self.conn.execute(
            "INSERT INTO term_templates (id, language, version, title, body, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id, language, version) DO UPDATE SET
                 title = excluded.title,
                 body = excluded.body,
                 updated_at = excluded.updated_at",
            params![
                template.id,
                template.language,
                template.version,
                template.title,
                template.body,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn term_templates(&self) -> Result<Vec<TermTemplate>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, language, version, title, body FROM term_templates
             ORDER BY id, language, version",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(TermTemplate {
                id: row.get(0)?,
                language: row.get(1)?,
                version: row.get(2)?,
                title: row.get(3)?,
                body: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Versions on record for one term in one language, oldest first as stored.
    pub fn term_template_versions(&self, id: &str, language: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT version FROM term_templates WHERE id = ?1 AND language = ?2
             ORDER BY version",
        )?;
        let rows = stmt.query_map(params![id, language], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Store an edited term **as a new version**, and return what was stored.
    ///
    /// This is the only write the editor performs, and it never overwrites: the
    /// version somebody already signed stays in the database, readable forever,
    /// while [`crate::term::choose_template`] hands the newest version to the next
    /// term generated. The draft's own `version` field is ignored — the number
    /// comes from what the database already holds, so two operators editing the
    /// same term cannot both produce "version 2".
    ///
    /// The template is checked first ([`TermTemplate::check`]), so an unknown
    /// variable is refused here rather than surfacing at the counter.
    pub fn save_term_template_version(&self, draft: &TermTemplate) -> Result<TermTemplate> {
        draft.check()?;
        let trimmed = draft.as_version(&draft.version);
        let existing = self.term_template_versions(&trimmed.id, &trimmed.language)?;
        let stored = trimmed.as_version(&crate::term::next_version(&existing));
        self.upsert_term_template(&stored)?;
        Ok(stored)
    }

    /// Insert the built-in terms, leaving an edited template of the same id,
    /// language and version alone.
    pub fn seed_builtin_terms(&self) -> Result<usize> {
        let mut inserted = 0;
        for template in TermTemplate::builtin() {
            let exists: i64 = self.conn.query_row(
                "SELECT count(*) FROM term_templates
                 WHERE id = ?1 AND language = ?2 AND version = ?3",
                params![template.id, template.language, template.version],
                |row| row.get(0),
            )?;
            if exists == 0 {
                self.upsert_term_template(&template)?;
                inserted += 1;
            }
        }
        Ok(inserted)
    }

    // ------------------------------------------------------------- documents

    /// File a document (typically the signed term) against a distribution.
    pub fn insert_document(&self, document: &AttachedDocument) -> Result<()> {
        let content = document.content.as_ref().ok_or(StoreError::Decode {
            column: "documents.content",
            value: "no content to store".into(),
        })?;

        self.conn.execute(
            "INSERT INTO documents (id, distribution_id, kind, filename, media_type, size_bytes,
                                    sha256, uploaded_at, uploaded_by, content)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                document.id.to_string(),
                document.distribution_id.to_string(),
                document_kind_str(document.kind),
                document.filename,
                document.media_type,
                document.size_bytes as i64,
                document.sha256,
                document.uploaded_at.to_rfc3339(),
                document.uploaded_by,
                content,
            ],
        )?;
        Ok(())
    }

    /// Documents filed against a distribution, **without** their content.
    pub fn documents_for(&self, distribution_id: Uuid) -> Result<Vec<AttachedDocument>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, distribution_id, kind, filename, media_type, size_bytes, sha256,
                    uploaded_at, uploaded_by
             FROM documents WHERE distribution_id = ?1 ORDER BY uploaded_at DESC",
        )?;
        let rows = stmt.query_map(params![distribution_id.to_string()], row_to_document)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }

    /// What is on file against each hand-over, **counted per kind**.
    ///
    /// The signature state needs to know about *signed* terms specifically
    /// (`crate::receipt`): a generated term filed against a hand-over is not
    /// evidence that anybody signed it, and a total would let one stand in for the
    /// other. One query, grouped by both columns, because two queries would be two
    /// answers that could disagree.
    pub fn filed_documents(
        &self,
    ) -> Result<std::collections::BTreeMap<Uuid, crate::receipt::Filed>> {
        let mut stmt = self.conn.prepare(
            "SELECT distribution_id, kind, count(*) FROM documents GROUP BY distribution_id, kind",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut out: std::collections::BTreeMap<Uuid, crate::receipt::Filed> =
            std::collections::BTreeMap::new();
        for row in rows {
            let (id, kind, count) = row?;
            let Ok(id) = Uuid::parse_str(&id) else {
                continue;
            };
            // An unknown kind still counts towards the total: a row this build does
            // not recognise is a document somebody filed, and reporting "no
            // documents" for it would be worse than not knowing what it is.
            let kind = document_kind_from(&kind).unwrap_or(DocumentKind::Other);
            out.entry(id).or_default().add(kind, count.max(0) as usize);
        }
        Ok(out)
    }

    /// How many documents each distribution has, for the table badge.
    pub fn document_counts(&self) -> Result<std::collections::BTreeMap<Uuid, usize>> {
        let mut stmt = self
            .conn
            .prepare("SELECT distribution_id, count(*) FROM documents GROUP BY distribution_id")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut out = std::collections::BTreeMap::new();
        for row in rows {
            let (id, count) = row?;
            if let Ok(id) = Uuid::parse_str(&id) {
                out.insert(id, count as usize);
            }
        }
        Ok(out)
    }

    /// Load one document with its content, for export.
    pub fn document_content(&self, id: Uuid) -> Result<AttachedDocument> {
        let found = self
            .conn
            .query_row(
                "SELECT id, distribution_id, kind, filename, media_type, size_bytes, sha256,
                        uploaded_at, uploaded_by, content
                 FROM documents WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    let document = row_to_document(row)?;
                    let content: Vec<u8> = row.get(9)?;
                    Ok(document.map(|mut d| {
                        d.content = Some(content);
                        d
                    }))
                },
            )
            .optional()?;

        match found {
            Some(document) => document,
            None => Err(StoreError::NotFound(format!("document {id}"))),
        }
    }

    // ----------------------------------------------------------------- audit

    /// Append one hash-chained audit entry.
    ///
    /// The table refuses `UPDATE`/`DELETE` at the database level, so this is
    /// the only way its contents can change.
    pub fn append_audit(
        &self,
        actor: &str,
        event: &str,
        target: &str,
        details: &str,
    ) -> Result<AuditEntry> {
        let (last_seq, last_hash) = self
            .conn
            .query_row(
                "SELECT seq, hash FROM audit ORDER BY seq DESC LIMIT 1",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .unwrap_or((0, GENESIS.to_owned()));

        let mut entry = AuditEntry {
            seq: last_seq as u64 + 1,
            at: Utc::now(),
            actor: actor.to_owned(),
            event: event.to_owned(),
            target: target.to_owned(),
            details: details.to_owned(),
            prev_hash: last_hash,
            hash: String::new(),
        };
        entry.hash = entry.compute_hash();

        self.conn.execute(
            "INSERT INTO audit (seq, at, actor, event, target, details, prev_hash, hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                entry.seq as i64,
                entry.at.to_rfc3339(),
                entry.actor,
                entry.event,
                entry.target,
                entry.details,
                entry.prev_hash,
                entry.hash,
            ],
        )?;

        self.mirror_entry(&entry);
        Ok(entry)
    }

    /// Write an entry to the segregated mirror, when one is configured.
    ///
    /// The database row is the record; the mirror is the second copy that makes
    /// a rebuilt chain detectable. So a mirror failure must **not** fail the
    /// mutation — undoing a hand-over because a second file could not be written
    /// would lose the very fact being recorded. It is instead logged at `error`
    /// and surfaced through [`Self::mirror_status`], per `AGENTS.md` §3: audit
    /// failure is loud, never `let _ =`.
    fn mirror_entry(&self, entry: &AuditEntry) {
        let Some(mirror) = &self.mirror else {
            return;
        };
        let mut mirror = mirror.borrow_mut();
        // Verbatim: the mirror is a copy of *this* chain, so its hashes are the
        // database's hashes. Re-deriving the entry would give it a second
        // timestamp and a second sequence, and the two chains could then never
        // be compared.
        if let Err(e) = mirror.append_existing(entry) {
            tracing::error!(
                event = "audit.mirror.append_failed",
                path = %mirror.path().display(),
                seq = entry.seq,
                reason = %e,
                detail = "the entry is in the database; the segregated copy is now behind"
            );
        }
    }

    /// Compare this register's chain with its segregated mirror.
    ///
    /// See `features/audit-trail.md` phase 2. A divergence means one of the two
    /// was rewritten, which is the case the mirror exists to catch: the database
    /// triggers stop every ordinary path, and a mirror on storage the operator
    /// cannot rewrite catches the path that got around them.
    pub fn mirror_status(&self) -> audit::MirrorStatus {
        let Some(mirror) = &self.mirror else {
            return audit::MirrorStatus::NotConfigured;
        };
        let database = match self.audit_entries(usize::MAX) {
            Ok(mut entries) => {
                entries.reverse(); // `audit_entries` is newest-first.
                entries
            }
            Err(e) => {
                return audit::MirrorStatus::Unreadable {
                    reason: e.to_string(),
                };
            }
        };
        match mirror.borrow().entries() {
            Ok(entries) => audit::compare_with_mirror(&database, &entries),
            Err(e) => audit::MirrorStatus::Unreadable {
                reason: e.to_string(),
            },
        }
    }

    /// Whether the chain verified when the register was opened.
    ///
    /// `features/audit-trail.md` phase 7: a broken chain discovered when
    /// somebody happens to press "Verify" is discovered too late. Checked once
    /// at open, and carried so the shell can put it in front of the operator.
    pub fn chain_status(&self) -> &ChainStatus {
        &self.chain_status
    }

    /// Verify the chain once, at open.
    ///
    /// Bounded by [`Self::VERIFY_ON_OPEN_LIMIT`]: verification reads and re-hashes
    /// every entry, and a register with a year of history behind a network share
    /// should not make the application take ten seconds to start. Past the limit
    /// the check is left to the Audit screen's button, and the status says it was
    /// not done rather than implying it passed.
    fn verify_chain_on_open(&self) -> ChainStatus {
        let count: i64 = match self
            .conn
            .query_row("SELECT count(*) FROM audit", [], |row| row.get(0))
        {
            Ok(count) => count,
            Err(e) => {
                return ChainStatus::Broken {
                    reason: e.to_string(),
                };
            }
        };

        if count as usize > Self::VERIFY_ON_OPEN_LIMIT {
            tracing::info!(
                event = "audit.verify.skipped_at_open",
                entries = count,
                limit = Self::VERIFY_ON_OPEN_LIMIT
            );
            return ChainStatus::NotChecked;
        }

        match self.verify_audit() {
            Ok(entries) => ChainStatus::Verified { entries },
            Err(e) => {
                // Loud, per AGENTS.md §3: a broken chain is the one thing this
                // whole feature exists to detect.
                tracing::error!(
                    event = "audit.verify.failed",
                    path = %self.path.display(),
                    reason = %e
                );
                ChainStatus::Broken {
                    reason: e.to_string(),
                }
            }
        }
    }

    /// How many entries are worth re-hashing before showing the first frame.
    pub const VERIFY_ON_OPEN_LIMIT: usize = 20_000;

    /// Audit entries matching a filter, newest first.
    ///
    /// Filtering happens in Rust rather than SQL because [`audit::AuditFilter`]
    /// is where the rules are tested, and a `LIKE` clause built in two places
    /// would eventually disagree with itself. `limit` is applied *after* the
    /// filter, so asking for "everything about serial 20423633" does not return
    /// nothing because the newest 500 entries happen to be about another key.
    pub fn audit_entries_matching(
        &self,
        filter: &audit::AuditFilter,
        limit: usize,
    ) -> Result<Vec<AuditEntry>> {
        let all = self.audit_entries(usize::MAX)?;
        Ok(all
            .into_iter()
            .filter(|entry| filter.matches(entry))
            .take(limit)
            .collect())
    }

    /// Audit entries, newest first, capped at `limit`.
    pub fn audit_entries(&self, limit: usize) -> Result<Vec<AuditEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, at, actor, event, target, details, prev_hash, hash
             FROM audit ORDER BY seq DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], row_to_audit)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }

    /// The whole chain, **oldest first** — the order it was written, the order it
    /// verifies in, and the order an extract has to be cut from.
    ///
    /// [`Self::audit_entries`] is the screen's read and comes newest first,
    /// because a screen shows what just happened. An extract is the opposite job:
    /// it is evidence, its last entry is the chain head, and reversing a list to
    /// find that would be one more place to get it the wrong way round.
    pub fn audit_trail(&self) -> Result<Vec<AuditEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, at, actor, event, target, details, prev_hash, hash
             FROM audit ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map([], row_to_audit)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }

    /// Verify the whole chain; returns the number of entries checked.
    pub fn verify_audit(&self) -> Result<usize> {
        let entries = self.audit_trail()?;
        crate::audit::verify(&entries).map_err(|e| StoreError::Decode {
            column: "audit",
            value: e.to_string(),
        })?;
        Ok(entries.len())
    }
}

/// Apply `PRAGMA key`. Only meaningful in a SQLCipher build.
/// `sqlcipher_export` the whole database into `target`, under `key`.
///
/// `ATTACH … KEY ''` is how SQLCipher writes a *plain* file, which is what makes
/// removing a password the same operation as changing one.
fn export_to(conn: &Connection, target: &Path, key: Option<&str>, schema: i64) -> Result<()> {
    // The path is bound as a parameter and the key is applied with a pragma on
    // the attached handle, so neither is formatted into SQL. `AGENTS.md` §2: no
    // user value is ever concatenated into a statement, and a password least of
    // all.
    conn.execute(
        "ATTACH DATABASE ?1 AS rekeyed KEY ?2",
        params![target.to_string_lossy(), key.unwrap_or("")],
    )
    .map_err(|e| StoreError::Rekey {
        stage: "opening the new file",
        reason: e.to_string(),
    })?;

    let exported = conn
        .query_row("SELECT sqlcipher_export('rekeyed')", [], |_| Ok(()))
        .map_err(|e| StoreError::Rekey {
            stage: "copying the database",
            reason: e.to_string(),
        });

    // `user_version` is a pragma, and `sqlcipher_export` copies schema and rows
    // but not pragmas. Without this the copy arrives claiming version 0 and the
    // next open would run every migration again over a fully-migrated database.
    let versioned = exported.and_then(|()| {
        conn.execute_batch(&format!("PRAGMA rekeyed.user_version = {schema};"))
            .map_err(|e| StoreError::Rekey {
                stage: "carrying the schema version across",
                reason: e.to_string(),
            })
    });

    // Detached whatever happened: leaving it attached would keep a handle on a
    // file the caller is about to delete.
    let detached = conn
        .execute_batch("DETACH DATABASE rekeyed")
        .map_err(|e| StoreError::Rekey {
            stage: "closing the new file",
            reason: e.to_string(),
        });

    versioned.and(detached)
}

/// Prove a re-encrypted copy before anything is swapped.
///
/// On its own connection on purpose: this has to answer "will the file open when
/// it is reopened", and the handle that wrote it is not that question.
fn verify_rekeyed(target: &Path, key: Option<&str>, schema: i64) -> Result<()> {
    let conn = Connection::open(target).map_err(|e| StoreError::Rekey {
        stage: "reopening the new file",
        reason: e.to_string(),
    })?;
    if let Some(key) = key {
        apply_key(&conn, key)?;
    }

    let store = RekeyedCopy { conn };
    store.check(schema)
}

/// The questions asked of a re-encrypted copy before it replaces anything.
struct RekeyedCopy {
    conn: Connection,
}

impl RekeyedCopy {
    fn check(&self, schema: i64) -> Result<()> {
        // Reading anything is what surfaces a wrong key, so this is first.
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|e| StoreError::Rekey {
                stage: "reading the new file",
                reason: e.to_string(),
            })?;
        if version != schema {
            return Err(StoreError::Rekey {
                stage: "checking the new file",
                reason: format!("schema version {version} in the copy, {schema} in the original"),
            });
        }

        let integrity: String = self
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(|e| StoreError::Rekey {
                stage: "checking the new file",
                reason: e.to_string(),
            })?;
        if integrity != "ok" {
            return Err(StoreError::Rekey {
                stage: "checking the new file",
                reason: format!("integrity check said `{integrity}`"),
            });
        }

        // The audit chain is the thing that makes this register evidence, so a
        // copy whose chain does not verify is not a copy worth swapping in.
        let mut stmt = self
            .conn
            .prepare(
                "SELECT seq, at, actor, event, target, details, prev_hash, hash
                 FROM audit ORDER BY seq ASC",
            )
            .map_err(|e| StoreError::Rekey {
                stage: "reading the audit trail in the new file",
                reason: e.to_string(),
            })?;
        let entries = stmt
            .query_map([], row_to_audit)
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(|e| StoreError::Rekey {
                stage: "reading the audit trail in the new file",
                reason: e.to_string(),
            })?
            .into_iter()
            .collect::<Result<Vec<_>>>()?;

        crate::audit::verify(&entries).map_err(|e| StoreError::Rekey {
            stage: "verifying the audit chain in the new file",
            reason: e.to_string(),
        })?;
        Ok(())
    }
}

#[cfg(feature = "encrypted-db")]
fn apply_key(conn: &Connection, password: &str) -> Result<()> {
    conn.pragma_update(None, "key", password)?;
    Ok(())
}

#[cfg(not(feature = "encrypted-db"))]
fn apply_key(_conn: &Connection, _password: &str) -> Result<()> {
    Err(StoreError::EncryptionUnavailable)
}

/// SQLite reports a wrong SQLCipher key as "file is not a database".
fn is_encryption_error(extended_code: i32) -> bool {
    // SQLITE_NOTADB = 26, SQLITE_CORRUPT = 11 (and their extended variants).
    let primary = extended_code & 0xff;
    primary == 26 || primary == 11
}

// ------------------------------------------------------------- row decoding

type RowResult<T> = rusqlite::Result<Result<T>>;

/// Open the segregated audit mirror, if one is configured.
///
/// A mirror that cannot be opened is logged and dropped rather than failing the
/// register's open: the database's own trail is the record, and refusing to let
/// an operator work because a second copy is unreachable — a share not mounted,
/// a permission changed — would stop the hand-over rather than protect it. The
/// gap is visible through [`Store::mirror_status`].
fn open_mirror(path: Option<&Path>) -> Option<std::cell::RefCell<crate::audit::AuditLog>> {
    let path = path?;
    match crate::audit::AuditLog::open(path) {
        Ok(log) => {
            tracing::info!(event = "audit.mirror.opened", path = %path.display());
            Some(std::cell::RefCell::new(log))
        }
        Err(e) => {
            tracing::error!(
                event = "audit.mirror.unavailable",
                path = %path.display(),
                reason = %e,
                detail = "entries are being written to the database only"
            );
            None
        }
    }
}

fn parse_uuid(column: &'static str, raw: &str) -> Result<Uuid> {
    Uuid::parse_str(raw).map_err(|_| StoreError::Decode {
        column,
        value: raw.to_owned(),
    })
}

fn parse_time(column: &'static str, raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| StoreError::Decode {
            column,
            value: raw.to_owned(),
        })
}

/// The stored spelling of a status. Same string the audit trail uses
/// ([`KeyStatus::audit_name`]), so a column value and an audit detail cannot
/// drift apart.
pub fn key_status_str(status: KeyStatus) -> &'static str {
    status.audit_name()
}

pub fn key_status_from(raw: &str) -> Result<KeyStatus> {
    Ok(match raw {
        "in_stock" => KeyStatus::InStock,
        "bootstrapped" => KeyStatus::Bootstrapped,
        "distributed" => KeyStatus::Distributed,
        "returned" => KeyStatus::Returned,
        "lost" => KeyStatus::Lost,
        "retired" => KeyStatus::Retired,
        other => {
            return Err(StoreError::Decode {
                column: "keys.status",
                value: other.to_owned(),
            });
        }
    })
}

/// The stored spelling of a serial's provenance; see [`key_status_str`].
pub fn serial_source_str(source: SerialSource) -> &'static str {
    source.audit_name()
}

pub fn serial_source_from(raw: &str) -> Result<SerialSource> {
    Ok(match raw {
        "device" => SerialSource::Device,
        "scanned-label" => SerialSource::ScannedLabel,
        "manual-entry" => SerialSource::ManualEntry,
        other => {
            return Err(StoreError::Decode {
                column: "keys.serial_source",
                value: other.to_owned(),
            });
        }
    })
}

/// The stored spelling of a step kind — [`StepKind::slug`], which is also the
/// id the built-in templates give the step, so a run row and the template that
/// produced it read the same.
pub fn step_kind_str(kind: StepKind) -> &'static str {
    kind.slug()
}

pub fn step_kind_from(raw: &str) -> Result<StepKind> {
    StepKind::ALL
        .iter()
        .copied()
        .find(|kind| kind.slug() == raw)
        .ok_or_else(|| StoreError::Decode {
            column: "bootstrap_run_steps.kind",
            value: raw.to_owned(),
        })
}

pub fn step_status_str(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "pending",
        StepStatus::Running => "running",
        StepStatus::Done => "done",
        StepStatus::Failed => "failed",
        StepStatus::Skipped => "skipped",
    }
}

pub fn step_status_from(raw: &str) -> Result<StepStatus> {
    Ok(match raw {
        "pending" => StepStatus::Pending,
        "running" => StepStatus::Running,
        "done" => StepStatus::Done,
        "failed" => StepStatus::Failed,
        "skipped" => StepStatus::Skipped,
        other => {
            return Err(StoreError::Decode {
                column: "bootstrap_run_steps.status",
                value: other.to_owned(),
            });
        }
    })
}

/// The stored spelling of an incident kind; see [`key_status_str`].
pub fn incident_kind_str(kind: IncidentKind) -> &'static str {
    kind.audit_name()
}

pub fn incident_kind_from(raw: &str) -> Result<IncidentKind> {
    IncidentKind::ALL
        .iter()
        .copied()
        .find(|kind| kind.audit_name() == raw)
        .ok_or_else(|| StoreError::Decode {
            column: "key_incidents.kind",
            value: raw.to_owned(),
        })
}

/// The stored spelling of a remediation kind; see [`key_status_str`].
pub fn remediation_kind_str(kind: RemediationKind) -> &'static str {
    kind.audit_name()
}

pub fn remediation_kind_from(raw: &str) -> Result<RemediationKind> {
    Ok(match raw {
        "certificate-revoked" => RemediationKind::CertificateRevoked,
        "credential-removed" => RemediationKind::CredentialRemoved,
        "sanitised" => RemediationKind::Sanitised,
        other => {
            return Err(StoreError::Decode {
                column: "key_remediations.kind",
                value: other.to_owned(),
            });
        }
    })
}

pub fn document_kind_str(kind: DocumentKind) -> &'static str {
    match kind {
        DocumentKind::SignedTerm => "signed-term",
        DocumentKind::GeneratedTerm => "generated-term",
        DocumentKind::ReturnReceipt => "return-receipt",
        DocumentKind::Other => "other",
    }
}

pub fn document_kind_from(raw: &str) -> Result<DocumentKind> {
    Ok(match raw {
        "signed-term" => DocumentKind::SignedTerm,
        "generated-term" => DocumentKind::GeneratedTerm,
        "return-receipt" => DocumentKind::ReturnReceipt,
        "other" => DocumentKind::Other,
        other => {
            return Err(StoreError::Decode {
                column: "documents.kind",
                value: other.to_owned(),
            });
        }
    })
}

pub fn delivery_str(method: DeliveryMethod) -> &'static str {
    match method {
        DeliveryMethod::InPerson => "in_person",
        DeliveryMethod::Courier => "courier",
        DeliveryMethod::Post => "post",
    }
}

pub fn delivery_from(raw: &str) -> Result<DeliveryMethod> {
    Ok(match raw {
        "in_person" => DeliveryMethod::InPerson,
        "courier" => DeliveryMethod::Courier,
        "post" => DeliveryMethod::Post,
        other => {
            return Err(StoreError::Decode {
                column: "distributions.method",
                value: other.to_owned(),
            });
        }
    })
}

fn row_to_key(row: &rusqlite::Row<'_>) -> RowResult<YubiKeyRecord> {
    let id: String = row.get(0)?;
    let applications: String = row.get(6)?;
    let status: String = row.get(7)?;
    let created_at: String = row.get(10)?;
    let updated_at: String = row.get(11)?;
    let serial_source: String = row.get(12)?;

    Ok((|| {
        Ok(YubiKeyRecord {
            id: parse_uuid("keys.id", &id)?,
            serial: row_u32(row, 1),
            model: row_string(row, 2),
            firmware: row_string(row, 3),
            form_factor: row_string(row, 4),
            fips: row_bool(row, 5),
            applications: serde_json::from_str(&applications)?,
            status: key_status_from(&status)?,
            batch: row_string(row, 8),
            notes: row_string(row, 9),
            serial_source: serial_source_from(&serial_source)?,
            created_at: parse_time("keys.created_at", &created_at)?,
            updated_at: parse_time("keys.updated_at", &updated_at)?,
        })
    })())
}

fn row_to_holder(row: &rusqlite::Row<'_>) -> RowResult<Holder> {
    let id: String = row.get(0)?;
    let created_at: String = row.get(6)?;
    Ok((|| {
        Ok(Holder {
            id: parse_uuid("holders.id", &id)?,
            full_name: row_string(row, 1),
            email: row_string(row, 2),
            unit: row_string(row, 3),
            registration: row_string(row, 4),
            active: row_bool(row, 5),
            created_at: parse_time("holders.created_at", &created_at)?,
            identification_number: row_string(row, 7),
            phone: row_string(row, 8),
            address: row_string(row, 9),
        })
    })())
}

fn row_to_incident(row: &rusqlite::Row<'_>) -> RowResult<KeyIncident> {
    let id: String = row.get(0)?;
    let kind: String = row.get(2)?;
    let reported_at: String = row.get(3)?;
    let recorded_at: String = row.get(7)?;
    let closed_at: Option<String> = row.get(9)?;

    Ok((|| {
        Ok(KeyIncident {
            id: parse_uuid("key_incidents.id", &id)?,
            key_serial: row_u32(row, 1),
            kind: incident_kind_from(&kind)?,
            reported_at: parse_time("key_incidents.reported_at", &reported_at)?,
            reported_by: row_string(row, 4),
            holder_display: row_string(row, 5),
            circumstances: row_string(row, 6),
            recorded_at: parse_time("key_incidents.recorded_at", &recorded_at)?,
            recorded_by: row_string(row, 8),
            closed_at: match closed_at {
                Some(raw) => Some(parse_time("key_incidents.closed_at", &raw)?),
                None => None,
            },
            closing_note: row_string(row, 10),
        })
    })())
}

fn row_to_remediation(row: &rusqlite::Row<'_>) -> RowResult<Remediation> {
    let id: String = row.get(0)?;
    let incident_id: Option<String> = row.get(2)?;
    let kind: String = row.get(3)?;
    let recorded_at: String = row.get(7)?;

    Ok((|| {
        Ok(Remediation {
            id: parse_uuid("key_remediations.id", &id)?,
            key_serial: row_u32(row, 1),
            incident_id: match incident_id {
                Some(raw) => Some(parse_uuid("key_remediations.incident_id", &raw)?),
                None => None,
            },
            kind: remediation_kind_from(&kind)?,
            subject: row_string(row, 4),
            reference: row_string(row, 5),
            reason: row_string(row, 6),
            recorded_at: parse_time("key_remediations.recorded_at", &recorded_at)?,
            recorded_by: row_string(row, 8),
            detail: row_string(row, 9),
        })
    })())
}

fn row_to_rma(row: &rusqlite::Row<'_>) -> RowResult<RmaCase> {
    let id: String = row.get(0)?;
    let sent_at: String = row.get(3)?;
    let replacement: Option<i64> = row.get(6)?;
    let replaced_at: Option<String> = row.get(7)?;
    let closed_at: Option<String> = row.get(8)?;

    Ok((|| {
        Ok(RmaCase {
            id: parse_uuid("key_rma.id", &id)?,
            key_serial: row_u32(row, 1),
            reference: row_string(row, 2),
            sent_at: parse_time("key_rma.sent_at", &sent_at)?,
            sent_by: row_string(row, 4),
            fault: row_string(row, 5),
            replacement_serial: replacement.map(|serial| serial as u32),
            replaced_at: match replaced_at {
                Some(raw) => Some(parse_time("key_rma.replaced_at", &raw)?),
                None => None,
            },
            closed_at: match closed_at {
                Some(raw) => Some(parse_time("key_rma.closed_at", &raw)?),
                None => None,
            },
            notes: row_string(row, 9),
        })
    })())
}

fn row_to_distribution(row: &rusqlite::Row<'_>) -> RowResult<DistributionRecord> {
    let id: String = row.get(0)?;
    let key_id: String = row.get(1)?;
    let holder_id: String = row.get(3)?;
    let distributed_at: String = row.get(5)?;
    let method: String = row.get(7)?;
    let run_id: Option<String> = row.get(9)?;
    let returned_at: Option<String> = row.get(10)?;

    Ok((|| {
        Ok(DistributionRecord {
            id: parse_uuid("distributions.id", &id)?,
            key_id: parse_uuid("distributions.key_id", &key_id)?,
            key_serial: row_u32(row, 2),
            holder_id: parse_uuid("distributions.holder_id", &holder_id)?,
            holder_display: row_string(row, 4),
            distributed_at: parse_time("distributions.distributed_at", &distributed_at)?,
            distributed_by: row_string(row, 6),
            method: delivery_from(&method)?,
            receipt_ref: row_string(row, 8),
            bootstrap_run_id: match run_id {
                Some(raw) => Some(parse_uuid("distributions.bootstrap_run_id", &raw)?),
                None => None,
            },
            returned_at: match returned_at {
                Some(raw) => Some(parse_time("distributions.returned_at", &raw)?),
                None => None,
            },
            returned_to: row.get::<_, Option<String>>(11).unwrap_or(None),
            notes: row_string(row, 12),
        })
    })())
}

/// A run without its steps: they are rows of their own since schema v5, and are
/// attached by [`Store::runs`] in one further query.
fn row_to_run(row: &rusqlite::Row<'_>) -> RowResult<BootstrapRun> {
    let id: String = row.get(0)?;
    let holder_id: Option<String> = row.get(2)?;
    let started_at: String = row.get(6)?;
    let finished_at: Option<String> = row.get(7)?;
    let status: String = row.get(8)?;

    Ok((|| {
        Ok(BootstrapRun {
            id: parse_uuid("bootstrap_runs.id", &id)?,
            key_serial: row_u32(row, 1),
            holder_id: match holder_id {
                Some(raw) => Some(parse_uuid("bootstrap_runs.holder_id", &raw)?),
                None => None,
            },
            template_id: row_string(row, 3),
            template_version: row_string(row, 4),
            operator: row_string(row, 5),
            started_at: parse_time("bootstrap_runs.started_at", &started_at)?,
            finished_at: match finished_at {
                Some(raw) => Some(parse_time("bootstrap_runs.finished_at", &raw)?),
                None => None,
            },
            status: serde_json::from_str(&status)?,
            steps: Vec::new(),
            custody: row_string(row, 9),
        })
    })())
}

/// One row of `bootstrap_run_steps`, with the run it belongs to.
fn row_to_run_step(row: &rusqlite::Row<'_>) -> RowResult<(Uuid, StepOutcome)> {
    let run_id: String = row.get(0)?;
    let kind: String = row.get(2)?;
    let status: String = row.get(3)?;
    let started_at: Option<String> = row.get(4)?;
    let finished_at: Option<String> = row.get(5)?;

    Ok((|| {
        Ok((
            parse_uuid("bootstrap_run_steps.run_id", &run_id)?,
            StepOutcome {
                step_id: row_string(row, 1),
                kind: step_kind_from(&kind)?,
                status: step_status_from(&status)?,
                started_at: match started_at {
                    Some(raw) => Some(parse_time("bootstrap_run_steps.started_at", &raw)?),
                    None => None,
                },
                finished_at: match finished_at {
                    Some(raw) => Some(parse_time("bootstrap_run_steps.finished_at", &raw)?),
                    None => None,
                },
                detail: row_string(row, 6),
            },
        ))
    })())
}

fn row_to_document(row: &rusqlite::Row<'_>) -> RowResult<AttachedDocument> {
    let id: String = row.get(0)?;
    let distribution_id: String = row.get(1)?;
    let kind: String = row.get(2)?;
    let uploaded_at: String = row.get(7)?;

    Ok((|| {
        Ok(AttachedDocument {
            id: parse_uuid("documents.id", &id)?,
            distribution_id: parse_uuid("documents.distribution_id", &distribution_id)?,
            kind: document_kind_from(&kind)?,
            filename: row_string(row, 3),
            media_type: row_string(row, 4),
            size_bytes: row.get::<_, i64>(5).unwrap_or_default() as usize,
            sha256: row_string(row, 6),
            uploaded_at: parse_time("documents.uploaded_at", &uploaded_at)?,
            uploaded_by: row_string(row, 8),
            content: None,
        })
    })())
}

fn row_to_audit(row: &rusqlite::Row<'_>) -> RowResult<AuditEntry> {
    let at: String = row.get(1)?;
    Ok((|| {
        Ok(AuditEntry {
            seq: row.get::<_, i64>(0).unwrap_or_default() as u64,
            at: parse_time("audit.at", &at)?,
            actor: row_string(row, 2),
            event: row_string(row, 3),
            target: row_string(row, 4),
            details: row_string(row, 5),
            prev_hash: row_string(row, 6),
            hash: row_string(row, 7),
        })
    })())
}

fn row_string(row: &rusqlite::Row<'_>, index: usize) -> String {
    row.get::<_, String>(index).unwrap_or_default()
}

fn row_u32(row: &rusqlite::Row<'_>, index: usize) -> u32 {
    row.get::<_, i64>(index).unwrap_or_default() as u32
}

fn row_bool(row: &rusqlite::Row<'_>, index: usize) -> bool {
    row.get::<_, i64>(index).unwrap_or_default() != 0
}

/// Schema v1. Audit immutability is enforced here, by the database.
const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS keys (
    id            TEXT PRIMARY KEY,
    serial        INTEGER NOT NULL UNIQUE,
    model         TEXT NOT NULL,
    firmware      TEXT NOT NULL,
    form_factor   TEXT NOT NULL DEFAULT '',
    fips          INTEGER NOT NULL DEFAULT 0,
    applications  TEXT NOT NULL DEFAULT '[]',
    status        TEXT NOT NULL,
    batch         TEXT NOT NULL DEFAULT '',
    notes         TEXT NOT NULL DEFAULT '',
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS holders (
    id            TEXT PRIMARY KEY,
    full_name     TEXT NOT NULL,
    email         TEXT NOT NULL UNIQUE,
    unit          TEXT NOT NULL DEFAULT '',
    registration  TEXT NOT NULL DEFAULT '',
    active        INTEGER NOT NULL DEFAULT 1,
    created_at    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS bootstrap_runs (
    id               TEXT PRIMARY KEY,
    key_serial       INTEGER NOT NULL,
    holder_id        TEXT REFERENCES holders(id),
    template_id      TEXT NOT NULL,
    template_version TEXT NOT NULL,
    operator         TEXT NOT NULL,
    started_at       TEXT NOT NULL,
    finished_at      TEXT,
    status           TEXT NOT NULL,
    steps            TEXT NOT NULL DEFAULT '[]',
    custody          TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS distributions (
    id               TEXT PRIMARY KEY,
    key_id           TEXT NOT NULL REFERENCES keys(id),
    key_serial       INTEGER NOT NULL,
    holder_id        TEXT NOT NULL REFERENCES holders(id),
    holder_display   TEXT NOT NULL,
    distributed_at   TEXT NOT NULL,
    distributed_by   TEXT NOT NULL,
    method           TEXT NOT NULL,
    receipt_ref      TEXT NOT NULL DEFAULT '',
    bootstrap_run_id TEXT REFERENCES bootstrap_runs(id),
    returned_at      TEXT,
    returned_to      TEXT,
    notes            TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS idx_distributions_serial ON distributions(key_serial);
CREATE INDEX IF NOT EXISTS idx_distributions_holder ON distributions(holder_id);
CREATE INDEX IF NOT EXISTS idx_runs_serial ON bootstrap_runs(key_serial);

CREATE TABLE IF NOT EXISTS templates (
    id          TEXT NOT NULL,
    version     TEXT NOT NULL,
    name        TEXT NOT NULL,
    body        TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (id, version)
);

-- Append-only by database restriction, not by application discipline.
CREATE TABLE IF NOT EXISTS audit (
    seq        INTEGER PRIMARY KEY,
    at         TEXT NOT NULL,
    actor      TEXT NOT NULL,
    event      TEXT NOT NULL,
    target     TEXT NOT NULL DEFAULT '',
    details    TEXT NOT NULL DEFAULT '',
    prev_hash  TEXT NOT NULL,
    hash       TEXT NOT NULL
);

CREATE TRIGGER IF NOT EXISTS audit_no_update
BEFORE UPDATE ON audit
BEGIN
    SELECT RAISE(ABORT, 'audit trail is append-only');
END;

CREATE TRIGGER IF NOT EXISTS audit_no_delete
BEFORE DELETE ON audit
BEGIN
    SELECT RAISE(ABORT, 'audit trail is append-only');
END;
"#;

/// Schema v2 — records **how** a serial was learned.
///
/// A serial read from the device is verified; one read from a box label, or
/// typed by hand, is a claim about a key nobody has touched yet. Reports and the
/// inventory badge have to tell those apart, so the provenance is a column
/// rather than a convention. Every row that existed before this migration came
/// from a device read, which is why `device` is the default.
const MIGRATE_V2: &str = r#"
ALTER TABLE keys ADD COLUMN serial_source TEXT NOT NULL DEFAULT 'device';
CREATE INDEX IF NOT EXISTS idx_keys_serial_source ON keys(serial_source);
"#;

/// Schema v3 — the consignment term.
///
/// Three additions, all driven by the term: the optional holder fields it prints
/// (identification number, phone, address), the multilingual term templates, and
/// the signed document itself.
///
/// The signed term is stored **in** the database rather than as a path, because
/// the database is the unit of deployment: a path reference breaks the moment the
/// file moves to a share, and the signed term is the evidence that makes a
/// distribution record worth keeping. It is also personal data, which is one more
/// reason to turn the password on.
const MIGRATE_V3: &str = r#"
ALTER TABLE holders ADD COLUMN identification_number TEXT NOT NULL DEFAULT '';
ALTER TABLE holders ADD COLUMN phone TEXT NOT NULL DEFAULT '';
ALTER TABLE holders ADD COLUMN address TEXT NOT NULL DEFAULT '';

CREATE TABLE IF NOT EXISTS term_templates (
    id          TEXT NOT NULL,
    language    TEXT NOT NULL,
    version     TEXT NOT NULL,
    title       TEXT NOT NULL,
    body        TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (id, language, version)
);

CREATE TABLE IF NOT EXISTS documents (
    id              TEXT PRIMARY KEY,
    distribution_id TEXT NOT NULL REFERENCES distributions(id),
    kind            TEXT NOT NULL,
    filename        TEXT NOT NULL,
    media_type      TEXT NOT NULL,
    size_bytes      INTEGER NOT NULL,
    sha256          TEXT NOT NULL,
    uploaded_at     TEXT NOT NULL,
    uploaded_by     TEXT NOT NULL,
    content         BLOB NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_documents_distribution ON documents(distribution_id);
"#;

/// Schema v4 — a bootstrap template can be **retired**.
///
/// The Templates screen lets an operator add, edit and remove a procedure, and
/// "remove" has two meanings that must not be confused. A template no run refers
/// to is deleted outright. One that a run *does* refer to cannot be: a run saying
/// it applied `org-standard v1`, with no `org-standard v1` to look up, is not a
/// record. Such a version is retired instead — withdrawn from the wizard, kept in
/// the database — and this column is where that decision lives.
///
/// It is a column rather than a field in the template body because the body is
/// the *template format*, shared with the future import/export, while retirement
/// is this deployment's opinion about a template. It also has to be queryable:
/// `templates()` returns what the wizard may offer, which is the rows where this
/// is NULL. Every row that existed before this migration was in use, which is why
/// the default is NULL.
const MIGRATE_V4: &str = r#"
ALTER TABLE templates ADD COLUMN retired_at TEXT;
"#;

/// Schema v5 — a bootstrap run's steps become **rows**, not a JSON blob.
///
/// Until now the step outcomes lived in `bootstrap_runs.steps` as serialised
/// JSON. That was the right shape while a run was written once and read back
/// whole, and it is the wrong shape for everything Wave 1 needs:
///
/// * **Step-level reporting.** "How many keys got a signing certificate this
///   quarter?" is a `WHERE kind = 'piv-cert-import' AND status = 'done'` over
///   rows, and a full-table scan plus a JSON parse per run over a blob.
/// * **The executor writes one step at a time.** A blob has to be rewritten
///   whole on every step outcome, so a run interrupted mid-write loses the
///   steps that had already succeeded — exactly the record that matters when a
///   key was half-configured.
/// * **The enum names leak the Rust type.** The blob stored serde's variant
///   names (`Fido2Pin`, `Done`); a column stores the same readable strings the
///   rest of this schema uses (`fido2-pin`, `done`), which is what makes the
///   file answerable from a SQL console during an audit.
///
/// The backfill is done in Rust rather than in SQL ([`Store::backfill_run_steps`]):
/// mapping `Fido2Pin` to `fido2-pin` in SQL would mean a twelve-branch `CASE`
/// hand-kept in step with [`StepKind::slug`], and the first divergence would be
/// silent. The old column is dropped once its contents are rows, so there is one
/// source of truth and not two.
const MIGRATE_V5_CREATE: &str = r#"
CREATE TABLE IF NOT EXISTS bootstrap_run_steps (
    run_id      TEXT NOT NULL REFERENCES bootstrap_runs(id),
    position    INTEGER NOT NULL,
    step_id     TEXT NOT NULL,
    kind        TEXT NOT NULL,
    status      TEXT NOT NULL,
    started_at  TEXT,
    finished_at TEXT,
    detail      TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (run_id, position)
);

CREATE INDEX IF NOT EXISTS idx_run_steps_kind ON bootstrap_run_steps(kind, status);
"#;

/// The second half of v5, run only once the blob has been turned into rows.
const MIGRATE_V5_DROP: &str = r#"
ALTER TABLE bootstrap_runs DROP COLUMN steps;
"#;

/// Schema v6 — what happens to a key **after** the hand-over
/// (`features/key-lifecycle-and-revocation.md` phases 2, 3, 4, 6 and 8).
///
/// Three tables, and the reason there are three rather than one is that they
/// answer three different questions an audit asks:
///
/// * `key_incidents` — *what happened, who said so, and when.* One row per report;
///   a key can be lost, recovered and lost again, and flattening that into columns
///   on `keys` would overwrite the first report with the second.
/// * `key_remediations` — *and what was done about it.* One row per obligation
///   met: the certificate revoked at its CA, the credential removed at its relying
///   party, the applets returned to factory default. One table for the three
///   because they share a shape — a subject, a reference somebody else can check,
///   and who recorded it — and because "everything that has been done to this key"
///   is then one query rather than a union of three.
/// * `key_rma` — *where the hardware physically went, and what replaced it.* The
///   replacement is a serial, not a copy of the row: the new key keeps its own
///   inventory row, its own hand-overs and its own runs.
///
/// **What is deliberately not a table**: the *dependency list* — what a key was
/// carrying. That is read back out of the bootstrap run's step details
/// ([`crate::domain::lifecycle::dependencies`]), which is where every other piece
/// of run evidence lives. Storing it would create a second truth about what a run
/// did, and would leave every register written before this migration with an empty
/// list; derived, a register from v1 answers in full.
///
/// Nothing here is nullable-by-accident: `closed_at`, `replacement_serial`,
/// `replaced_at` and `closed_at` on the RMA are the four genuinely-unknown facts,
/// and each is `NULL` until somebody knows it.
const MIGRATE_V6: &str = r#"
CREATE TABLE IF NOT EXISTS key_incidents (
    id              TEXT PRIMARY KEY,
    key_serial      INTEGER NOT NULL,
    kind            TEXT NOT NULL,
    reported_at     TEXT NOT NULL,
    reported_by     TEXT NOT NULL,
    holder_display  TEXT NOT NULL DEFAULT '',
    circumstances   TEXT NOT NULL DEFAULT '',
    recorded_at     TEXT NOT NULL,
    recorded_by     TEXT NOT NULL DEFAULT '',
    closed_at       TEXT,
    closing_note    TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS idx_incidents_serial ON key_incidents(key_serial);

CREATE TABLE IF NOT EXISTS key_remediations (
    id           TEXT PRIMARY KEY,
    key_serial   INTEGER NOT NULL,
    incident_id  TEXT REFERENCES key_incidents(id),
    kind         TEXT NOT NULL,
    subject      TEXT NOT NULL,
    reference    TEXT NOT NULL DEFAULT '',
    reason       TEXT NOT NULL DEFAULT '',
    recorded_at  TEXT NOT NULL,
    recorded_by  TEXT NOT NULL DEFAULT '',
    detail       TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS idx_remediations_serial ON key_remediations(key_serial, kind);

CREATE TABLE IF NOT EXISTS key_rma (
    id                  TEXT PRIMARY KEY,
    key_serial          INTEGER NOT NULL,
    reference           TEXT NOT NULL,
    sent_at             TEXT NOT NULL,
    sent_by             TEXT NOT NULL DEFAULT '',
    fault               TEXT NOT NULL DEFAULT '',
    replacement_serial  INTEGER,
    replaced_at         TEXT,
    closed_at           TEXT,
    notes               TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS idx_rma_serial ON key_rma(key_serial);
"#;
