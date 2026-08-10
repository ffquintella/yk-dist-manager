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

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::audit::{AuditEntry, GENESIS};
use crate::domain::{
    BootstrapRun, DeliveryMethod, DistributionRecord, Holder, KeyStatus, StepOutcome, YubiKeyRecord,
};
use crate::template::BootstrapTemplate;

/// Current schema version, tracked in `PRAGMA user_version`.
pub const SCHEMA_VERSION: i64 = 1;

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
    #[error("record not found: {0}")]
    NotFound(String),
    #[error("serialisation error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Where the database file lives, which decides the locking strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// Local disk: WAL, fast and crash-safe.
    LocalDisk,
    /// SMB/NFS/AFP share: rollback journal, full sync, long busy timeout.
    NetworkShare,
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
        if looks_remote {
            Location::NetworkShare
        } else {
            Location::LocalDisk
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Location::LocalDisk => "local disk (WAL)",
            Location::NetworkShare => "network share (rollback journal)",
        }
    }
}

/// How to open the database.
#[derive(Debug, Clone)]
pub struct StoreConfig {
    pub path: PathBuf,
    /// `None` = unencrypted file.
    pub password: Option<String>,
    pub location: Location,
}

impl StoreConfig {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let location = Location::detect(&path);
        Self {
            path,
            password: None,
            location,
        }
    }

    pub fn with_password(mut self, password: Option<String>) -> Self {
        self.password = password.filter(|p| !p.is_empty());
        self
    }
}

/// Open handle to the single-file database.
pub struct Store {
    conn: Connection,
    path: PathBuf,
    location: Location,
    encrypted: bool,
}

impl Store {
    /// `$YKDM_DB`, else the per-user data directory.
    pub fn default_path() -> PathBuf {
        if let Ok(explicit) = std::env::var("YKDM_DB")
            && !explicit.trim().is_empty()
        {
            return PathBuf::from(explicit);
        }
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".into());
        let mut path = PathBuf::from(home);
        if cfg!(target_os = "macos") {
            path.push("Library/Application Support/yk-dist-manager");
        } else if cfg!(target_os = "windows") {
            path.push("AppData/Roaming/yk-dist-manager");
        } else {
            path.push(".local/share/yk-dist-manager");
        }
        path.push("yk-dist-manager.sqlite3");
        path
    }

    /// Open (creating if needed), apply pragmas, migrate.
    pub fn open(config: &StoreConfig) -> Result<Self> {
        if let Some(parent) = config.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(&config.path)?;
        Self::finish_open(conn, config.path.clone(), config)
    }

    /// In-memory database, for tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let config = StoreConfig {
            path: PathBuf::from(":memory:"),
            password: None,
            location: Location::LocalDisk,
        };
        Self::finish_open(conn, config.path.clone(), &config)
    }

    fn finish_open(conn: Connection, path: PathBuf, config: &StoreConfig) -> Result<Self> {
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
        };
        store.apply_pragmas()?;
        store.migrate()?;
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
            Location::NetworkShare => {
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
            "{} — {}{}",
            self.path.display(),
            self.location.label(),
            if self.encrypted {
                ", password-protected"
            } else {
                ", no password"
            }
        )
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
                               status, batch, notes, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(serial) DO UPDATE SET
                 model = excluded.model,
                 firmware = excluded.firmware,
                 form_factor = excluded.form_factor,
                 fips = excluded.fips,
                 applications = excluded.applications,
                 batch = excluded.batch,
                 notes = excluded.notes,
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
                record.created_at.to_rfc3339(),
                record.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn keys(&self) -> Result<Vec<YubiKeyRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, serial, model, firmware, form_factor, fips, applications, status,
                    batch, notes, created_at, updated_at
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
                        batch, notes, created_at, updated_at
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

    // --------------------------------------------------------------- holders

    pub fn insert_holder(&self, holder: &Holder) -> Result<()> {
        self.conn.execute(
            "INSERT INTO holders (id, full_name, email, unit, registration, active, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(email) DO UPDATE SET
                 full_name = excluded.full_name,
                 unit = excluded.unit,
                 registration = excluded.registration,
                 active = excluded.active",
            params![
                holder.id.to_string(),
                holder.full_name,
                holder.email,
                holder.unit,
                holder.registration,
                holder.active as i64,
                holder.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn holders(&self) -> Result<Vec<Holder>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, full_name, email, unit, registration, active, created_at
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

    pub fn templates(&self) -> Result<Vec<BootstrapTemplate>> {
        let mut stmt = self
            .conn
            .prepare("SELECT body FROM templates ORDER BY id, version")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for body in rows {
            out.push(serde_json::from_str(&body?)?);
        }
        Ok(out)
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

pub fn key_status_str(status: KeyStatus) -> &'static str {
    match status {
        KeyStatus::InStock => "in_stock",
        KeyStatus::Bootstrapped => "bootstrapped",
        KeyStatus::Distributed => "distributed",
        KeyStatus::Returned => "returned",
        KeyStatus::Lost => "lost",
        KeyStatus::Retired => "retired",
    }
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
