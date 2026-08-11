//! Hosting the database on an **SMB share**, connected by this application.
//!
//! Every other storage document here ends with "a real network share is still the
//! recommendation", and until now the application could not help anybody get one:
//! it could open a path, so the share had to be mounted first, and the symptom of
//! that not having happened was "is the share mounted?" — a message that names the
//! problem and offers nothing.
//!
//! So the share is connected here, in three separate steps, in this order:
//!
//! 1. **Parse** what the operator typed into a [`ShareTarget`] — host, share, and
//!    the path *within* the share ([`parse`]).
//! 2. **Attach** it through a [`Connector`], which reports the local path the
//!    share's root is reachable at and whether *this process* made the connection
//!    ([`ShareConnection::open`]).
//! 3. **Open** the database at that root, with the `Store` calls that already
//!    exist. A path under `\\server\share` or `/Volumes/share` classifies as
//!    [`crate::store::Location::NetworkShare`] and gets the rollback-journal
//!    pragmas, exactly as a share mounted by the operating system would.
//!
//! `Store` therefore learns nothing about SMB, and a database on a share this
//! application connected behaves identically to one on a share Finder connected.
//!
//! **Credentials.** [`Access::LoggedOnUser`] is the default: on Windows a UNC path
//! is opened with the session's own token, so the right implementation is to make
//! no API call at all and let Windows authenticate the way it already does for
//! Explorer. [`Access::Anonymous`] is guest access, chosen deliberately.
//! [`Access::Named`] is an account whose password the operator types every time,
//! held in a [`Secret`] that redacts itself in `Debug`, is zeroed on drop, and
//! never reaches an argument vector, a file or a log line. That last constraint is
//! why the backends are native API calls and not `net use` / `mount_smbfs`: a
//! password in `argv` is readable by every process on the workstation.
//!
//! An explicit choice is honoured **exactly**. The signed-in user is never a silent
//! fallback under a named account — connecting as an unexpected identity is a share
//! opened with permissions nobody reviewed, and on a share that is read-only for
//! everyone else it would look like the register had lost its writes.
//!
//! See `features/smb-share-hosting.md`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

mod mock;
pub mod mounts;
pub mod system;
pub use mock::{MockConnector, MockOutcome, StubbornConnector};

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

/// The longest location string accepted, matching the field the operator types it
/// into. A path is not a novel, and an unbounded one reaches a system call.
pub const MAX_LOCATION: usize = crate::domain::MAX_NOTE;

/// Longest host or share name accepted. NetBIOS stops at 15 and DNS labels at 63;
/// this is generous and finite, which is the requirement.
const MAX_COMPONENT: usize = 255;

// --------------------------------------------------------------------- errors

#[derive(Debug, thiserror::Error)]
pub enum SmbError {
    #[error(
        "{location} is not an SMB location — expected smb://server/share/…, \\\\server\\share\\… or //server/share/…"
    )]
    NotAShare { location: String },
    #[error(
        "{location} names no share — an SMB location needs a server *and* a share: smb://server/share/…"
    )]
    NoShare { location: String },
    #[error("the {part} in {location} is not usable: {reason}")]
    BadComponent {
        part: &'static str,
        location: String,
        reason: String,
    },
    #[error("{location} is too long ({found} characters, maximum {max})")]
    TooLong {
        location: String,
        found: usize,
        max: usize,
    },
    /// The server refused the identity we presented.
    #[error("{share} refused {identity}: {reason}")]
    Refused {
        share: String,
        identity: String,
        reason: String,
    },
    /// The share, or the network, is not there.
    #[error("{share} could not be reached: {reason}")]
    Unreachable { share: String, reason: String },
    /// This build, on this platform, cannot make the connection itself.
    #[error("{share} has to be mounted by the system on this platform: {reason}")]
    Unsupported { share: String, reason: String },
    #[error("disconnecting from {share} failed: {reason}")]
    DetachFailed { share: String, reason: String },
}

pub type Result<T> = std::result::Result<T, SmbError>;

// --------------------------------------------------------------------- target

/// A share, and the path to the database inside it.
///
/// The identity of a share is the server and the share name — deliberately *not*
/// the user, so the same share reached as two different accounts is one entry in
/// the recent list and one connection to take down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareTarget {
    /// Server name or address, as the operator wrote it.
    pub host: String,
    /// Share name, without separators.
    pub share: String,
    /// Path within the share, `/`-separated, without a leading slash. May be
    /// empty, which means the database sits in the share's root.
    pub inner: String,
}

impl ShareTarget {
    /// `\\server\share` — a usable path on Windows, and a recognisable label
    /// everywhere else.
    pub fn unc(&self) -> String {
        format!(r"\\{}\{}", self.host, self.share)
    }

    /// `smb://server/share` — what NetFS and every dialog on macOS expects.
    pub fn url(&self) -> String {
        format!("smb://{}/{}", self.host, self.share)
    }

    /// `//server/share` — the spelling used in messages, because it is the one
    /// that reads the same on all three platforms.
    pub fn describe(&self) -> String {
        format!("//{}/{}", self.host, self.share)
    }

    /// The canonical location of the database, including the path inside the
    /// share. This is what the settings file remembers.
    pub fn location(&self) -> String {
        if self.inner.is_empty() {
            self.describe()
        } else {
            format!("{}/{}", self.describe(), self.inner)
        }
    }

    /// Where the database file is, given the local path the share's root is
    /// reachable at.
    ///
    /// The inner path is joined component by component rather than as one string,
    /// so a `/`-separated location becomes a native path on Windows and the join
    /// cannot be talked into an absolute path by a leading separator.
    pub fn database_path(&self, root: &Path) -> PathBuf {
        let mut path = root.to_path_buf();
        for component in self.inner.split('/').filter(|part| !part.is_empty()) {
            path.push(component);
        }
        path
    }
}

/// A parsed location: the share, plus the user name it carried, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharePath {
    pub target: ShareTarget,
    /// `DOMAIN\user` or `user`, when the location was written
    /// `smb://user@server/share`. Not part of the target's identity.
    pub user: Option<String>,
}

/// Does this look like an SMB location rather than a plain file path?
///
/// Used by the chooser to notice that the operator typed a share into the path
/// field, and to offer the share card instead of a failing open.
pub fn looks_like_smb_location(raw: &str) -> bool {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("smb://")
        || lower.starts_with("cifs://")
        || trimmed.starts_with(r"\\")
        || trimmed.starts_with("//")
}

/// Parse a location into a share and a path inside it.
///
/// Accepts every spelling an operator is likely to paste: `smb://` and `cifs://`
/// URLs, Windows UNC (`\\server\share\dir\file`), and the POSIX `//server/share/…`
/// form. Separators may be mixed, because a path copied out of a Windows dialog and
/// pasted into a macOS window regularly is.
pub fn parse(raw: &str) -> Result<SharePath> {
    let location = raw.trim().to_owned();
    if location.chars().count() > MAX_LOCATION {
        return Err(SmbError::TooLong {
            found: location.chars().count(),
            location: location.chars().take(60).collect(),
            max: MAX_LOCATION,
        });
    }
    if !looks_like_smb_location(&location) {
        return Err(SmbError::NotAShare { location });
    }

    // Normalise to `host[/share[/inner]]`: strip the scheme or the leading
    // separators, then treat `\` as `/`.
    let lower = location.to_ascii_lowercase();
    let body = if let Some(rest) = lower.strip_prefix("smb://") {
        &location[location.len() - rest.len()..]
    } else if let Some(rest) = lower.strip_prefix("cifs://") {
        &location[location.len() - rest.len()..]
    } else {
        location.trim_start_matches(['\\', '/'])
    };
    let body = body.replace('\\', "/");

    // `user@host` — everything before the last `@`, so a user name containing one
    // (an e-mail as a user name does) survives.
    let (user, rest) = match body.split_once('/') {
        Some((authority, tail)) => split_user(authority).map(|(user, host)| {
            let mut rest = String::with_capacity(host.len() + 1 + tail.len());
            rest.push_str(host);
            rest.push('/');
            rest.push_str(tail);
            (user, rest)
        })?,
        None => split_user(&body).map(|(user, host)| (user, host.to_owned()))?,
    };

    let mut parts = rest.split('/').filter(|part| !part.is_empty());
    let host = parts.next().unwrap_or_default();
    let share = parts.next();
    let inner: Vec<&str> = parts.collect();

    check_component("server name", host, &location)?;
    let Some(share) = share else {
        return Err(SmbError::NoShare { location });
    };
    check_component("share name", share, &location)?;
    for part in &inner {
        check_inner(part, &location)?;
    }

    Ok(SharePath {
        target: ShareTarget {
            host: host.to_owned(),
            share: share.to_owned(),
            inner: inner.join("/"),
        },
        user: user.map(decode_user),
    })
}

/// Split `user@host` into its two halves, at the **last** `@`.
fn split_user(authority: &str) -> Result<(Option<&str>, &str)> {
    match authority.rsplit_once('@') {
        Some((user, host)) if !user.is_empty() => Ok((Some(user), host)),
        // `@host` with no user is a typo, not an anonymous request: saying so is
        // better than guessing which of the two the operator meant.
        Some((_, host)) => Err(SmbError::BadComponent {
            part: "user name",
            location: host.to_owned(),
            reason: "there is an `@` with no user name in front of it".into(),
        }),
        None => Ok((None, authority)),
    }
}

/// `DOMAIN%5Cuser` is how a URL carries a backslash. Nothing else is decoded:
/// a full percent-decoder here would only invite a share name that decodes into a
/// separator.
fn decode_user(raw: &str) -> String {
    raw.replace("%5C", "\\").replace("%5c", "\\")
}

fn check_component(part: &'static str, value: &str, location: &str) -> Result<()> {
    let reason = if value.is_empty() {
        Some("it is empty".to_owned())
    } else if value.chars().count() > MAX_COMPONENT {
        Some(format!("it is longer than {MAX_COMPONENT} characters"))
    } else if let Some(bad) = value.chars().find(|c| c.is_control() || *c == '\0') {
        Some(format!(
            "it contains a control character ({:#04x})",
            bad as u32
        ))
    } else if value == "." || value == ".." {
        Some("it is a relative path element".to_owned())
    } else {
        None
    };

    match reason {
        Some(reason) => Err(SmbError::BadComponent {
            part,
            location: location.to_owned(),
            reason,
        }),
        None => Ok(()),
    }
}

/// A path component inside the share.
///
/// `..` is refused rather than resolved: the inner path is joined onto a mount
/// point, so a traversal would reach outside the share the operator named — and
/// "the register is somewhere else on the file server" is not a state this tool
/// should be able to produce from a typo.
fn check_inner(value: &str, location: &str) -> Result<()> {
    if value == ".." {
        return Err(SmbError::BadComponent {
            part: "path inside the share",
            location: location.to_owned(),
            reason: "it contains a `..` segment, which would point outside the share".into(),
        });
    }
    check_component("path inside the share", value, location)
}

// ----------------------------------------------------------------- credential

/// Which identity to present to the file server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Access {
    /// The credentials the operator is already signed in with.
    ///
    /// On Windows this is the session token, used implicitly: nothing is sent and
    /// no connection is made, because opening the UNC path authenticates the same
    /// way Explorer does. On macOS it is the Keychain entry for that server.
    #[default]
    LoggedOnUser,
    /// Guest access: no user name, no password. Deliberate, and labelled.
    Anonymous,
    /// A named account, whose password the operator types every time.
    Named,
}

impl Access {
    /// The stable name used in the settings file, the status line and the audit
    /// entry.
    pub fn label(&self) -> &'static str {
        match self {
            Access::LoggedOnUser => "the signed-in user",
            Access::Anonymous => "guest",
            Access::Named => "a named account",
        }
    }

    /// Everything the chooser offers, in the order it offers it.
    pub const ALL: [Access; 3] = [Access::LoggedOnUser, Access::Anonymous, Access::Named];
}

/// A password, kept as briefly and as quietly as possible.
///
/// Bytes rather than a `String` so the buffer can be overwritten on drop without
/// `unsafe`; redacted in `Debug` so no `{:?}` — including a `tracing` field on a
/// failed connection — can leak it; and readable only from inside this crate, so
/// no test can assert on it and no widget can echo it.
///
/// Best-effort, and honest about it: the operator's own `String` in the text field
/// is cleared by the caller, and a `String` that reallocated while being typed may
/// have left a copy behind. What this rules out is the *durable* leak — a log line,
/// a settings file, an argument vector, a panic message.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(value: &str) -> Self {
        Self(value.as_bytes().to_vec())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The value, for the one caller that has to hand it to the operating system.
    ///
    /// Crate-private on purpose. Invalid UTF-8 cannot occur — the only constructor
    /// takes a `&str` — but is answered with an empty string rather than a panic,
    /// because a panic message is one of the places a secret must never reach.
    pub(crate) fn expose(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("")
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(********)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// How to authenticate to the share.
#[derive(Debug, Clone, Default)]
pub struct Credential {
    pub access: Access,
    /// `DOMAIN\user` or `user`. Empty unless [`Access::Named`].
    pub user: String,
    password: Secret,
}

impl Credential {
    /// The default: whoever is signed in at this workstation.
    pub fn logged_on_user() -> Self {
        Self {
            access: Access::LoggedOnUser,
            user: String::new(),
            password: Secret::default(),
        }
    }

    /// Guest access.
    pub fn anonymous() -> Self {
        Self {
            access: Access::Anonymous,
            user: String::new(),
            password: Secret::default(),
        }
    }

    /// A named account. The password is consumed here and not kept anywhere else.
    pub fn named(user: &str, password: &str) -> Self {
        Self {
            access: Access::Named,
            user: user.trim().to_owned(),
            password: Secret::new(password),
        }
    }

    /// The identity, for a status line and an audit entry. Never the password.
    pub fn describe(&self) -> String {
        match self.access {
            Access::Named if !self.user.is_empty() => self.user.clone(),
            other => other.label().to_owned(),
        }
    }

    pub(crate) fn password(&self) -> &Secret {
        &self.password
    }
}

// ----------------------------------------------------------------- connectors

/// A share reached, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub target: ShareTarget,
    /// Local path at which the share's **root** is reachable.
    pub root: PathBuf,
    /// The identity that was presented, as [`Credential::describe`] words it.
    pub identity: String,
    /// This process made the connection, so this process takes it down.
    ///
    /// False for a share that was already mounted — by Finder, by a login script,
    /// by the operator five minutes ago. Unmounting somebody else's mount because
    /// this application happened to use it would be worse than not mounting at all.
    pub ours: bool,
}

impl Attachment {
    /// Where the database file is.
    pub fn database_path(&self) -> PathBuf {
        self.target.database_path(&self.root)
    }

    /// One line for the status bar.
    pub fn describe(&self) -> String {
        format!(
            "{} as {} ({})",
            self.target.describe(),
            self.identity,
            if self.ours {
                "connected by this application"
            } else {
                "already mounted"
            }
        )
    }
}

/// What a connector achieved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connected {
    /// Local path the share's root is reachable at.
    pub root: PathBuf,
    /// This process made the connection, so this process takes it down.
    pub ours: bool,
}

impl Connected {
    /// This session made the connection.
    pub fn ours(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            ours: true,
        }
    }

    /// The share turned out to be mounted already — a login script, another
    /// application, or a race with our own probe. Reported so it is not torn down
    /// on close.
    pub fn adopted(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            ours: false,
        }
    }
}

/// Makes a share reachable as a local path.
///
/// One implementation per platform, plus [`MockConnector`], which is what lets the
/// whole flow — connect, open, write, close, disconnect — be tested with no file
/// server anywhere near it.
pub trait Connector: std::fmt::Debug + Send + Sync {
    /// What this connector is, for `--diagnose` and for a log line.
    fn label(&self) -> &'static str;

    /// The local path this share is *already* reachable at, if it is.
    ///
    /// Every attach starts here, so a share the operator already mounted is used as
    /// it is and left alone afterwards — and so a credential is not sent to a
    /// server that did not need one.
    fn existing_root(&self, target: &ShareTarget) -> Option<PathBuf>;

    /// Make the connection. Only called when [`Connector::existing_root`] found
    /// nothing.
    fn connect(&self, target: &ShareTarget, credential: &Credential) -> Result<Connected>;

    /// Take down a connection this process made.
    fn disconnect(&self, attachment: &Attachment) -> Result<()>;
}

/// The connector for the platform this build runs on.
pub fn platform_connector() -> Box<dyn Connector> {
    #[cfg(windows)]
    {
        Box::new(windows::WNetConnector)
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::NetFsConnector)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Box::new(system::SystemMountConnector)
    }
}

/// Can this build make a connection itself, or only use one the system made?
///
/// Reported by `--diagnose` and said plainly on the chooser, because the answer
/// changes what the operator has to do before the register will open.
pub fn can_connect() -> bool {
    cfg!(any(windows, target_os = "macos"))
}

/// An attached share, held for as long as the database on it is open.
///
/// The `Drop` impl is a backstop, not the intended exit: [`ShareConnection::close`]
/// reports what happened, and the audit entry that records the disconnection has to
/// be written while the database is still open. Dropping tears the connection down
/// and logs any failure, because a process that is going away should not leave a
/// connection behind either.
pub struct ShareConnection {
    attachment: Attachment,
    connector: Box<dyn Connector>,
    released: bool,
}

impl std::fmt::Debug for ShareConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShareConnection")
            .field("attachment", &self.attachment)
            .field("connector", &self.connector.label())
            .finish()
    }
}

impl ShareConnection {
    /// Attach the share, adopting an existing mount when there is one.
    pub fn open(
        target: &ShareTarget,
        credential: &Credential,
        connector: Box<dyn Connector>,
    ) -> Result<Self> {
        let attachment = match connector.existing_root(target) {
            Some(root) => {
                tracing::info!(
                    event = "db.share.already_mounted",
                    share = %target.describe(),
                    root = %root.display(),
                );
                Attachment {
                    target: target.clone(),
                    root,
                    identity: "whoever mounted it".into(),
                    ours: false,
                }
            }
            None => {
                let connected = connector.connect(target, credential).inspect_err(|e| {
                    // No secret is in scope of this line: `identity` is a user name
                    // at most, and `Secret` redacts itself in any case.
                    tracing::error!(
                        event = "db.share.connect.failed",
                        share = %target.describe(),
                        identity = %credential.describe(),
                        connector = connector.label(),
                        reason = %e,
                    );
                })?;
                tracing::info!(
                    event = "db.share.connected",
                    share = %target.describe(),
                    identity = %credential.describe(),
                    connector = connector.label(),
                    root = %connected.root.display(),
                    ours = connected.ours,
                );
                Attachment {
                    target: target.clone(),
                    root: connected.root,
                    identity: credential.describe(),
                    ours: connected.ours,
                }
            }
        };

        Ok(Self {
            attachment,
            connector,
            released: false,
        })
    }

    pub fn attachment(&self) -> &Attachment {
        &self.attachment
    }

    pub fn target(&self) -> &ShareTarget {
        &self.attachment.target
    }

    /// Where the database file is.
    pub fn database_path(&self) -> PathBuf {
        self.attachment.database_path()
    }

    /// How to open the database on this share.
    ///
    /// States [`Location::NetworkShare`](crate::store::Location::NetworkShare)
    /// rather than leaving it to the path heuristic. The file *is* on a network
    /// filesystem — this connection is the proof — and the mount point the operating
    /// system chose is not something to reason from: macOS puts it under `/Volumes`,
    /// an autofs map under `/net`, and a test under a temporary directory. Guessing
    /// wrong would put a shared file in WAL mode, whose shared-memory sidecar cannot
    /// cross a network filesystem at all.
    pub fn store_config(&self) -> crate::store::StoreConfig {
        crate::store::StoreConfig::new(self.database_path())
            .with_location(crate::store::Location::NetworkShare)
    }

    /// One line for the status bar.
    pub fn describe(&self) -> String {
        self.attachment.describe()
    }

    /// Was this connection made by this session?
    pub fn is_ours(&self) -> bool {
        self.attachment.ours
    }

    /// Disconnect, reporting the outcome.
    ///
    /// A connection this process did not make is left exactly as it was found.
    pub fn close(mut self) -> Result<()> {
        self.release()
    }

    fn release(&mut self) -> Result<()> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        if !self.attachment.ours {
            tracing::info!(
                event = "db.share.left_mounted",
                share = %self.attachment.target.describe(),
                detail = "this session did not mount it, so it is not this session's to unmount",
            );
            return Ok(());
        }
        self.connector.disconnect(&self.attachment)?;
        tracing::info!(
            event = "db.share.disconnected",
            share = %self.attachment.target.describe(),
        );
        Ok(())
    }
}

impl Drop for ShareConnection {
    fn drop(&mut self) {
        if let Err(e) = self.release() {
            tracing::error!(
                event = "db.share.detach.failed",
                share = %self.attachment.target.describe(),
                reason = %e,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_a_location_reaches_the_same_share() {
        for raw in [
            "smb://fileserver/ti-share/yubikeys/keys.sqlite3",
            "SMB://fileserver/ti-share/yubikeys/keys.sqlite3",
            "cifs://fileserver/ti-share/yubikeys/keys.sqlite3",
            r"\\fileserver\ti-share\yubikeys\keys.sqlite3",
            "//fileserver/ti-share/yubikeys/keys.sqlite3",
            // Pasted from a Windows dialog into a macOS field, which happens.
            r"//fileserver\ti-share/yubikeys\keys.sqlite3",
            "smb://fileserver/ti-share/yubikeys/keys.sqlite3/",
        ] {
            let parsed = parse(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert_eq!(parsed.target.host, "fileserver", "{raw}");
            assert_eq!(parsed.target.share, "ti-share", "{raw}");
            assert_eq!(parsed.target.inner, "yubikeys/keys.sqlite3", "{raw}");
            assert_eq!(parsed.user, None, "{raw}");
        }
    }

    #[test]
    fn a_location_may_carry_a_user_without_it_becoming_part_of_the_share() {
        let plain = parse("smb://fileserver/ti-share/keys.sqlite3").unwrap();
        let with_user = parse("smb://felipe@fileserver/ti-share/keys.sqlite3").unwrap();
        assert_eq!(with_user.user.as_deref(), Some("felipe"));
        // Two accounts, one share: the identity of the target must not include
        // the user, or the recent list and the connection bookkeeping fork.
        assert_eq!(with_user.target, plain.target);

        // A URL cannot carry a raw backslash, so a domain arrives percent-encoded.
        let domain = parse(r"smb://FGV%5Cfelipe@fileserver/ti-share").unwrap();
        assert_eq!(domain.user.as_deref(), Some(r"FGV\felipe"));

        // An e-mail as a user name contains an `@` of its own.
        let email = parse("smb://felipe@fgv.br@fileserver/ti-share").unwrap();
        assert_eq!(email.user.as_deref(), Some("felipe@fgv.br"));
        assert_eq!(email.target.host, "fileserver");
    }

    #[test]
    fn a_share_in_the_root_of_the_share_has_no_inner_path() {
        let parsed = parse(r"\\nas\public").unwrap();
        assert_eq!(parsed.target.inner, "");
        assert_eq!(parsed.target.location(), "//nas/public");
    }

    #[test]
    fn a_location_that_is_not_a_share_is_refused_as_such() {
        let error = parse("/Volumes/ti-share/keys.sqlite3").unwrap_err();
        assert!(matches!(error, SmbError::NotAShare { .. }), "{error}");
        assert!(error.to_string().contains("smb://server/share"));
    }

    #[test]
    fn a_server_without_a_share_is_refused_because_it_cannot_be_guessed() {
        for raw in ["smb://fileserver", r"\\fileserver", "smb://fileserver/"] {
            let error = parse(raw).unwrap_err();
            assert!(matches!(error, SmbError::NoShare { .. }), "{raw}: {error}");
        }
    }

    #[test]
    fn a_traversal_out_of_the_share_is_refused() {
        let error = parse(r"\\fileserver\ti-share\..\..\etc\passwd").unwrap_err();
        let message = error.to_string();
        assert!(message.contains(".."), "{message}");
        assert!(
            message.contains("outside the share"),
            "the refusal must say what it is preventing: {message}"
        );
    }

    #[test]
    fn a_control_character_or_an_empty_component_is_refused() {
        let nul = parse("smb://file\0server/share").unwrap_err();
        assert!(nul.to_string().contains("control character"), "{nul}");

        // `@` with nothing in front of it is a typo, not a request for guest access.
        let empty_user = parse("smb://@fileserver/share").unwrap_err();
        assert!(empty_user.to_string().contains("user name"), "{empty_user}");
    }

    #[test]
    fn an_over_long_location_is_refused_before_it_reaches_a_system_call() {
        let long = format!("smb://host/share/{}", "a".repeat(MAX_LOCATION));
        let error = parse(&long).unwrap_err();
        assert!(matches!(error, SmbError::TooLong { .. }), "{error}");
        // And the message does not itself become the novel it is refusing.
        assert!(error.to_string().len() < 200);
    }

    #[test]
    fn the_database_path_is_joined_component_by_component() {
        let target = parse("smb://nas/public/yubikeys/keys.sqlite3")
            .unwrap()
            .target;
        let path = target.database_path(Path::new("/Volumes/public"));
        assert_eq!(
            path,
            Path::new("/Volumes/public")
                .join("yubikeys")
                .join("keys.sqlite3")
        );
    }

    #[test]
    fn a_secret_is_redacted_in_debug_and_zeroed_on_drop() {
        let secret = Secret::new("not-in-a-log-line");
        assert_eq!(format!("{secret:?}"), "Secret(********)");
        assert_eq!(secret.expose(), "not-in-a-log-line");

        // The credential that carries it must not print it either — this is the
        // line a `tracing` field or a `dbg!` would produce.
        let credential = Credential::named("felipe", "not-in-a-log-line");
        let rendered = format!("{credential:?}");
        assert!(!rendered.contains("not-in-a-log-line"), "{rendered}");
        assert!(rendered.contains("Secret(********)"), "{rendered}");
        assert!(!credential.describe().contains("not-in-a-log-line"));

        // And the buffer is cleared rather than handed back to the allocator with
        // the password still in it.
        let mut owned = Secret::new("abc");
        owned.0.fill(0);
        assert_eq!(owned.0, vec![0, 0, 0]);
    }

    #[test]
    fn the_default_access_is_the_signed_in_user() {
        assert_eq!(Access::default(), Access::LoggedOnUser);
        assert_eq!(Credential::default().access, Access::LoggedOnUser);
        assert_eq!(
            Credential::logged_on_user().describe(),
            "the signed-in user"
        );
        assert_eq!(Credential::anonymous().describe(), "guest");
        assert_eq!(
            Credential::named("FGV\\felipe", "x").describe(),
            "FGV\\felipe"
        );
    }

    #[test]
    fn the_access_mode_round_trips_through_the_settings_format() {
        // The settings file is JSON, and a hand-edited or older file must still
        // load: the names are part of the format.
        assert_eq!(
            serde_json::to_string(&Access::LoggedOnUser).unwrap(),
            "\"logged-on-user\""
        );
        assert_eq!(
            serde_json::from_str::<Access>("\"anonymous\"").unwrap(),
            Access::Anonymous
        );
    }

    #[test]
    fn a_share_this_process_did_not_mount_is_left_alone() {
        let target = parse(r"\\nas\public\keys.sqlite3").unwrap().target;
        let connector = MockConnector::adopting("/mnt/public");
        let calls = connector.calls();
        let connection =
            ShareConnection::open(&target, &Credential::anonymous(), Box::new(connector)).unwrap();

        assert!(!connection.is_ours());
        assert_eq!(
            connection.database_path(),
            Path::new("/mnt/public/keys.sqlite3")
        );
        connection.close().unwrap();
        assert_eq!(calls.lock().unwrap().as_slice(), ["existing_root"]);
    }

    #[test]
    fn a_share_this_process_connected_is_disconnected_once() {
        let target = parse("smb://nas/public").unwrap().target;
        let connector = MockConnector::connecting("/Volumes/public");
        let calls = connector.calls();
        let connection = ShareConnection::open(
            &target,
            &Credential::named("felipe", "typed-once"),
            Box::new(connector),
        )
        .unwrap();

        assert!(connection.is_ours());
        connection.close().unwrap();
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            ["existing_root", "connect", "disconnect"],
            "closing disconnects, and dropping afterwards must not do it again"
        );
    }

    #[test]
    fn dropping_a_connection_still_disconnects() {
        let target = parse("smb://nas/public").unwrap().target;
        let connector = MockConnector::connecting("/Volumes/public");
        let calls = connector.calls();
        {
            let _connection =
                ShareConnection::open(&target, &Credential::anonymous(), Box::new(connector))
                    .unwrap();
        }
        assert!(calls.lock().unwrap().contains(&"disconnect".to_owned()));
    }

    #[test]
    fn a_refused_credential_leaves_nothing_attached() {
        let target = parse("smb://nas/private").unwrap().target;
        let connector = MockConnector::refusing("the user name or password was refused");
        let calls = connector.calls();
        let error = ShareConnection::open(
            &target,
            &Credential::named("felipe", "wrong"),
            Box::new(connector),
        )
        .unwrap_err();

        assert!(error.to_string().contains("//nas/private"), "{error}");
        assert!(
            !calls.lock().unwrap().contains(&"disconnect".to_owned()),
            "nothing was attached, so nothing may be detached"
        );
    }

    #[test]
    fn the_status_line_says_which_share_and_whose_credentials() {
        let target = parse("smb://nas/public").unwrap().target;
        let connection = ShareConnection::open(
            &target,
            &Credential::anonymous(),
            Box::new(MockConnector::connecting("/Volumes/public")),
        )
        .unwrap();
        let line = connection.describe();
        assert!(line.contains("//nas/public"), "{line}");
        assert!(line.contains("guest"), "{line}");
        assert!(line.contains("connected by this application"), "{line}");
    }
}
