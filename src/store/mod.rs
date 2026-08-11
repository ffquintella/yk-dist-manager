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

pub mod cloud;
pub mod smb;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

pub use cloud::{LeaseError, LeaseHolder, Renewal, Settled, SyncLease, SyncPolicy};

use crate::audit::{AuditEntry, GENESIS};
use crate::domain::{AttachedDocument, DocumentKind};
use crate::domain::{
    BootstrapRun, DeliveryMethod, DistributionRecord, Holder, KeyStatus, SerialSource, StepOutcome,
    YubiKeyRecord,
};
use crate::template::{BootstrapTemplate, StoredTemplate};
use crate::term::TermTemplate;

/// Current schema version, tracked in `PRAGMA user_version`.
pub const SCHEMA_VERSION: i64 = 4;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
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
}

pub type Result<T> = std::result::Result<T, StoreError>;

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
        let store = Self::open(config)?;
        store.seed_builtin_templates()?;
        Ok(store)
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

        let store = Self {
            conn,
            path,
            location: config.location,
            encrypted: config.password.is_some(),
            lease,
            sync: config.sync,
        };
        store.apply_pragmas()?;
        store.migrate()?;

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

        self.conn
            .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tracing::info!(event = "db.migrated", from = version, to = SCHEMA_VERSION);
        Ok(())
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
        self.conn.execute(
            "UPDATE keys SET status = ?1, updated_at = ?2 WHERE serial = ?3",
            params![key_status_str(next), Utc::now().to_rfc3339(), serial],
        )?;
        Ok(())
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

    // ---------------------------------------------------------- bootstrap runs

    pub fn insert_run(&self, run: &BootstrapRun) -> Result<()> {
        self.conn.execute(
            "INSERT INTO bootstrap_runs (id, key_serial, holder_id, template_id, template_version,
                                         operator, started_at, finished_at, status, steps, custody)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
                 finished_at = excluded.finished_at,
                 status = excluded.status,
                 steps = excluded.steps,
                 custody = excluded.custody",
            params![
                run.id.to_string(),
                run.key_serial,
                run.holder_id.map(|id| id.to_string()),
                run.template_id,
                run.template_version,
                run.operator,
                run.started_at.to_rfc3339(),
                run.finished_at.map(|at| at.to_rfc3339()),
                serde_json::to_string(&run.status)?,
                serde_json::to_string(&run.steps)?,
                run.custody,
            ],
        )?;
        Ok(())
    }

    pub fn runs(&self) -> Result<Vec<BootstrapRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, key_serial, holder_id, template_id, template_version, operator,
                    started_at, finished_at, status, steps, custody
             FROM bootstrap_runs ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_run)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
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
        Ok(entry)
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

    /// Verify the whole chain; returns the number of entries checked.
    pub fn verify_audit(&self) -> Result<usize> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, at, actor, event, target, details, prev_hash, hash
             FROM audit ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map([], row_to_audit)?;
        let entries = rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        crate::audit::verify(&entries).map_err(|e| StoreError::Decode {
            column: "audit",
            value: e.to_string(),
        })?;
        Ok(entries.len())
    }
}

/// Apply `PRAGMA key`. Only meaningful in a SQLCipher build.
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

fn row_to_run(row: &rusqlite::Row<'_>) -> RowResult<BootstrapRun> {
    let id: String = row.get(0)?;
    let holder_id: Option<String> = row.get(2)?;
    let started_at: String = row.get(6)?;
    let finished_at: Option<String> = row.get(7)?;
    let status: String = row.get(8)?;
    let steps: String = row.get(9)?;

    Ok((|| {
        let steps: Vec<StepOutcome> = serde_json::from_str(&steps)?;
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
            steps,
            custody: row_string(row, 10),
        })
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
