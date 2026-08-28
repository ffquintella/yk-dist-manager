//! Who is using the tool, and what that lets them do
//! (`features/operator-auth-and-roles.md`).
//!
//! Until this module existed, the `actor` on every audit entry came from `$USER`
//! and was editable in Settings. That is a label, and an audit trail whose actor
//! is a label is only as strong as the assumption that whoever is at the
//! workstation is who they say they are — the exact assumption an audit trail
//! exists to avoid needing.
//!
//! ## The three parts, and why they are separate
//!
//! * [`Role`] and [`Action`] — **authorisation**. By role, never per user, because
//!   the norm is explicit about that: a permission granted to a person is a
//!   permission nobody can review, while a permission granted to a role is one
//!   line in a table.
//! * [`credential`] — **a local password**, Argon2id, the break-glass path.
//! * [`lockout`] — **how slowly a wrong one may be retried**.
//! * [`session`] — **how long being signed in lasts**, and what a sensitive
//!   operation needs on top of it.
//!
//! ## Where the refusal lives
//!
//! Not here, and not in `src/ui/`. This module decides; [`crate::store`] refuses.
//! Two layers, and the spec asks for both:
//!
//! 1. A SQLite **authorizer** on the connection, so a role that may not write to
//!    `templates` is refused by the database while the statement is being
//!    prepared — the same reasoning that made read-only mode a connection flag
//!    rather than a guard in each of twenty-odd methods. A method added next year
//!    cannot forget it.
//! 2. [`crate::store::Store::require`], for the operations that are not a table
//!    write at all: resetting an applet, changing the database password, taking
//!    an export out of the register.
//!
//! A UI that additionally hides what a role cannot do is welcome; a UI bug that
//! shows the button anyway still cannot get past either layer.

pub mod credential;
pub mod lockout;
pub mod session;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use credential::CredentialError;
pub use session::{Idle, Session, SessionState};

/// What an operator is allowed to be.
///
/// Three, from the spec, and deliberately not more: a role nobody can describe in
/// one sentence is a role nobody will assign correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Role {
    /// Everything, including templates, applet resets, the database password and
    /// the operator list itself.
    Administrator,
    /// The daily work: the inventory, holders, bootstrap runs, hand-overs and
    /// returns, and what happens to a key afterwards.
    Distributor,
    /// Reads everything, verifies the chain, exports reports. Changes nothing.
    Auditor,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Administrator, Role::Distributor, Role::Auditor];

    /// The value stored in the `operators.role` column, and printed in audit
    /// details. A word, not a number: the register has to be answerable from a
    /// SQL console during an audit.
    pub fn slug(&self) -> &'static str {
        match self {
            Role::Administrator => "administrator",
            Role::Distributor => "distributor",
            Role::Auditor => "auditor",
        }
    }

    pub fn parse(raw: &str) -> Option<Role> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "administrator" | "admin" => Some(Role::Administrator),
            "distributor" => Some(Role::Distributor),
            "auditor" => Some(Role::Auditor),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Role::Administrator => "Administrator",
            Role::Distributor => "Distributor",
            Role::Auditor => "Auditor",
        }
    }

    /// One sentence, shown beside the role wherever one is chosen.
    pub fn description(&self) -> &'static str {
        match self {
            Role::Administrator => {
                "Everything, including editing procedures, resetting applets, changing the \
                 database password and managing operators."
            }
            Role::Distributor => {
                "The daily work: read the inventory, register holders, run a bootstrap, record \
                 hand-overs and returns. Cannot edit a procedure or reset an applet."
            }
            Role::Auditor => {
                "Reads everything and exports reports; verifies the audit chain. Changes nothing."
            }
        }
    }

    /// May this role take this action?
    ///
    /// The whole authorisation matrix, in one place, so the answer cannot differ
    /// between the screen that hides a button and the store that refuses the
    /// write.
    pub fn may(&self, action: Action) -> bool {
        use Action::*;
        match self {
            Role::Administrator => true,
            Role::Distributor => matches!(
                action,
                Read | ManageHolders
                    | ManageInventory
                    | RunBootstrap
                    | RecordDistribution
                    | RecordLifecycle
                    | Export
            ),
            Role::Auditor => matches!(action, Read | Export | VerifyAudit),
        }
    }

    /// May this role write to this table?
    ///
    /// What the SQLite authorizer asks. Tables rather than actions because that is
    /// the granularity the database offers, and it is the coarser of the two
    /// layers on purpose: it catches everything, including a mutation added after
    /// this was written, while [`Role::may`] draws the line where a table cannot.
    ///
    /// Three tables are writable by **every** signed-in role, and each for its own
    /// reason:
    ///
    /// * `audit` — an auditor's own login has to be recordable, and the table is
    ///   append-only by trigger anyway, so "write" here can only mean *append*.
    /// * `open_sessions` — presence is a claim about who is looking, not a change
    ///   to the register.
    /// * `operator_sign_ins` — everything the sign-in mechanism writes about
    ///   itself (the failure counter, the last successful sign-in, the FIDO2
    ///   signature counter) must be writable by a session that has **not**
    ///   authenticated, because that is the only session there is while somebody
    ///   is signing in.
    pub fn may_write_table(&self, table: &str) -> bool {
        if always_writable(table) {
            return true;
        }
        match self {
            Role::Administrator => true,
            Role::Distributor => !ADMINISTRATOR_ONLY_TABLES.contains(&table),
            Role::Auditor => false,
        }
    }
}

/// Tables only an administrator may write.
///
/// `templates` and `term_templates` decide what is written to security hardware
/// and what a holder signs; `operators` is the authorisation list itself, and a
/// role that could edit it would be every role.
pub const ADMINISTRATOR_ONLY_TABLES: [&str; 3] = ["templates", "term_templates", "operators"];

/// Tables any signed-in session may write, including one that has not signed in.
pub const ALWAYS_WRITABLE_TABLES: [&str; 3] = ["audit", "open_sessions", "operator_sign_ins"];

fn always_writable(table: &str) -> bool {
    ALWAYS_WRITABLE_TABLES.contains(&table)
}

/// Something an operator can ask the register to do.
///
/// Coarser than "one per method": the point is a list a person can read and
/// assign, so `ManageInventory` covers adding a key, refreshing it and changing
/// its state rather than being three entries nobody would keep in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Read anything in the register.
    Read,
    /// Register or correct a holder.
    ManageHolders,
    /// Add, refresh or change the state of a key.
    ManageInventory,
    /// Apply a procedure to a key.
    RunBootstrap,
    /// Record a hand-over or a return.
    RecordDistribution,
    /// Record what happened to a key after the hand-over: a loss, a revocation,
    /// an RMA.
    RecordLifecycle,
    /// Verify the audit chain.
    VerifyAudit,
    /// Take an export out of the register — which takes personal data with it.
    Export,
    /// Add, edit, import, retire or change the signature policy of a procedure or
    /// a term.
    ManageTemplates,
    /// Factory-reset an applet, destroying what is on it.
    ResetApplet,
    /// Change the password the database file is encrypted with.
    ChangeDatabasePassword,
    /// Add, change the role of, or disable an operator.
    ManageOperators,
    /// Change a setting that affects security: the transport, the audit mirror,
    /// the trusted signing keys.
    ChangeSecuritySettings,
}

impl Action {
    /// What the refusal says, and what an audit detail records.
    pub fn slug(&self) -> &'static str {
        use Action::*;
        match self {
            Read => "read",
            ManageHolders => "manage-holders",
            ManageInventory => "manage-inventory",
            RunBootstrap => "run-bootstrap",
            RecordDistribution => "record-distribution",
            RecordLifecycle => "record-lifecycle",
            VerifyAudit => "verify-audit",
            Export => "export",
            ManageTemplates => "manage-templates",
            ResetApplet => "reset-applet",
            ChangeDatabasePassword => "change-database-password",
            ManageOperators => "manage-operators",
            ChangeSecuritySettings => "change-security-settings",
        }
    }

    /// A sentence naming the action, for the refusal an operator reads.
    pub fn describe(&self) -> &'static str {
        use Action::*;
        match self {
            Read => "read this register",
            ManageHolders => "register or correct a holder",
            ManageInventory => "change the inventory",
            RunBootstrap => "run a bootstrap",
            RecordDistribution => "record a hand-over or a return",
            RecordLifecycle => "record what happened to a key after the hand-over",
            VerifyAudit => "verify the audit chain",
            Export => "take an export out of this register",
            ManageTemplates => "edit a procedure or a term",
            ResetApplet => "factory-reset an applet",
            ChangeDatabasePassword => "change the database password",
            ManageOperators => "manage operators",
            ChangeSecuritySettings => "change a security setting",
        }
    }

    /// Does this need the key touched again, and not merely a session?
    ///
    /// `features/operator-auth-and-roles.md` phase 5. The list is the spec's:
    /// template edit, applet reset, password change, export of personal data —
    /// plus operator management and the security settings, which are the two
    /// other ways to change what everybody else may do.
    ///
    /// The reasoning is the shared workstation: a session is *how long ago
    /// somebody authenticated*, and for anything destructive or irreversible that
    /// is not the same question as *is that person still here*.
    pub fn needs_reverification(&self) -> bool {
        use Action::*;
        matches!(
            self,
            ManageTemplates
                | ResetApplet
                | ChangeDatabasePassword
                | ManageOperators
                | ChangeSecuritySettings
                | Export
        )
    }
}

/// How an operator proved who they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMethod {
    /// A FIDO2 credential on the operator's own key, verified over CTAP2 with
    /// user verification. The preferred one: the unit distributing security keys
    /// is the unit most able to hold one.
    Fido2,
    /// The corporate directory. **Not built** — the mechanism is an ESI decision
    /// (`AGENTS.md` §8), so the variant exists to be named in a record and
    /// nothing produces it yet.
    Directory,
    /// A password in this register, Argon2id-hashed. The break-glass path.
    Local,
}

impl AuthMethod {
    pub fn slug(&self) -> &'static str {
        match self {
            AuthMethod::Fido2 => "fido2",
            AuthMethod::Directory => "ad",
            AuthMethod::Local => "local",
        }
    }

    pub fn parse(raw: &str) -> Option<AuthMethod> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "fido2" => Some(AuthMethod::Fido2),
            "ad" | "directory" => Some(AuthMethod::Directory),
            "local" => Some(AuthMethod::Local),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            AuthMethod::Fido2 => "security key",
            AuthMethod::Directory => "corporate directory",
            AuthMethod::Local => "password",
        }
    }
}

/// An operator on this register.
///
/// Note what is **not** here: no password, and no hash either. The PHC string
/// lives in the `operators` table and is read only by the one store method that
/// verifies against it, so there is no field on any struct the GUI can reach that
/// could be printed, logged or serialised by accident (`AGENTS.md` §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operator {
    pub id: Uuid,
    /// Unique within the register, lower-cased, and what appears as the audit
    /// `actor`.
    pub username: String,
    /// The name a person is called by, for the screen.
    pub display_name: String,
    pub role: Role,
    /// A disabled operator keeps their history and cannot sign in. Deleting one
    /// would orphan every audit entry they wrote.
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub updated_at: DateTime<Utc>,
    /// True when a password has been set. Never the hash, and never the password.
    pub has_password: bool,
    /// The FIDO2 credential registered for them, when there is one.
    pub credential: Option<RegisteredCredential>,
    pub last_login_at: Option<DateTime<Utc>>,
}

impl Operator {
    /// Which methods this operator can actually sign in with, right now.
    pub fn methods(&self) -> Vec<AuthMethod> {
        let mut methods = Vec::new();
        if self.credential.is_some() {
            methods.push(AuthMethod::Fido2);
        }
        if self.has_password {
            methods.push(AuthMethod::Local);
        }
        methods
    }

    /// An operator with no way to sign in is a row, not an account. Worth saying
    /// on screen, because enrolling one and forgetting the credential is the
    /// obvious mistake.
    pub fn can_sign_in(&self) -> bool {
        self.active && !self.methods().is_empty()
    }
}

/// The FIDO2 credential this register registered for an operator.
///
/// Public data by construction: a credential id, a relying-party id and a
/// signature counter. The private key never leaves the authenticator, which is
/// the property the whole method rests on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredCredential {
    pub credential_id_hex: String,
    pub relying_party: String,
    /// The serial of the key it was made on, so a refusal can say *that is not
    /// your key* rather than *authentication failed*.
    pub serial: u32,
    /// The last signature counter seen. A counter that goes backwards is the
    /// classic sign of a cloned authenticator.
    pub counter: u32,
}

/// What a register knows about who may use it.
///
/// The three states are the whole first-run story, and the middle one is the
/// reason an existing register cannot be locked out by this feature:
///
/// * [`Authority::Unenrolled`] — the `operators` table is empty. Every register
///   written before schema v9 is in this state after the migration, and it
///   behaves exactly as it did before: the actor is the workstation's signed-in
///   user, said plainly to be a label. Nothing is refused.
/// * [`Authority::SignedOut`] — operators exist and nobody has signed in. Reads
///   are allowed so the sign-in screen can name the register; nothing operational
///   can be written.
/// * [`Authority::SignedIn`] — a role, and [`Role::may`] decides.
///
/// A migration that *created* an administrator would have had to invent a
/// credential for one, and a migration that demanded one before opening would
/// have made every existing register unopenable until somebody read a release
/// note. Turning the control on is therefore a deliberate, audited act by
/// somebody at the keyboard — the same shape as the database password and the
/// template-signature policy, both of which are off until a deployment turns them
/// on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authority {
    Unenrolled,
    SignedOut,
    SignedIn(Role),
}

impl Authority {
    /// The code carried in the atomic the SQLite authorizer reads.
    ///
    /// An integer rather than the enum because the callback must be
    /// `Send + 'static` and cannot borrow the store: it reads one `AtomicU8`,
    /// which is also the cheapest thing that can be read on every statement
    /// preparation.
    pub fn code(&self) -> u8 {
        match self {
            Authority::Unenrolled => 0,
            Authority::SignedOut => 1,
            Authority::SignedIn(Role::Administrator) => 2,
            Authority::SignedIn(Role::Distributor) => 3,
            Authority::SignedIn(Role::Auditor) => 4,
        }
    }

    pub fn from_code(code: u8) -> Authority {
        match code {
            1 => Authority::SignedOut,
            2 => Authority::SignedIn(Role::Administrator),
            3 => Authority::SignedIn(Role::Distributor),
            4 => Authority::SignedIn(Role::Auditor),
            _ => Authority::Unenrolled,
        }
    }

    /// May a statement under this authority write to this table?
    pub fn may_write_table(&self, table: &str) -> bool {
        match self {
            // A register with no operators is the register this tool has always
            // been. The control is off, and off means off — not "quietly on".
            Authority::Unenrolled => true,
            // Enough to record the sign-in attempt and its outcome, and nothing
            // else. In particular not `operators`: enrolling from a signed-out
            // session would be the way round the whole feature.
            Authority::SignedOut => always_writable(table),
            Authority::SignedIn(role) => role.may_write_table(table),
        }
    }

    /// May this authority take this action?
    pub fn may(&self, action: Action) -> bool {
        match self {
            Authority::Unenrolled => true,
            Authority::SignedOut => matches!(action, Action::Read),
            Authority::SignedIn(role) => role.may(action),
        }
    }

    /// Who the refusal names.
    pub fn who(&self) -> &'static str {
        match self {
            Authority::Unenrolled => "this register",
            Authority::SignedOut => "a session that has not signed in",
            Authority::SignedIn(role) => role.label(),
        }
    }

    pub fn role(&self) -> Option<Role> {
        match self {
            Authority::SignedIn(role) => Some(*role),
            _ => None,
        }
    }

    pub fn is_signed_in(&self) -> bool {
        matches!(self, Authority::SignedIn(_))
    }
}

/// What is being asked for when an operator is enrolled.
///
/// A struct rather than five parameters because four of them are strings, and a
/// call that transposed the display name and the username would be accepted by
/// the compiler and wrong in every audit entry afterwards.
#[derive(Debug, Clone)]
pub struct NewOperator {
    pub username: String,
    pub display_name: String,
    pub role: Role,
}

/// Normalise a username: trimmed, lower-cased, bounded.
///
/// Lower-cased because `Ana` and `ana` signing different audit entries would be
/// two actors for one person, and the register would answer "everything Ana did"
/// with half of it.
pub fn normalise_username(raw: &str) -> Result<String, crate::domain::ValidationError> {
    let trimmed = crate::domain::require_text("username", raw)?;
    let lowered = trimmed.to_lowercase();
    if lowered
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '=' || c == ';')
    {
        return Err(crate::domain::ValidationError::Missing("username"));
    }
    Ok(lowered)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec's table, asserted rather than described: a distributor cannot
    /// edit a template and an auditor cannot write anything.
    #[test]
    fn the_role_matrix_is_the_one_the_specification_states() {
        assert!(Role::Distributor.may(Action::RunBootstrap));
        assert!(Role::Distributor.may(Action::RecordDistribution));
        assert!(Role::Distributor.may(Action::ManageHolders));
        assert!(!Role::Distributor.may(Action::ManageTemplates));
        assert!(!Role::Distributor.may(Action::ResetApplet));
        assert!(!Role::Distributor.may(Action::ChangeDatabasePassword));
        assert!(!Role::Distributor.may(Action::ManageOperators));
        assert!(!Role::Distributor.may(Action::ChangeSecuritySettings));

        assert!(Role::Auditor.may(Action::Read));
        assert!(Role::Auditor.may(Action::VerifyAudit));
        assert!(Role::Auditor.may(Action::Export));
        for action in [
            Action::ManageHolders,
            Action::ManageInventory,
            Action::RunBootstrap,
            Action::RecordDistribution,
            Action::RecordLifecycle,
            Action::ManageTemplates,
            Action::ResetApplet,
            Action::ChangeDatabasePassword,
            Action::ManageOperators,
            Action::ChangeSecuritySettings,
        ] {
            assert!(
                !Role::Auditor.may(action),
                "an auditor changes nothing, and {} is a change",
                action.slug()
            );
        }

        // The administrator's row is "everything", and it is asserted rather than
        // assumed: a new action added to the enum must not accidentally be denied
        // to the only role that can grant it to anybody else.
        for action in [
            Action::Read,
            Action::ManageHolders,
            Action::ManageInventory,
            Action::RunBootstrap,
            Action::RecordDistribution,
            Action::RecordLifecycle,
            Action::VerifyAudit,
            Action::Export,
            Action::ManageTemplates,
            Action::ResetApplet,
            Action::ChangeDatabasePassword,
            Action::ManageOperators,
            Action::ChangeSecuritySettings,
        ] {
            assert!(Role::Administrator.may(action));
        }
    }

    #[test]
    fn an_auditor_may_write_only_the_three_tables_that_are_not_the_register() {
        for table in ALWAYS_WRITABLE_TABLES {
            assert!(Role::Auditor.may_write_table(table));
        }
        for table in ["keys", "holders", "distributions", "templates", "operators"] {
            assert!(!Role::Auditor.may_write_table(table));
        }
    }

    #[test]
    fn a_distributor_may_write_the_register_but_not_the_procedures_or_the_operator_list() {
        for table in [
            "keys",
            "holders",
            "distributions",
            "bootstrap_runs",
            "bootstrap_run_steps",
            "documents",
            "key_incidents",
            "batches",
        ] {
            assert!(Role::Distributor.may_write_table(table));
        }
        for table in ADMINISTRATOR_ONLY_TABLES {
            assert!(!Role::Distributor.may_write_table(table));
        }
    }

    /// The first-run promise, as a test: a register with nothing in its
    /// `operators` table is the register this tool has always been.
    #[test]
    fn an_unenrolled_register_refuses_nothing() {
        let authority = Authority::Unenrolled;
        assert!(authority.may(Action::ManageTemplates));
        assert!(authority.may_write_table("templates"));
        assert!(authority.may_write_table("operators"));
    }

    /// And the other half of it: once operators exist, a session that has not
    /// signed in can read, count its own failures, and do nothing else.
    #[test]
    fn a_signed_out_session_can_count_its_failures_and_nothing_more() {
        let authority = Authority::SignedOut;
        assert!(authority.may(Action::Read));
        assert!(!authority.may(Action::ManageInventory));
        assert!(authority.may_write_table("operator_sign_ins"));
        assert!(authority.may_write_table("audit"));
        // Enrolling from a signed-out session would be the way round the feature.
        assert!(!authority.may_write_table("operators"));
        assert!(!authority.may_write_table("keys"));
    }

    #[test]
    fn the_sensitive_operations_are_the_ones_the_specification_names() {
        for action in [
            Action::ManageTemplates,
            Action::ResetApplet,
            Action::ChangeDatabasePassword,
            Action::Export,
        ] {
            assert!(action.needs_reverification(), "{}", action.slug());
        }
        for action in [
            Action::Read,
            Action::RunBootstrap,
            Action::RecordDistribution,
            Action::ManageHolders,
        ] {
            assert!(!action.needs_reverification(), "{}", action.slug());
        }
    }

    #[test]
    fn an_authority_survives_the_round_trip_through_its_code() {
        for authority in [
            Authority::Unenrolled,
            Authority::SignedOut,
            Authority::SignedIn(Role::Administrator),
            Authority::SignedIn(Role::Distributor),
            Authority::SignedIn(Role::Auditor),
        ] {
            assert_eq!(Authority::from_code(authority.code()), authority);
        }
    }

    #[test]
    fn a_role_survives_the_round_trip_through_its_slug() {
        for role in Role::ALL {
            assert_eq!(Role::parse(role.slug()), Some(role));
        }
        assert_eq!(Role::parse("ADMIN"), Some(Role::Administrator));
        assert_eq!(Role::parse("root"), None);
    }

    #[test]
    fn a_username_is_lower_cased_so_one_person_is_one_actor() {
        assert_eq!(normalise_username("  Ana.Silva  ").unwrap(), "ana.silva");
        // A name with a space or an `=` in it would break the `name=value` audit
        // detail format the whole register is read back through.
        assert!(normalise_username("ana silva").is_err());
        assert!(normalise_username("ana=admin").is_err());
        assert!(normalise_username("   ").is_err());
    }

    #[test]
    fn an_operator_with_no_credential_cannot_sign_in() {
        let mut operator = Operator {
            id: Uuid::new_v4(),
            username: "ana".into(),
            display_name: "Ana Silva".into(),
            role: Role::Distributor,
            active: true,
            created_at: Utc::now(),
            created_by: "system".into(),
            updated_at: Utc::now(),
            has_password: false,
            credential: None,
            last_login_at: None,
        };
        assert!(!operator.can_sign_in());
        assert!(operator.methods().is_empty());

        operator.has_password = true;
        assert!(operator.can_sign_in());
        assert_eq!(operator.methods(), vec![AuthMethod::Local]);

        // Disabled beats having a credential.
        operator.active = false;
        assert!(!operator.can_sign_in());
    }
}
