//! Windows: connect to an SMB share through **`WNetAddConnection2W`**.
//!
//! Three decisions, each of which is the whole reason this file exists.
//!
//! **[`Access::LoggedOnUser`] makes no call at all.** A UNC path opened by a
//! process running as the signed-in user is authenticated by Windows the same way
//! Explorer's is — Kerberos, or NTLM, against the account already logged on. The
//! correct implementation of "use the credentials the operator is already signed in
//! with" is therefore to open the path and let the operating system do its job.
//! Calling the API with empty credentials would be worse than nothing: it would ask
//! for a *new* session to the same server and can collide with one Windows already
//! has (`ERROR_SESSION_CREDENTIAL_CONFLICT`).
//!
//! **No drive letter.** The connection is *deviceless*: `lpLocalName` is null, so
//! nothing is mapped and the UNC path simply starts working. A mapped letter would
//! be per-session state that drifts — `Z:` is a different share on the next
//! workstation — and the register's location must mean the same thing everywhere.
//!
//! **The password goes through the API, not a command line.** `net use` would put it
//! in an argument vector every process in the session can read, which is the durable
//! leak AGENTS.md §2 forbids. `WNetAddConnection2W` takes it as a wide string in
//! this process's memory, which is what a [`Secret`](super::Secret) can supply.

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_ALREADY_ASSIGNED, ERROR_BAD_NET_NAME, ERROR_BAD_NETPATH,
    ERROR_DEVICE_ALREADY_REMEMBERED, ERROR_INVALID_PASSWORD, ERROR_LOGON_FAILURE,
    ERROR_NO_NET_OR_BAD_PATH, ERROR_NO_NETWORK, ERROR_SESSION_CREDENTIAL_CONFLICT, NO_ERROR,
};
use windows_sys::Win32::NetworkManagement::WNet::{
    CONNECT_TEMPORARY, NETRESOURCEW, RESOURCETYPE_DISK, WNetAddConnection2W, WNetCancelConnection2W,
};

use super::{Access, Attachment, Connected, Connector, Credential, Result, ShareTarget, SmbError};

#[derive(Debug)]
pub struct WNetConnector;

impl Connector for WNetConnector {
    fn label(&self) -> &'static str {
        "WNet (Windows)"
    }

    fn existing_root(&self, target: &ShareTarget) -> Option<PathBuf> {
        let root = PathBuf::from(target.unc());
        // A readable directory at `\\server\share` means the share is already
        // usable in this session — either Windows authenticated us implicitly, or a
        // connection is already open. Either way there is nothing to add, and
        // nothing this session should later take down.
        std::fs::metadata(&root).is_ok().then_some(root)
    }

    fn connect(&self, target: &ShareTarget, credential: &Credential) -> Result<Connected> {
        let root = PathBuf::from(target.unc());

        // The signed-in user is not a credential to present; it is the absence of
        // one. If the path is not already reachable as this user, no API call will
        // change that — what changes it is choosing another identity.
        if credential.access == Access::LoggedOnUser {
            return match std::fs::metadata(&root) {
                Ok(_) => Ok(Connected::adopted(root)),
                Err(e) => Err(SmbError::Refused {
                    share: target.describe(),
                    identity: credential.describe(),
                    reason: format!(
                        "Windows could not open it as the signed-in user ({e}) — if this share \
                         needs another account, choose a named one; if it allows guests, choose \
                         guest"
                    ),
                }),
            };
        }

        let mut remote = wide(&target.unc());
        let resource = NETRESOURCEW {
            dwScope: 0,
            dwType: RESOURCETYPE_DISK,
            dwDisplayType: 0,
            dwUsage: 0,
            lpLocalName: std::ptr::null_mut(),
            lpRemoteName: remote.as_mut_ptr(),
            lpComment: std::ptr::null_mut(),
            lpProvider: std::ptr::null_mut(),
        };

        // Guest access is an empty user name with an empty password. `Named` sends
        // what the operator typed, `DOMAIN\user` included — Windows parses that
        // itself, so nothing here has to guess where the domain ends.
        let user = wide(&credential.user);
        let mut password = wide(credential.password().expose());

        // SAFETY: `resource`, `user` and `password` are live, NUL-terminated wide
        // buffers owned by this frame, and the call is synchronous. `CONNECT_TEMPORARY`
        // keeps the connection out of the user's remembered connections, so nothing
        // this application does outlives the process in the operator's profile.
        let status = unsafe {
            WNetAddConnection2W(
                &resource,
                password.as_ptr(),
                user.as_ptr(),
                CONNECT_TEMPORARY,
            )
        };

        // Overwrite the wide copy of the password as soon as the call is over. The
        // `Secret` guards its own buffer; this one is the copy the API needed, and
        // it is this function's to clear before the allocator gets it back. The
        // buffers themselves are freed at the end of the scope, in reverse order of
        // declaration, so `resource` stops existing before the buffer its
        // `lpRemoteName` points into.
        password.fill(0);

        if status == NO_ERROR {
            return Ok(Connected::ours(root));
        }
        // Already connected — by a login script, by Explorer, or by a race with our
        // own probe. A success, but not one this session may disconnect.
        if status == ERROR_ALREADY_ASSIGNED || status == ERROR_DEVICE_ALREADY_REMEMBERED {
            return Ok(Connected::adopted(root));
        }
        Err(translate(status, target, credential))
    }

    fn disconnect(&self, attachment: &Attachment) -> Result<()> {
        let name = wide(&attachment.target.unc());
        // SAFETY: a live NUL-terminated wide buffer; `force = FALSE` so an open
        // handle refuses the disconnection rather than having it pulled away.
        let status = unsafe { WNetCancelConnection2W(name.as_ptr(), 0, 0) };
        if status == NO_ERROR {
            return Ok(());
        }
        Err(SmbError::DetachFailed {
            share: attachment.target.describe(),
            reason: format!("Windows refused to disconnect (error {status})"),
        })
    }
}

/// A NUL-terminated UTF-16 buffer, which is what the `…W` entry points take.
fn wide(value: &str) -> Vec<u16> {
    Path::new(value)
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Turn a Win32 error into the sentence an operator can act on.
///
/// The distinction matters more here than the number does: "wrong password",
/// "right password, wrong permissions" and "wrong server name" are three different
/// things to go and do, and Windows tells them apart.
fn translate(status: u32, target: &ShareTarget, credential: &Credential) -> SmbError {
    let share = target.describe();
    let identity = credential.describe();
    match status {
        ERROR_LOGON_FAILURE | ERROR_INVALID_PASSWORD => SmbError::Refused {
            share,
            identity,
            reason: format!("the user name or password was refused (error {status})"),
        },
        ERROR_ACCESS_DENIED => SmbError::Refused {
            share,
            identity,
            reason: format!(
                "the account was accepted but may not use this share (error {status}) — ask \
                 whoever owns the share for access"
            ),
        },
        ERROR_SESSION_CREDENTIAL_CONFLICT => SmbError::Refused {
            share,
            identity,
            reason: format!(
                "Windows already has a connection to this server as another user (error \
                 {status}) — that connection has to be closed first (`net use \
                 <server-share> /delete`), or use the signed-in user's credentials"
            ),
        },
        ERROR_BAD_NETPATH | ERROR_BAD_NET_NAME | ERROR_NO_NET_OR_BAD_PATH => {
            SmbError::Unreachable {
                share,
                reason: format!(
                    "the server or the share name is wrong, or the server is not answering \
                     (error {status})"
                ),
            }
        }
        ERROR_NO_NETWORK => SmbError::Unreachable {
            share,
            reason: format!("this workstation has no network (error {status})"),
        },
        other => SmbError::Unreachable {
            share,
            reason: format!("Windows refused the connection with error {other}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_string_is_nul_terminated() {
        let encoded = wide(r"\\server\share");
        assert_eq!(encoded.last(), Some(&0));
        assert_eq!(encoded.len(), r"\\server\share".len() + 1);
    }

    #[test]
    fn every_refusal_names_the_share_and_what_to_change() {
        let target = crate::store::smb::parse(r"\\fileserver\ti-share")
            .unwrap()
            .target;
        let named = Credential::named(r"FGV\felipe", "x");

        let wrong = translate(ERROR_LOGON_FAILURE, &target, &named);
        assert!(
            wrong.to_string().contains("user name or password"),
            "{wrong}"
        );
        assert!(
            wrong.to_string().contains("//fileserver/ti-share"),
            "{wrong}"
        );

        let denied = translate(ERROR_ACCESS_DENIED, &target, &named);
        assert!(
            denied.to_string().contains("may not use this share"),
            "{denied}"
        );

        let conflict = translate(ERROR_SESSION_CREDENTIAL_CONFLICT, &target, &named);
        assert!(conflict.to_string().contains("another user"), "{conflict}");
        assert!(conflict.to_string().contains("/delete"), "{conflict}");

        let bad_path = translate(ERROR_BAD_NETPATH, &target, &named);
        assert!(
            bad_path.to_string().contains("share name is wrong"),
            "{bad_path}"
        );

        let unknown = translate(4242, &target, &named);
        assert!(unknown.to_string().contains("4242"), "{unknown}");
    }

    #[test]
    fn no_refusal_can_carry_the_password() {
        let target = crate::store::smb::parse(r"\\fileserver\ti-share")
            .unwrap()
            .target;
        let named = Credential::named("felipe", "not-in-a-message");
        for status in [
            ERROR_LOGON_FAILURE,
            ERROR_ACCESS_DENIED,
            ERROR_SESSION_CREDENTIAL_CONFLICT,
            ERROR_BAD_NETPATH,
            ERROR_NO_NETWORK,
            4242,
        ] {
            let message = translate(status, &target, &named).to_string();
            assert!(!message.contains("not-in-a-message"), "{message}");
        }
    }
}
