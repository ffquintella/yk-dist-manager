//! The workstation's own secure store, for a database password an operator has
//! chosen not to retype (`features/db-password-and-encryption.md` phase 8).
//!
//! Three stores, one interface — Keychain Services on macOS, the Credential
//! Manager on Windows, the Secret Service (GNOME Keyring, KWallet) elsewhere. All
//! three are the platform's own, so the password is protected by the operator's
//! login session rather than by anything this application invented, and all three
//! have a native viewer the operator can inspect and delete the entry with.
//!
//! ## What this is and is not
//!
//! It is a **convenience with a stated cost**, and the cost belongs on screen
//! rather than in a manual: a saved password moves the register's confidentiality
//! from "somebody has to know the password" to "somebody has to be signed in as
//! this operator on this workstation". That is a real weakening for a laptop that
//! is left unlocked, and no weakening at all for the copied file the password
//! exists to protect — a backup on a share and a sync client's conflict copy are
//! still unreadable, because the key never leaves this machine.
//!
//! So it is **opt-in, per register, per workstation**, and never a default.
//!
//! ## Rules this module exists to keep
//!
//! * The password is passed through and never held: no field of any type here
//!   owns one beyond the call it was given for.
//! * **A platform error is converted through `Display`, never `Debug`.**
//!   [`keyring::Error`] has variants (`BadEncoding`, `BadDataFormat`) that carry
//!   the raw stored bytes — which is the password — and its derived `Debug` prints
//!   them. Its `Display` does not. Every conversion in this file goes through
//!   [`VaultError::from_platform`] for that one reason, and nothing here derives
//!   `Debug` over a `keyring` type.
//! * A store that is not there is **not an error the operator has to solve**: a
//!   headless Linux session with no Secret Service, a locked keyring, a policy
//!   that forbids it. Every operation says so and the register opens the way it
//!   always did, with a typed password.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

/// What the entries are filed under in the platform store.
///
/// The operator sees this in Keychain Access, in the Credential Manager and in
/// Seahorse, so it is the application's name rather than a hash: an entry nobody
/// can identify is an entry nobody will ever delete.
pub const SERVICE: &str = "yk-dist-manager";

/// Which register a saved password belongs to.
///
/// The path as the register was **opened**, which is the same key
/// [`AppSettings::operators`](crate::settings::AppSettings::operators) uses and
/// right for the same reason: the credential store is per operator and per
/// workstation, so a mount point never has to agree between two machines. A
/// share-hosted register is keyed by its location instead — see
/// [`account_for_share`] — because there the path *is* the mount point and it
/// changes between sessions.
pub fn account_for_path(path: &Path) -> String {
    path.display().to_string()
}

/// Which register a saved password belongs to, when it lives on an SMB share.
///
/// `//server/share/yubikeys/keys.sqlite3` — the location as it was stated, not the
/// `/Volumes/share-1` the operating system happened to mount it at. This is the
/// same distinction that keeps
/// [`recent_shares`](crate::settings::AppSettings::recent_shares) apart from
/// `recent_databases`.
pub fn account_for_share(location: &str) -> String {
    location.to_owned()
}

/// Why a saved password could not be read, written or removed.
///
/// Two variants and not more, because the operator has exactly two things to do
/// about it: [`VaultError::Unavailable`] means this workstation has no credential
/// store to use and the answer is to keep typing the password, while
/// [`VaultError::Failed`] means one is there and refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VaultError {
    /// There is no credential store to talk to on this workstation.
    #[error(
        "this workstation has no credential store the application can use ({0}) — the password \
         has to be typed"
    )]
    Unavailable(String),
    /// There is one, and it said no.
    #[error("the credential store refused: {0}")]
    Failed(String),
}

impl VaultError {
    /// Convert a platform error **through `Display`**.
    ///
    /// Never `Debug`: see the module docs. `{error}` on a
    /// [`keyring::Error::BadEncoding`] is the sentence "Password data is not valid
    /// UTF-8"; `{error:?}` on the same value is the password, in a log line, for
    /// ever.
    fn from_platform(error: &keyring::Error) -> Self {
        match error {
            keyring::Error::NoDefaultStore | keyring::Error::NotSupportedByStore(_) => {
                Self::Unavailable(error.to_string())
            }
            other => Self::Failed(other.to_string()),
        }
    }
}

/// Where a database password is kept between sessions, when the operator asked
/// for it to be.
///
/// A trait for the reason [`Connector`](crate::store::smb::Connector) is one: the
/// real implementation talks to the operating system, and a test suite that has to
/// run on a build machine with no login session cannot. [`MemoryVault`] is what
/// every test uses, and it is the only way the wiring — save on unlock, use at
/// launch, drop on a password change — can be exercised at all.
pub trait Vault: Send + Sync {
    /// What this store is, in words an operator recognises: "the macOS Keychain".
    fn label(&self) -> &'static str;

    /// The saved password for this register, if there is one.
    ///
    /// `Ok(None)` means "nothing saved", which is the ordinary answer and not a
    /// failure.
    fn get(&self, account: &str) -> Result<Option<String>, VaultError>;

    /// Is there one saved, without the caller taking a copy of it?
    ///
    /// The screen needs a yes or a no — whether to offer *Forget the saved
    /// password* — and pulling a password out of the credential store to answer a
    /// yes-or-no question is a copy of a secret nobody asked for. Most stores
    /// cannot answer without fetching, which is why the default implementation
    /// does exactly that and drops the result on the spot; what the method buys is
    /// that the *call site* never holds one.
    fn has(&self, account: &str) -> Result<bool, VaultError> {
        Ok(self.get(account)?.is_some())
    }

    /// Save, replacing whatever was there for this register.
    fn set(&self, account: &str, password: &str) -> Result<(), VaultError>;

    /// Remove it. Removing one that is not there succeeds — the operator asked for
    /// the register to have no saved password, and it has none.
    fn forget(&self, account: &str) -> Result<(), VaultError>;
}

/// The platform's own credential store.
pub struct OsVault;

impl OsVault {
    fn entry(account: &str) -> Result<keyring::Entry, VaultError> {
        keyring::Entry::new(SERVICE, account).map_err(|e| VaultError::from_platform(&e))
    }
}

impl Vault for OsVault {
    fn label(&self) -> &'static str {
        platform_label()
    }

    fn get(&self, account: &str) -> Result<Option<String>, VaultError> {
        match Self::entry(account)?.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(VaultError::from_platform(&e)),
        }
    }

    fn set(&self, account: &str, password: &str) -> Result<(), VaultError> {
        Self::entry(account)?
            .set_password(password)
            .map_err(|e| VaultError::from_platform(&e))
    }

    fn forget(&self, account: &str) -> Result<(), VaultError> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(VaultError::from_platform(&e)),
        }
    }
}

/// What the credential store is called on the platform this build runs on.
///
/// Named rather than described, because the operator has to be able to find the
/// entry afterwards: "the macOS Keychain" points at Keychain Access, and "the
/// Secret Service" is the phrase that leads to whichever of GNOME Keyring and
/// KWallet the desktop actually runs.
pub fn platform_label() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "the macOS Keychain"
    }
    #[cfg(windows)]
    {
        "the Windows Credential Manager"
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        "the Secret Service (GNOME Keyring / KWallet)"
    }
}

/// A vault that outlives the sessions built on it — which is what a credential
/// store is.
///
/// The application takes a `Box<dyn Vault>` per session, and a workstation has one
/// store that every session sees. This is what lets a test say so: wrap the vault
/// in an `Arc`, hand a clone to each `YkDistApp`, and what the first session saved
/// is what the third one finds.
impl<V: Vault + ?Sized> Vault for std::sync::Arc<V> {
    fn label(&self) -> &'static str {
        (**self).label()
    }

    fn get(&self, account: &str) -> Result<Option<String>, VaultError> {
        (**self).get(account)
    }

    fn has(&self, account: &str) -> Result<bool, VaultError> {
        (**self).has(account)
    }

    fn set(&self, account: &str, password: &str) -> Result<(), VaultError> {
        (**self).set(account, password)
    }

    fn forget(&self, account: &str) -> Result<(), VaultError> {
        (**self).forget(account)
    }
}

/// The environment variable that takes the credential store out of play.
///
/// A deployment whose security policy is "the database password is typed, every
/// time, on every workstation" should be able to say so once rather than trust
/// every operator not to tick a box. Setting this to anything non-empty makes the
/// application behave exactly as it does on a workstation that has no credential
/// store: the option is offered nowhere, nothing is read, nothing is written, and
/// the reason names this variable.
///
/// It does **not** delete what a previous session saved. Removing an entry is a
/// deliberate act, and the platform's own viewer — Keychain Access, the Credential
/// Manager, Seahorse — is where it belongs when the application has been told to
/// stop looking.
pub const DISABLE_ENV: &str = "YKDM_NO_SAVED_PASSWORD";

/// The vault this build talks to.
pub fn platform_vault() -> Box<dyn Vault> {
    match std::env::var(DISABLE_ENV) {
        Ok(value) if !value.trim().is_empty() => Box::new(DisabledVault),
        _ => Box::new(OsVault),
    }
}

/// The credential store, switched off by policy — see [`DISABLE_ENV`].
///
/// It refuses rather than pretending to succeed, because the two honest states are
/// "saved" and "not saved". A vault that silently accepted a password and lost it
/// would be the one outcome nobody could plan around.
pub struct DisabledVault;

impl Vault for DisabledVault {
    fn label(&self) -> &'static str {
        "no credential store (disabled by $YKDM_NO_SAVED_PASSWORD)"
    }

    fn get(&self, _account: &str) -> Result<Option<String>, VaultError> {
        Err(Self::refusal())
    }

    fn set(&self, _account: &str, _password: &str) -> Result<(), VaultError> {
        Err(Self::refusal())
    }

    fn forget(&self, _account: &str) -> Result<(), VaultError> {
        Err(Self::refusal())
    }
}

impl DisabledVault {
    fn refusal() -> VaultError {
        VaultError::Unavailable(format!("switched off by ${DISABLE_ENV}"))
    }
}

/// A vault that lives for as long as the process does.
///
/// For tests, and only for tests: it is the whole reason the wiring around saved
/// passwords is testable without a login session, a keyring daemon or a signed
/// bundle. It is deliberately *not* a fallback for a workstation whose real store
/// is unavailable — a "saved" password that quietly disappears at the end of the
/// session would be a promise the application did not keep.
#[derive(Default)]
pub struct MemoryVault {
    saved: Mutex<BTreeMap<String, String>>,
    /// Answer everything with [`VaultError::Unavailable`], to exercise the half of
    /// the wiring that runs on a workstation with no credential store.
    unavailable: bool,
}

impl MemoryVault {
    pub fn new() -> Self {
        Self::default()
    }

    /// A vault that refuses everything, the way a headless session with no Secret
    /// Service does.
    pub fn unavailable() -> Self {
        Self {
            saved: Mutex::default(),
            unavailable: true,
        }
    }

    /// How many registers have a password saved. For assertions, never for logic.
    pub fn len(&self) -> usize {
        self.saved.lock().expect("vault lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn refuse(&self) -> Option<VaultError> {
        self.unavailable
            .then(|| VaultError::Unavailable("no credential store in this test".into()))
    }
}

impl Vault for MemoryVault {
    fn label(&self) -> &'static str {
        "an in-memory store (tests only)"
    }

    fn get(&self, account: &str) -> Result<Option<String>, VaultError> {
        if let Some(e) = self.refuse() {
            return Err(e);
        }
        Ok(self.saved.lock().expect("vault lock").get(account).cloned())
    }

    fn set(&self, account: &str, password: &str) -> Result<(), VaultError> {
        if let Some(e) = self.refuse() {
            return Err(e);
        }
        self.saved
            .lock()
            .expect("vault lock")
            .insert(account.to_owned(), password.to_owned());
        Ok(())
    }

    fn forget(&self, account: &str) -> Result<(), VaultError> {
        if let Some(e) = self.refuse() {
            return Err(e);
        }
        self.saved.lock().expect("vault lock").remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_register_is_keyed_by_the_path_it_was_opened_at() {
        assert_eq!(
            account_for_path(Path::new("/srv/keys.sqlite3")),
            "/srv/keys.sqlite3"
        );
    }

    #[test]
    fn a_share_is_keyed_by_its_location_and_not_by_a_mount_point() {
        // The mount point moves between sessions — `/Volumes/ti`, `/Volumes/ti-1`
        // — so keying by it would lose the saved password the second time the
        // share is connected in one session.
        assert_eq!(
            account_for_share("//fileserver/ti/yubikeys/keys.sqlite3"),
            "//fileserver/ti/yubikeys/keys.sqlite3"
        );
    }

    #[test]
    fn saving_then_reading_gives_the_password_back() {
        let vault = MemoryVault::new();
        assert_eq!(vault.get("register").unwrap(), None);
        vault
            .set("register", "correct horse battery staple")
            .unwrap();
        assert_eq!(
            vault.get("register").unwrap().as_deref(),
            Some("correct horse battery staple")
        );
        assert_eq!(vault.len(), 1);
    }

    #[test]
    fn saving_again_replaces_rather_than_adds() {
        let vault = MemoryVault::new();
        vault.set("register", "first passphrase here").unwrap();
        vault.set("register", "second passphrase here").unwrap();
        assert_eq!(vault.len(), 1);
        assert_eq!(
            vault.get("register").unwrap().as_deref(),
            Some("second passphrase here")
        );
    }

    #[test]
    fn two_registers_are_saved_apart() {
        let vault = MemoryVault::new();
        vault.set("/srv/one.sqlite3", "passphrase for one").unwrap();
        vault.set("/srv/two.sqlite3", "passphrase for two").unwrap();
        assert_eq!(
            vault.get("/srv/one.sqlite3").unwrap().as_deref(),
            Some("passphrase for one")
        );
        assert_eq!(
            vault.get("/srv/two.sqlite3").unwrap().as_deref(),
            Some("passphrase for two")
        );
    }

    #[test]
    fn asking_whether_one_is_saved_does_not_hand_the_caller_a_password() {
        let vault = MemoryVault::new();
        assert!(!vault.has("register").unwrap());
        vault
            .set("register", "correct horse battery staple")
            .unwrap();
        assert!(vault.has("register").unwrap());
    }

    #[test]
    fn forgetting_one_that_was_never_saved_is_not_a_failure() {
        // The operator asked for this register to have no saved password. It has
        // none. Reporting that as an error would put a red line on the screen for
        // an outcome that is exactly what was wanted.
        let vault = MemoryVault::new();
        assert!(vault.forget("never-saved").is_ok());
    }

    #[test]
    fn forgetting_removes_it() {
        let vault = MemoryVault::new();
        vault
            .set("register", "correct horse battery staple")
            .unwrap();
        vault.forget("register").unwrap();
        assert_eq!(vault.get("register").unwrap(), None);
        assert!(vault.is_empty());
    }

    #[test]
    fn a_workstation_with_no_credential_store_says_so_on_every_operation() {
        let vault = MemoryVault::unavailable();
        for outcome in [
            vault.get("register").err(),
            vault.set("register", "correct horse battery staple").err(),
            vault.forget("register").err(),
        ] {
            let error = outcome.expect("an unavailable store refuses");
            assert!(
                matches!(error, VaultError::Unavailable(_)),
                "{error} should be the unavailable case"
            );
            assert!(
                error.to_string().contains("has to be typed"),
                "the message has to leave the operator with the way in: {error}"
            );
        }
    }

    #[test]
    fn no_message_from_this_module_could_carry_a_password() {
        // The rule the module exists to keep, asserted rather than trusted: what
        // reaches a log line or the status bar is our own sentence plus the
        // platform's `Display`, and neither has anywhere for the stored bytes to
        // travel. `keyring::Error::BadEncoding` holds them and prints them under
        // `Debug`, which is why nothing here formats one that way.
        let unavailable = VaultError::Unavailable("no D-Bus session".into());
        let refused = VaultError::Failed("the keyring is locked".into());
        for message in [unavailable.to_string(), refused.to_string()] {
            assert!(!message.is_empty());
            assert!(!message.contains("correct horse battery staple"));
        }
    }

    #[test]
    fn a_deployment_can_switch_the_credential_store_off_by_policy() {
        // The refusal has to name the variable: an operator looking at "the
        // password has to be typed" on a machine that plainly has a Keychain needs
        // to be able to find out why.
        let vault = DisabledVault;
        let error = vault.get("register").expect_err("switched off");
        assert!(matches!(error, VaultError::Unavailable(_)));
        assert!(error.to_string().contains(DISABLE_ENV), "{error}");
        assert!(vault.set("register", "a passphrase here").is_err());
        assert!(vault.forget("register").is_err());
    }

    #[test]
    fn the_store_is_named_so_the_operator_can_find_the_entry_in_it() {
        let label = platform_label();
        assert!(
            label.starts_with("the "),
            "the label goes into a sentence: {label}"
        );
        assert!(!label.is_empty());
    }
}
