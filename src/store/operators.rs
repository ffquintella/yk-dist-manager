//! Who may use this register, and what the database refuses them
//! (`features/operator-auth-and-roles.md` phases 1, 2, 3 and 7).
//!
//! # Why the refusal is here and not in `src/ui/`
//!
//! The specification is explicit: *enforce in `Store`, not only in the UI*, so
//! that a UI bug cannot bypass authorisation. A screen that hides a button an
//! auditor may not press is good manners; it is not a control, because the button
//! is not what writes to the register.
//!
//! Two layers, and they cover different things:
//!
//! 1. **A SQLite authorizer on the connection** ([`Store::install_authorizer`]).
//!    Every statement is checked as it is prepared, so a role that may not write
//!    `templates` is refused by the database itself. This is the same reasoning
//!    that made read-only mode a connection flag rather than a guard in each of
//!    the twenty-odd methods that write: *a guard per method could be forgotten
//!    by the next mutation added; a connection that is not allowed to write
//!    cannot be.* A mutation written next year is covered without anybody
//!    remembering to cover it.
//! 2. **[`Store::require`]**, for what is not a table write at all. Resetting an
//!    applet writes to a piece of hardware; changing the database password
//!    re-keys a file; an export takes personal data out of the register. None of
//!    those is an `INSERT`, and none of them may be done by an auditor.
//!
//! # The failure counter is a separate table on purpose
//!
//! See `MIGRATE_V9` in the parent module: `operator_sign_ins` holds everything the
//! sign-in mechanism writes about itself, and it must be writable by a session
//! that has **not** authenticated — while somebody is signing in, that is the only
//! session there is. It is keyed on the username that was **typed**, so that a
//! lockout cannot become an oracle for who is on the register.
//!
//! # Nothing here handles a password for longer than one call
//!
//! A password arrives as a `&str`, reaches [`crate::operator::credential`], and
//! is gone. It is in no column, no log line, no audit entry and no error message
//! (`AGENTS.md` §2). A failed sign-in is audited as
//! `operator.login.failed` carrying the reason and the attempt count, and nothing
//! that was typed — not even a length.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use super::{Result, Store, StoreError};
use crate::operator::lockout::Lockout;
use crate::operator::{
    Action, AuthMethod, Authority, NewOperator, Operator, RegisteredCredential, Role, Session,
    credential, normalise_username,
};

/// Why a sign-in did not happen.
///
/// The reason reaches the audit entry, and every variant is safe to write there:
/// none of them quotes anything that was typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInRefusal {
    /// No such operator, or the wrong password. **One variant for both**, because
    /// telling them apart on screen tells an attacker which usernames exist.
    Credential,
    /// The account exists and is disabled.
    Disabled,
    /// Too many recent failures.
    LockedOut,
    /// The account has no credential of the kind that was offered.
    NoSuchMethod,
    /// The key answered, but not for the credential this register registered.
    WrongCredential,
    /// The authenticator did not verify the user — no PIN, no biometric.
    NotUserVerified,
    /// The signature counter did not advance, which is what a cloned
    /// authenticator looks like.
    CounterReplay,
}

impl SignInRefusal {
    pub fn slug(&self) -> &'static str {
        match self {
            SignInRefusal::Credential => "bad-credential",
            SignInRefusal::Disabled => "account-disabled",
            SignInRefusal::LockedOut => "locked-out",
            SignInRefusal::NoSuchMethod => "no-such-method",
            SignInRefusal::WrongCredential => "wrong-credential",
            SignInRefusal::NotUserVerified => "not-user-verified",
            SignInRefusal::CounterReplay => "counter-replay",
        }
    }

    /// What the operator is told.
    ///
    /// Deliberately identical for "no such account" and "wrong password": the
    /// screen must not answer the question *is there an account called ana*.
    pub fn message(&self) -> &'static str {
        match self {
            SignInRefusal::Credential | SignInRefusal::NoSuchMethod => {
                "that sign-in was not accepted"
            }
            SignInRefusal::Disabled => {
                "that account is disabled — an administrator can enable it again"
            }
            SignInRefusal::LockedOut => "too many recent failures — wait, or ask an administrator",
            SignInRefusal::WrongCredential => {
                "that security key is not the one registered for this account"
            }
            SignInRefusal::NotUserVerified => {
                "the security key did not verify you — a PIN or a fingerprint is required, not \
                 just a touch"
            }
            SignInRefusal::CounterReplay => {
                "the security key's signature counter did not advance, which is what a cloned \
                 authenticator looks like. Report this before using the key again"
            }
        }
    }
}

/// What a FIDO2 authentication produced, as this register needs to check it.
///
/// A plain struct rather than the transport's own type so that `store` does not
/// depend on `device`: the dependency direction runs downward only
/// (`AGENTS.md`, architecture map), and the caller in `app` is the one that has
/// both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssertedCredential {
    pub credential_id_hex: String,
    pub relying_party: String,
    pub serial: u32,
    pub user_verified: bool,
    pub counter: u32,
}

impl Store {
    // ------------------------------------------------------------- authority

    /// Install the connection's authorizer.
    ///
    /// Called **once**, when the register is opened, rather than on every role
    /// change: the callback reads one relaxed load from the shared atomic, so
    /// changing role is a store rather than an FFI call, and there is never a
    /// window in which no authorizer is installed.
    pub(super) fn install_authorizer(&self) -> Result<()> {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

        let authority = std::sync::Arc::clone(&self.authority);
        self.conn.authorizer(Some(move |context: AuthContext<'_>| {
            let authority =
                Authority::from_code(authority.load(std::sync::atomic::Ordering::Relaxed));
            let table = match context.action {
                AuthAction::Insert { table_name }
                | AuthAction::Delete { table_name }
                | AuthAction::Update { table_name, .. }
                | AuthAction::DropTable { table_name }
                | AuthAction::AlterTable { table_name, .. }
                | AuthAction::CreateTable { table_name }
                | AuthAction::CreateIndex { table_name, .. }
                | AuthAction::DropIndex { table_name, .. }
                | AuthAction::CreateTrigger { table_name, .. }
                | AuthAction::DropTrigger { table_name, .. } => table_name,
                // Everything else — reads, transactions, pragmas, functions — is
                // allowed. Reads deliberately so: an auditor reads everything by
                // definition, and a signed-out session has to be able to name the
                // register it is signing in to.
                _ => return Authorization::Allow,
            };
            if authority.may_write_table(table) {
                Authorization::Allow
            } else {
                // `Deny`, not `Ignore`: a refusal that silently did nothing is
                // exactly the failure mode this whole layer exists to avoid.
                Authorization::Deny
            }
        }))?;
        Ok(())
    }

    /// Act as this authority from now on.
    ///
    /// Clears any live re-verification, because a change of who is at the
    /// keyboard is the one moment the tool must not carry one forward.
    pub fn act_as(&self, authority: Authority) {
        self.authority
            .store(authority.code(), std::sync::atomic::Ordering::Relaxed);
        self.reverified_until.set(None);
    }

    /// What this connection is currently acting as.
    pub fn authority(&self) -> Authority {
        Authority::from_code(self.authority.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Refuse an action this role may not take.
    ///
    /// The half of the authorisation that a table name cannot express. Callers in
    /// `app` put this **before** the hardware write or the file write, not after:
    /// a factory reset refused once the applet is gone is not a refusal.
    pub fn require(&self, action: Action) -> Result<()> {
        let authority = self.authority();
        if !authority.may(action) {
            return Err(StoreError::Forbidden {
                who: authority.who().to_owned(),
                what: action.describe().to_owned(),
            });
        }
        if action.needs_reverification() && !self.is_reverified_at(Utc::now()) {
            return Err(StoreError::NeedsReverification {
                what: action.describe().to_owned(),
            });
        }
        Ok(())
    }

    /// The re-verification half of [`Store::require`], on its own.
    ///
    /// For a write the authorizer **already** covers by table name — a procedure,
    /// a term, the operator list. The role refusal is deliberately left to
    /// SQLite: it refuses the statement whether or not this line is here, it is
    /// the layer a mutation written next year inherits without anybody
    /// remembering, and `tests/unit_store_operators.rs` asserts that it is what
    /// speaks. What SQLite cannot express is a **live credential**, because that
    /// is a property of the session and not of the statement, so that is the one
    /// thing checked here (`features/operator-auth-and-roles.md` phase 5).
    ///
    /// Use [`Store::require`] instead wherever the operation is not a table write
    /// at all, and therefore has no authorizer behind it.
    pub fn require_fresh_credential(&self, action: Action) -> Result<()> {
        if !self.authority().may(action) {
            return Ok(());
        }
        if action.needs_reverification() && !self.is_reverified_at(Utc::now()) {
            return Err(StoreError::NeedsReverification {
                what: action.describe().to_owned(),
            });
        }
        Ok(())
    }

    /// The credential was presented again (`features/operator-auth-and-roles.md`
    /// phase 5).
    pub fn mark_reverified(&self, now: DateTime<Utc>) {
        self.reverified_until.set(Some(
            now + chrono::Duration::from_std(crate::operator::session::REVERIFICATION_WINDOW)
                .expect("a sane window"),
        ));
    }

    /// Is a sensitive operation currently authorised?
    ///
    /// Always true while the register is unenrolled: in that state the control is
    /// off, and pretending otherwise would refuse work on every register written
    /// before this release.
    pub fn is_reverified_at(&self, now: DateTime<Utc>) -> bool {
        if matches!(self.authority(), Authority::Unenrolled) {
            return true;
        }
        self.reverified_until.get().is_some_and(|until| now < until)
    }

    // -------------------------------------------------------------- the list

    /// Every operator, by username.
    pub fn operators(&self) -> Result<Vec<Operator>> {
        let mut statement = self.conn.prepare(OPERATOR_COLUMNS)?;
        let rows = statement.query_map([], |row| Ok(read_operator(row)))?;
        let mut operators = Vec::new();
        for row in rows {
            operators.push(row??);
        }
        Ok(operators)
    }

    /// How many operators this register has. Zero means unenrolled.
    pub fn operator_count(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT count(*) FROM operators", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Which authority a freshly opened register should act as.
    ///
    /// The whole first-run answer in one call: no operators means the control is
    /// off, and any operator at all means nobody is signed in yet.
    pub fn opening_authority(&self) -> Result<Authority> {
        Ok(if self.operator_count()? == 0 {
            Authority::Unenrolled
        } else {
            Authority::SignedOut
        })
    }

    pub fn operator_by_username(&self, username: &str) -> Result<Option<Operator>> {
        let username = username.trim().to_lowercase();
        self.conn
            .query_row(
                &format!("{OPERATOR_COLUMNS} WHERE o.username = ?1"),
                params![username],
                |row| Ok(read_operator(row)),
            )
            .optional()?
            .transpose()
    }

    pub fn operator_by_id(&self, id: Uuid) -> Result<Option<Operator>> {
        self.conn
            .query_row(
                &format!("{OPERATOR_COLUMNS} WHERE o.id = ?1"),
                params![id.to_string()],
                |row| Ok(read_operator(row)),
            )
            .optional()?
            .transpose()
    }

    /// Read the row that holds the Argon2id string.
    ///
    /// `pub(crate)` and nowhere near a struct the GUI can reach: the PHC string is
    /// not a secret, but the fewer places it can be printed from the better, and
    /// there is no field on [`Operator`] that could carry it into a `Debug` line.
    fn password_phc(&self, id: Uuid) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT password_phc FROM operators WHERE id = ?1",
                params![id.to_string()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    // ------------------------------------------------------------ enrolment

    /// Create the register's **first** administrator
    /// (`features/operator-auth-and-roles.md` phase 7).
    ///
    /// The deliberate, audited first-run path, and the reason an existing register
    /// cannot be locked out by this feature. It is available only while the
    /// register has no operators at all, and refuses the moment one exists — after
    /// that, enrolling is an administrator's job through
    /// [`Store::enrol_operator`], which the authorizer will not let anybody else
    /// do.
    ///
    /// The role is not a parameter. A first operator who was not an administrator
    /// would leave a register with authentication switched on and nobody able to
    /// enrol the second person, which is the same lockout by a longer route.
    pub fn enrol_first_administrator(
        &self,
        username: &str,
        display_name: &str,
        password: &str,
        by: &str,
    ) -> Result<Operator> {
        if self.operator_count()? > 0 {
            return Err(StoreError::Forbidden {
                who: "the first-run path".to_owned(),
                what: "enrol another operator — this register already has one, so an \
                       administrator has to do it"
                    .to_owned(),
            });
        }
        let operator = self.insert_operator(
            &NewOperator {
                username: username.to_owned(),
                display_name: display_name.to_owned(),
                role: Role::Administrator,
            },
            Some(password),
            by,
        )?;
        self.append_audit(
            by,
            "operator.enrolled",
            &format!("operator:{}", operator.username),
            &format!(
                "role={} method=local first=true detail=the first administrator of this register; \
                 authorisation is now in force",
                operator.role.slug()
            ),
        )?;
        tracing::info!(
            event = "operator.enrolled",
            operator = operator.username.as_str(),
            role = operator.role.slug(),
            first = true
        );
        Ok(operator)
    }

    /// Enrol an operator. Administrators only, and re-verified.
    pub fn enrol_operator(
        &self,
        new: &NewOperator,
        password: Option<&str>,
        by: &str,
    ) -> Result<Operator> {
        self.require(Action::ManageOperators)?;
        let operator = self.insert_operator(new, password, by)?;
        self.append_audit(
            by,
            "operator.enrolled",
            &format!("operator:{}", operator.username),
            &format!(
                "role={} method={} first=false",
                operator.role.slug(),
                if password.is_some() { "local" } else { "none" }
            ),
        )?;
        tracing::info!(
            event = "operator.enrolled",
            operator = operator.username.as_str(),
            role = operator.role.slug()
        );
        Ok(operator)
    }

    fn insert_operator(
        &self,
        new: &NewOperator,
        password: Option<&str>,
        by: &str,
    ) -> Result<Operator> {
        let username = normalise_username(&new.username)
            .map_err(|e| StoreError::WeakPassword(e.to_string()))?;
        let display_name = crate::domain::require_text("display name", &new.display_name)
            .map_err(|e| StoreError::WeakPassword(e.to_string()))?;

        if self.operator_by_username(&username)?.is_some() {
            return Err(StoreError::AlreadyExists(std::path::PathBuf::from(
                username,
            )));
        }

        // The same floor as the database password, and enforced here rather than
        // by the screen for the same reason it is there: a policy only the widget
        // applies is a policy every other caller skips.
        let phc = match password {
            Some(password) => {
                let assessment = crate::password::assess(password);
                if !assessment.is_acceptable() {
                    return Err(StoreError::WeakPassword(assessment.summary()));
                }
                Some(
                    credential::hash(password)
                        .map_err(|e| StoreError::WeakPassword(e.to_string()))?,
                )
            }
            None => None,
        };

        let now = Utc::now();
        let id = Uuid::new_v4();
        self.conn.execute(
            "INSERT INTO operators
                 (id, username, display_name, role, active, created_at, created_by, updated_at,
                  password_phc)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?5, ?7)",
            params![
                id.to_string(),
                username,
                display_name,
                new.role.slug(),
                now.to_rfc3339(),
                by,
                phc,
            ],
        )?;

        Ok(Operator {
            id,
            username,
            display_name,
            role: new.role,
            active: true,
            created_at: now,
            created_by: by.to_owned(),
            updated_at: now,
            has_password: phc.is_some(),
            credential: None,
            last_login_at: None,
        })
    }

    /// Move an operator to another role. Audited with both roles and who did it.
    pub fn set_operator_role(&self, id: Uuid, role: Role, by: &str) -> Result<()> {
        self.require(Action::ManageOperators)?;
        let operator = self
            .operator_by_id(id)?
            .ok_or_else(|| StoreError::NotFound(format!("operator {id}")))?;
        if operator.role == role {
            return Ok(());
        }
        // A register that lost its last administrator is a register nobody can
        // enrol into, change a template in, or reset a key with. Refused rather
        // than warned about: the recovery is a database edit.
        if operator.role == Role::Administrator && self.administrator_count()? <= 1 {
            return Err(StoreError::Forbidden {
                who: "this register".to_owned(),
                what: "give up its last administrator — enrol another one first".to_owned(),
            });
        }
        self.conn.execute(
            "UPDATE operators SET role = ?2, updated_at = ?3 WHERE id = ?1",
            params![id.to_string(), role.slug(), Utc::now().to_rfc3339()],
        )?;
        self.append_audit(
            by,
            "operator.role.changed",
            &format!("operator:{}", operator.username),
            &format!(
                "operator={} from={} to={} by={by}",
                operator.username,
                operator.role.slug(),
                role.slug()
            ),
        )?;
        Ok(())
    }

    /// Enable or disable an operator. Never a delete: deleting one would orphan
    /// every audit entry they wrote.
    pub fn set_operator_active(&self, id: Uuid, active: bool, by: &str) -> Result<()> {
        self.require(Action::ManageOperators)?;
        let operator = self
            .operator_by_id(id)?
            .ok_or_else(|| StoreError::NotFound(format!("operator {id}")))?;
        if operator.active == active {
            return Ok(());
        }
        if !active && operator.role == Role::Administrator && self.administrator_count()? <= 1 {
            return Err(StoreError::Forbidden {
                who: "this register".to_owned(),
                what: "disable its last administrator — enrol another one first".to_owned(),
            });
        }
        self.conn.execute(
            "UPDATE operators SET active = ?2, updated_at = ?3 WHERE id = ?1",
            params![id.to_string(), i64::from(active), Utc::now().to_rfc3339()],
        )?;
        self.append_audit(
            by,
            if active {
                "operator.enabled"
            } else {
                "operator.disabled"
            },
            &format!("operator:{}", operator.username),
            &format!(
                "operator={} role={} by={by}",
                operator.username,
                operator.role.slug()
            ),
        )?;
        Ok(())
    }

    /// Set or replace an operator's password.
    ///
    /// The audit entry says a credential changed and names the method. It does
    /// not, and cannot, carry the password or anything derived from it.
    pub fn set_operator_password(&self, id: Uuid, password: &str, by: &str) -> Result<()> {
        self.require(Action::ManageOperators)?;
        let operator = self
            .operator_by_id(id)?
            .ok_or_else(|| StoreError::NotFound(format!("operator {id}")))?;
        let assessment = crate::password::assess(password);
        if !assessment.is_acceptable() {
            return Err(StoreError::WeakPassword(assessment.summary()));
        }
        let phc =
            credential::hash(password).map_err(|e| StoreError::WeakPassword(e.to_string()))?;
        self.conn.execute(
            "UPDATE operators SET password_phc = ?2, updated_at = ?3 WHERE id = ?1",
            params![id.to_string(), phc, Utc::now().to_rfc3339()],
        )?;
        self.append_audit(
            by,
            "operator.credential.changed",
            &format!("operator:{}", operator.username),
            &format!("operator={} method=local by={by}", operator.username),
        )?;
        // Changing a credential clears the failure history: the old password is
        // gone, so the failures against it are no longer evidence of anything.
        self.clear_lockout_row(&operator.username)?;
        Ok(())
    }

    /// Register the FIDO2 credential an operator will sign in with
    /// (`features/operator-auth-and-roles.md` phase 3).
    pub fn register_operator_credential(
        &self,
        id: Uuid,
        credential: &RegisteredCredential,
        by: &str,
    ) -> Result<()> {
        self.require(Action::ManageOperators)?;
        let operator = self
            .operator_by_id(id)?
            .ok_or_else(|| StoreError::NotFound(format!("operator {id}")))?;
        self.conn.execute(
            "UPDATE operators
                SET credential_id = ?2, credential_rp = ?3, credential_serial = ?4,
                    updated_at = ?5
              WHERE id = ?1",
            params![
                id.to_string(),
                credential.credential_id_hex,
                credential.relying_party,
                credential.serial as i64,
                Utc::now().to_rfc3339(),
            ],
        )?;
        // The counter the credential was registered at, in the table the sign-in
        // will advance it in.
        self.conn.execute(
            "INSERT INTO operator_sign_ins (username, credential_counter)
             VALUES (?1, ?2)
             ON CONFLICT(username) DO UPDATE SET credential_counter = excluded.credential_counter",
            params![operator.username, i64::from(credential.counter)],
        )?;
        self.append_audit(
            by,
            "operator.credential.changed",
            &format!("operator:{}", operator.username),
            &format!(
                "operator={} method=fido2 rp={} serial={} credential={} by={by}",
                operator.username,
                credential.relying_party,
                credential.serial,
                credential.credential_id_hex
            ),
        )?;
        Ok(())
    }

    fn administrator_count(&self) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT count(*) FROM operators WHERE role = 'administrator' AND active = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    // --------------------------------------------------------------- lockout

    /// The failure history for a typed username.
    pub fn lockout_for(&self, username: &str) -> Result<Lockout> {
        let username = username.trim().to_lowercase();
        Ok(self
            .conn
            .query_row(
                "SELECT failed_attempts, locked_until, last_failure_at
                   FROM operator_sign_ins WHERE username = ?1",
                params![username],
                |row| {
                    Ok(Lockout {
                        failures: row.get::<_, i64>(0)? as u32,
                        locked_until: parse_time(row.get::<_, Option<String>>(1)?),
                        last_failure_at: parse_time(row.get::<_, Option<String>>(2)?),
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    fn write_lockout(&self, username: &str, lockout: &Lockout) -> Result<()> {
        self.conn.execute(
            "INSERT INTO operator_sign_ins (username, failed_attempts, locked_until, last_failure_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(username) DO UPDATE SET
                 failed_attempts = excluded.failed_attempts,
                 locked_until    = excluded.locked_until,
                 last_failure_at = excluded.last_failure_at",
            params![
                username,
                i64::from(lockout.failures),
                lockout.locked_until.map(|at| at.to_rfc3339()),
                lockout.last_failure_at.map(|at| at.to_rfc3339()),
            ],
        )?;
        Ok(())
    }

    /// Zero the failure history without dropping the row.
    ///
    /// Reset rather than deleted, because the same row also carries the last
    /// successful sign-in and the FIDO2 signature counter — and a counter that
    /// went back to zero every time somebody signed in successfully would defeat
    /// the replay check that reads it.
    fn clear_lockout_row(&self, username: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO operator_sign_ins (username, failed_attempts, locked_until, last_failure_at)
             VALUES (?1, 0, NULL, NULL)
             ON CONFLICT(username) DO UPDATE SET
                 failed_attempts = 0, locked_until = NULL, last_failure_at = NULL",
            params![username.trim().to_lowercase()],
        )?;
        Ok(())
    }

    /// Lift a lockout. An administrator's job, and audited.
    ///
    /// The database password's throttle deliberately never locks, because there is
    /// no administrator to lift it on a register a whole unit shares. Here there
    /// is one — which is what makes the norm's lockout usable at all.
    pub fn clear_operator_lockout(&self, username: &str, by: &str) -> Result<()> {
        self.require(Action::ManageOperators)?;
        let username = username.trim().to_lowercase();
        self.clear_lockout_row(&username)?;
        self.append_audit(
            by,
            "operator.lockout.cleared",
            &format!("operator:{username}"),
            &format!("operator={username} by={by}"),
        )?;
        Ok(())
    }

    // -------------------------------------------------------------- sign-in

    /// Sign in with a password (`features/operator-auth-and-roles.md` phase 2).
    ///
    /// Every path through here writes exactly one audit entry, because
    /// `operator.login` and `operator.login.failed` are two of the three events
    /// the norm requires to be audited **always**.
    pub fn sign_in_with_password(
        &self,
        username: &str,
        password: &str,
        now: DateTime<Utc>,
    ) -> Result<Session> {
        let username = username.trim().to_lowercase();
        let operator = self.operator_by_username(&username)?;

        let verdict = match &operator {
            None => Err(SignInRefusal::Credential),
            Some(operator) if !operator.active => Err(SignInRefusal::Disabled),
            Some(operator) if !operator.has_password => Err(SignInRefusal::NoSuchMethod),
            Some(operator) => match self.password_phc(operator.id)? {
                None => Err(SignInRefusal::NoSuchMethod),
                Some(stored) => match credential::verify(password, &stored) {
                    Ok(true) => Ok(()),
                    Ok(false) => Err(SignInRefusal::Credential),
                    // An unreadable stored hash is a broken register, not a wrong
                    // password — and it is loud, because it means somebody cannot
                    // sign in for a reason retyping will never fix.
                    Err(e) => {
                        tracing::error!(
                            event = "operator.credential.unreadable",
                            operator = operator.username.as_str(),
                            reason = %e
                        );
                        Err(SignInRefusal::Credential)
                    }
                },
            },
        };

        self.finish_sign_in(&username, operator, AuthMethod::Local, verdict, now)
    }

    /// Sign in with the operator's own security key
    /// (`features/operator-auth-and-roles.md` phase 3).
    ///
    /// The assertion has already been obtained from the authenticator by the
    /// caller — `store` does not depend on `device` — and what happens here is the
    /// checking: is this the credential this register registered, did the
    /// authenticator verify the *user* rather than merely their finger on the
    /// contact, and did the signature counter advance.
    pub fn sign_in_with_credential(
        &self,
        username: &str,
        asserted: &AssertedCredential,
        now: DateTime<Utc>,
    ) -> Result<Session> {
        let username = username.trim().to_lowercase();
        let operator = self.operator_by_username(&username)?;

        let verdict = match &operator {
            None => Err(SignInRefusal::Credential),
            Some(operator) if !operator.active => Err(SignInRefusal::Disabled),
            Some(operator) => match &operator.credential {
                None => Err(SignInRefusal::NoSuchMethod),
                Some(registered) => {
                    if !registered
                        .credential_id_hex
                        .eq_ignore_ascii_case(&asserted.credential_id_hex)
                        || registered.relying_party != asserted.relying_party
                    {
                        Err(SignInRefusal::WrongCredential)
                    } else if !asserted.user_verified {
                        // A touch proves somebody is present. It does not prove
                        // *who*, which is the entire point of authenticating.
                        Err(SignInRefusal::NotUserVerified)
                    } else if asserted.counter != 0 && asserted.counter <= registered.counter {
                        // Zero means the authenticator does not count, which is
                        // permitted by the specification; anything else going
                        // backwards is what a cloned authenticator looks like.
                        Err(SignInRefusal::CounterReplay)
                    } else {
                        Ok(())
                    }
                }
            },
        };

        if verdict.is_ok() {
            self.conn.execute(
                "INSERT INTO operator_sign_ins (username, credential_counter)
                 VALUES (?1, ?2)
                 ON CONFLICT(username) DO UPDATE SET credential_counter = excluded.credential_counter",
                params![username, i64::from(asserted.counter)],
            )?;
        }

        self.finish_sign_in(&username, operator, AuthMethod::Fido2, verdict, now)
    }

    /// The lockout check, the audit entry and the session, shared by both
    /// methods so neither can be given a quieter trail than the other.
    fn finish_sign_in(
        &self,
        username: &str,
        operator: Option<Operator>,
        method: AuthMethod,
        verdict: std::result::Result<(), SignInRefusal>,
        now: DateTime<Utc>,
    ) -> Result<Session> {
        let mut lockout = self.lockout_for(username)?;

        // Checked before the verdict is acted on, and *after* it was computed:
        // computing it first keeps the work — and therefore the time this call
        // takes — the same whether the account exists or not.
        if lockout.is_locked_at(now) {
            self.record_failed_sign_in(username, method, SignInRefusal::LockedOut, &lockout, now)?;
            return Err(StoreError::Forbidden {
                who: "this sign-in".to_owned(),
                what: lockout
                    .message_at(now)
                    .unwrap_or_else(|| SignInRefusal::LockedOut.message().to_owned()),
            });
        }

        match verdict {
            Err(refusal) => {
                lockout.record_failure(now);
                self.write_lockout(username, &lockout)?;
                self.record_failed_sign_in(username, method, refusal, &lockout, now)?;
                Err(StoreError::Forbidden {
                    who: "this sign-in".to_owned(),
                    what: refusal.message().to_owned(),
                })
            }
            Ok(()) => {
                let operator = operator.expect("an accepted sign-in has an operator");
                self.clear_lockout_row(username)?;
                self.conn.execute(
                    "UPDATE operator_sign_ins SET last_login_at = ?2 WHERE username = ?1",
                    params![username, now.to_rfc3339()],
                )?;
                self.append_audit(
                    &operator.username,
                    "operator.login",
                    &format!("operator:{}", operator.username),
                    &format!(
                        "operator={} method={} role={}",
                        operator.username,
                        method.slug(),
                        operator.role.slug()
                    ),
                )?;
                tracing::info!(
                    event = "operator.login",
                    operator = operator.username.as_str(),
                    method = method.slug(),
                    role = operator.role.slug()
                );
                Ok(Session::open(
                    operator.id,
                    operator.username,
                    operator.display_name,
                    operator.role,
                    method,
                    now,
                ))
            }
        }
    }

    /// The one place a failed sign-in is recorded.
    ///
    /// The detail is built from the reason, the attempt count and the remaining
    /// lockout — the three fields the specification's `operator.login.failed` row
    /// names — plus the username that was typed, which is not a secret and is the
    /// only thing that makes the entry investigable. Nothing that was typed as a
    /// credential appears, at any length.
    fn record_failed_sign_in(
        &self,
        username: &str,
        method: AuthMethod,
        refusal: SignInRefusal,
        lockout: &Lockout,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let detail = format!(
            "operator={username} method={} reason={} {}",
            method.slug(),
            refusal.slug(),
            lockout.audit_detail(now)
        );
        tracing::warn!(
            event = "operator.login.failed",
            operator = username,
            method = method.slug(),
            reason = refusal.slug(),
            attempt = lockout.failures
        );
        self.append_audit(
            "(not signed in)",
            "operator.login.failed",
            &format!("operator:{username}"),
            &detail,
        )?;
        Ok(())
    }

    /// Record a sign-out. `reason` is `explicit`, `timeout` or `locked`.
    pub fn record_sign_out(&self, username: &str, reason: &str) -> Result<()> {
        self.append_audit(
            username,
            "operator.logout",
            &format!("operator:{username}"),
            &format!("operator={username} reason={reason}"),
        )?;
        tracing::info!(
            event = "operator.logout",
            operator = username,
            reason = reason
        );
        Ok(())
    }

    /// Record that a session was locked rather than ended.
    pub fn record_session_locked(&self, username: &str, reason: &str) -> Result<()> {
        self.append_audit(
            username,
            "operator.session.locked",
            &format!("operator:{username}"),
            &format!("operator={username} reason={reason}"),
        )?;
        Ok(())
    }

    /// Record a re-verification for a sensitive operation
    /// (`features/operator-auth-and-roles.md` phase 5).
    pub fn record_reverified(
        &self,
        username: &str,
        action: Action,
        method: AuthMethod,
    ) -> Result<()> {
        self.append_audit(
            username,
            "operator.reverified",
            &format!("operator:{username}"),
            &format!(
                "operator={username} action={} method={}",
                action.slug(),
                method.slug()
            ),
        )?;
        Ok(())
    }

    /// Record that authorisation refused something.
    ///
    /// A refusal is a security event in its own right: it is either somebody
    /// trying what they may not do, or a screen offering a button it should have
    /// hidden. Both are worth having on the trail.
    pub fn record_refusal(&self, actor: &str, action: Action, reason: &str) -> Result<()> {
        self.append_audit(
            actor,
            "operator.authorisation.refused",
            &format!("action:{}", action.slug()),
            &format!(
                "operator={actor} action={} authority={} reason={reason}",
                action.slug(),
                self.authority().who()
            ),
        )?;
        tracing::warn!(
            event = "operator.authorisation.refused",
            operator = actor,
            action = action.slug()
        );
        Ok(())
    }
}

/// One `LEFT JOIN`, so an operator and the sign-in state that belongs to their
/// username arrive together. The join is on the username rather than the id
/// because that is what `operator_sign_ins` is keyed on, for the reason
/// `MIGRATE_V9` gives.
const OPERATOR_COLUMNS: &str = "SELECT o.id, o.username, o.display_name, o.role, o.active, \
                                o.created_at, o.created_by, o.updated_at, o.password_phc, \
                                o.credential_id, o.credential_rp, o.credential_serial, \
                                COALESCE(s.credential_counter, 0), s.last_login_at \
                                FROM operators o \
                                LEFT JOIN operator_sign_ins s ON s.username = o.username";

fn parse_time(raw: Option<String>) -> Option<DateTime<Utc>> {
    raw.and_then(|raw| DateTime::parse_from_rfc3339(&raw).ok())
        .map(|at| at.with_timezone(&Utc))
}

fn read_operator(row: &rusqlite::Row<'_>) -> Result<Operator> {
    let id_raw: String = row.get(0)?;
    let id = Uuid::parse_str(&id_raw).map_err(|_| StoreError::Decode {
        column: "operators.id",
        value: id_raw,
    })?;
    let role_raw: String = row.get(3)?;
    let role = Role::parse(&role_raw).ok_or(StoreError::Decode {
        column: "operators.role",
        value: role_raw,
    })?;
    let credential = match (
        row.get::<_, Option<String>>(9)?,
        row.get::<_, Option<String>>(10)?,
        row.get::<_, Option<i64>>(11)?,
    ) {
        (Some(credential_id_hex), Some(relying_party), Some(serial)) => {
            Some(RegisteredCredential {
                credential_id_hex,
                relying_party,
                serial: serial as u32,
                counter: row.get::<_, i64>(12)? as u32,
            })
        }
        _ => None,
    };
    Ok(Operator {
        id,
        username: row.get(1)?,
        display_name: row.get(2)?,
        active: row.get::<_, i64>(4)? != 0,
        created_at: parse_time(Some(row.get(5)?)).unwrap_or_else(Utc::now),
        created_by: row.get(6)?,
        updated_at: parse_time(Some(row.get(7)?)).unwrap_or_else(Utc::now),
        has_password: row.get::<_, Option<String>>(8)?.is_some(),
        role,
        credential,
        last_login_at: parse_time(row.get::<_, Option<String>>(13)?),
    })
}
