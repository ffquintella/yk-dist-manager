//! macOS: mount an SMB share through **NetFS.framework**.
//!
//! `NetFSMountURLSync` is the API behind Finder's "Connect to Server". It is chosen
//! over `mount_smbfs` for one reason that is not negotiable here: it takes the
//! password as a `CFString` in this process's memory, while `mount_smbfs` takes it
//! in the URL — that is, in an argument vector every process on the workstation can
//! read (`ps`), which is exactly the durable leak AGENTS.md §2 forbids. The
//! alternatives are no better: a credentials file is the temporary file the same
//! rule forbids, and `mount_smbfs`'s interactive prompt reads `/dev/tty`, which a
//! windowed application does not have.
//!
//! Two more decisions worth stating:
//!
//! * **No mount point is chosen here.** NetFS is asked to mount with no path, which
//!   puts the share under `/Volumes/<share>` through the automounter and reports
//!   back where it landed — the same thing Finder does. Choosing a directory would
//!   mean creating one, deciding what to do when it is occupied, and cleaning it up
//!   after a crash: three problems the operating system has already solved.
//! * **An existing mount is found by reading `/sbin/mount`,** not by guessing
//!   `/Volumes/<share>`. The share name is not unique — two servers can both export
//!   `public`, and the automounter appends `-1` — so the only safe check is the
//!   `//user@HOST/share` device string the mount actually carries.

use std::path::PathBuf;
use std::ptr;

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::string::CFString;
use core_foundation::url::CFURL;
use core_foundation_sys::array::CFArrayRef;
use core_foundation_sys::string::CFStringRef;
use core_foundation_sys::url::{CFURLCreateWithString, CFURLRef};

use super::{
    Access, Attachment, Connected, Connector, Credential, Result, ShareTarget, SmbError, mounts,
};

/// Unmounting a share this session mounted. No secret is involved, so an argument
/// vector is the right tool, and `umount` is the documented way to take an smbfs
/// mount down — NetFS exposes no public unmount.
const UMOUNT: &str = "/sbin/umount";

/// The filesystem name an SMB mount appears under on macOS.
const FILESYSTEMS: [&str; 1] = ["smbfs"];

/// The user name that asks an SMB server for guest access.
///
/// Upper case because that is the name the SMB client itself uses, and it is what
/// shows up in the mount table afterwards.
const GUEST: &str = "GUEST";

#[link(name = "NetFS", kind = "framework")]
unsafe extern "C" {
    /// `NetFSMountURLSync(url, mountpath, user, passwd, open_options,
    /// mount_options, mountpoints)`
    ///
    /// Returns 0 on success, otherwise an `errno`. `user` and `passwd` may be null,
    /// which means "use whatever credentials the system already has for this
    /// server" — the Keychain entry, which is what [`Access::LoggedOnUser`] means
    /// on this platform. `mountpath` may be null, which mounts under `/Volumes`.
    /// On success `mountpoints` receives a `CFArrayRef` the caller owns.
    fn NetFSMountURLSync(
        url: CFURLRef,
        mountpath: CFURLRef,
        user: CFStringRef,
        passwd: CFStringRef,
        open_options: *const std::ffi::c_void,
        mount_options: *const std::ffi::c_void,
        mountpoints: *mut CFArrayRef,
    ) -> i32;
}

#[derive(Debug)]
pub struct NetFsConnector;

impl Connector for NetFsConnector {
    fn label(&self) -> &'static str {
        "NetFS (macOS)"
    }

    fn existing_root(&self, target: &ShareTarget) -> Option<PathBuf> {
        mounts::find(
            &mounts::read_table()?,
            &target.host,
            &target.share,
            &FILESYSTEMS,
        )
    }

    fn connect(&self, target: &ShareTarget, credential: &Credential) -> Result<Connected> {
        let url = share_url(target)?;

        // `None` means "pass null", which is how NetFS is told to use the
        // credentials the system already holds. It is deliberately *not* the same
        // as an empty string, which would present an empty user name.
        let (user, password) = match credential.access {
            Access::LoggedOnUser => (None, None),
            Access::Anonymous => (Some(CFString::new(GUEST)), Some(CFString::new(""))),
            Access::Named => (
                Some(CFString::new(&credential.user)),
                Some(CFString::new(credential.password().expose())),
            ),
        };
        let user_ref = user
            .as_ref()
            .map_or(ptr::null(), |value| value.as_concrete_TypeRef());
        let password_ref = password
            .as_ref()
            .map_or(ptr::null(), |value| value.as_concrete_TypeRef());

        let mut mountpoints: CFArrayRef = ptr::null();
        // SAFETY: every pointer is either null (documented as accepted for these
        // parameters) or a live CoreFoundation object owned by this scope, and
        // `mountpoints` is a valid out-pointer. The call is synchronous, so
        // nothing outlives this frame.
        let status = unsafe {
            NetFSMountURLSync(
                url.as_concrete_TypeRef(),
                ptr::null(),
                user_ref,
                password_ref,
                ptr::null(),
                ptr::null(),
                &mut mountpoints,
            )
        };
        // The password's CFString goes out of scope at the end of this function
        // either way; drop it as soon as the call is over.
        drop(password);

        let mounted = take_mountpoints(mountpoints);
        if status == 0 {
            return match mounted.first() {
                Some(root) => Ok(Connected::ours(root.clone())),
                // Mounted, but the framework did not say where. The mount table
                // knows, and reading it is cheaper than failing an operator who
                // now has a mounted share.
                None => match self.existing_root(target) {
                    Some(root) => Ok(Connected::ours(root)),
                    None => Err(SmbError::Unreachable {
                        share: target.describe(),
                        reason: "the share was mounted but macOS did not report where — check \
                                 Finder, then open the database by path"
                            .into(),
                    }),
                },
            };
        }

        // Already mounted between the probe and the call, or by somebody else's
        // login script a moment ago. That is a success, but not one this session
        // may unmount afterwards.
        if status == EEXIST
            && let Some(root) = self.existing_root(target)
        {
            return Ok(Connected::adopted(root));
        }

        Err(translate(status, target, credential))
    }

    fn disconnect(&self, attachment: &Attachment) -> Result<()> {
        let output = std::process::Command::new(UMOUNT)
            .arg(&attachment.root)
            .output()
            .map_err(|e| SmbError::DetachFailed {
                share: attachment.target.describe(),
                reason: format!("could not run {UMOUNT}: {e}"),
            })?;

        if output.status.success() {
            return Ok(());
        }
        Err(SmbError::DetachFailed {
            share: attachment.target.describe(),
            reason: first_line(&String::from_utf8_lossy(&output.stderr))
                .unwrap_or_else(|| format!("{UMOUNT} exited with {}", output.status)),
        })
    }
}

/// `smb://server/share` as a `CFURL`.
fn share_url(target: &ShareTarget) -> Result<CFURL> {
    let text = CFString::new(&target.url());
    // SAFETY: `text` is a live CFString for the duration of the call, and a null
    // allocator and null base URL are both documented as accepted.
    let raw =
        unsafe { CFURLCreateWithString(ptr::null(), text.as_concrete_TypeRef(), ptr::null()) };
    if raw.is_null() {
        return Err(SmbError::BadComponent {
            part: "location",
            location: target.url(),
            reason: "macOS could not read it as a URL".into(),
        });
    }
    // SAFETY: `CFURLCreateWithString` follows the create rule, so this takes the
    // one reference it returned and releases it on drop.
    Ok(unsafe { CFURL::wrap_under_create_rule(raw) })
}

/// Take ownership of the mountpoint array and read the paths out of it.
///
/// Anything that is not a string is skipped rather than trusted: the array's
/// contents are documented but not enforced, and a wrong `downcast` would be a
/// crash in front of an operator.
fn take_mountpoints(raw: CFArrayRef) -> Vec<PathBuf> {
    if raw.is_null() {
        return Vec::new();
    }
    // SAFETY: non-null, and the framework follows the create rule for this
    // out-parameter, so the array is released when this wrapper drops.
    let array: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(raw) };
    array
        .iter()
        .filter_map(|item| item.downcast::<CFString>())
        .map(|path| PathBuf::from(path.to_string()))
        .collect()
}

/// `EEXIST` — the share is already mounted.
const EEXIST: i32 = 17;

/// Turn an `errno` from NetFS into the sentence an operator can act on.
///
/// The number is kept in the message: three of these mean "ask the person who owns
/// the share", and a support ticket that quotes the code is easier to answer than
/// one that quotes a paraphrase.
fn translate(status: i32, target: &ShareTarget, credential: &Credential) -> SmbError {
    let share = target.describe();
    let identity = credential.describe();
    match status {
        // EPERM, EACCES, EAUTH, ENEEDAUTH
        1 | 13 | 80 | 81 => SmbError::Refused {
            share,
            identity,
            reason: match credential.access {
                Access::LoggedOnUser => format!(
                    "macOS has no accepted credentials for this server (errno {status}) — choose \
                     a named account, or guest if the share allows it"
                ),
                Access::Anonymous => {
                    format!("the share does not allow guest access (errno {status})")
                }
                Access::Named => format!("the user name or password was refused (errno {status})"),
            },
        },
        // ENOENT, ENODEV, ENETDOWN, ENETUNREACH, ETIMEDOUT, ECONNREFUSED,
        // EHOSTDOWN, EHOSTUNREACH
        2 | 19 | 50 | 51 | 60 | 61 | 64 | 65 => SmbError::Unreachable {
            share,
            reason: format!(
                "the server or the share name could not be reached (errno {status}) — check the \
                 name, and that this workstation is on the right network"
            ),
        },
        other => SmbError::Unreachable {
            share,
            reason: format!("macOS refused the mount with errno {other}"),
        },
    }
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_refusal_names_the_share_and_tells_the_operator_what_to_change() {
        let target = crate::store::smb::parse("smb://nas/private")
            .unwrap()
            .target;

        let wrong_password = translate(80, &target, &Credential::named("felipe", "x"));
        assert!(
            wrong_password.to_string().contains("user name or password"),
            "{wrong_password}"
        );

        // The same errno means something else entirely when we presented no
        // credentials at all, and the instruction has to differ with it.
        let no_keychain_entry = translate(80, &target, &Credential::logged_on_user());
        assert!(
            no_keychain_entry.to_string().contains("named account"),
            "{no_keychain_entry}"
        );

        let no_guest = translate(13, &target, &Credential::anonymous());
        assert!(no_guest.to_string().contains("guest"), "{no_guest}");

        let no_server = translate(65, &target, &Credential::logged_on_user());
        assert!(
            no_server.to_string().contains("right network"),
            "{no_server}"
        );

        // An unmapped code still says which share and still quotes the number.
        let odd = translate(9999, &target, &Credential::logged_on_user());
        assert!(odd.to_string().contains("//nas/private"), "{odd}");
        assert!(odd.to_string().contains("9999"), "{odd}");
    }

    #[test]
    fn a_null_mountpoint_array_is_not_dereferenced() {
        assert!(take_mountpoints(ptr::null()).is_empty());
    }

    #[test]
    fn a_mountpoint_array_is_read_and_anything_that_is_not_a_path_is_skipped() {
        // The framework documents an array of strings but does not enforce it, and a
        // wrong `downcast` would be a crash in front of an operator mid-hand-over.
        let mixed = CFArray::from_CFTypes(&[
            CFString::new("/Volumes/ti-share").as_CFType(),
            core_foundation::number::CFNumber::from(42).as_CFType(),
            CFString::new("/Volumes/ti-share-1").as_CFType(),
        ]);
        // `take_mountpoints` follows the create rule, so hand it a retained copy
        // rather than the one this scope owns.
        let raw = mixed.as_concrete_TypeRef();
        std::mem::forget(mixed.clone());
        let paths = take_mountpoints(raw);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/Volumes/ti-share"),
                PathBuf::from("/Volumes/ti-share-1"),
            ]
        );
    }

    #[test]
    fn a_share_becomes_the_url_netfs_expects() {
        // This is the one place the location is handed to the operating system, so
        // the spelling matters: `smb://server/share`, whatever the operator typed.
        let target = crate::store::smb::parse(r"\\fileserver\ti-share\yubikeys\keys.sqlite3")
            .unwrap()
            .target;
        let url = share_url(&target).expect("a plain share name is a valid URL");
        assert_eq!(url.get_string().to_string(), "smb://fileserver/ti-share");
    }

    #[test]
    fn a_stderr_line_from_umount_is_reported_as_one_line() {
        assert_eq!(
            first_line("umount(/Volumes/ti-share): Resource busy\n\n"),
            Some("umount(/Volumes/ti-share): Resource busy".to_owned())
        );
        assert_eq!(first_line("   \n\n"), None);
        assert_eq!(first_line(""), None);
    }
}
