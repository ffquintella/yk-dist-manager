//! The share is the **system's** to mount, and this connector says so.
//!
//! Used on Linux and anything that is neither Windows nor macOS. Not an oversight,
//! and not laziness. Mounting CIFS on Linux means `mount.cifs`, which needs root or
//! a `setuid` helper, and passing it a password means either an argument vector
//! (readable by every process) or a credentials file — the temporary file
//! AGENTS.md §2 forbids. The desktop alternative, `gio mount`, prompts interactively
//! and hands the mount to a per-session GVFS daemon, which puts the database behind
//! a FUSE layer SQLite has no business trying to lock through.
//!
//! So this platform gets the honest half: an **already-mounted** share is found and
//! used, and a request to connect one is refused with the instruction that actually
//! works. The refusal names `mount.cifs`, `fstab` and `autofs`, because the operator
//! reading it has to hand this to whoever administers the workstation.
//!
//! Compiled everywhere, so its parsing and its refusal are tested everywhere.

use std::path::PathBuf;

use super::{Attachment, Connected, Connector, Credential, Result, ShareTarget, SmbError, mounts};

/// The filesystem names a Linux SMB mount appears under.
const FILESYSTEMS: [&str; 3] = ["cifs", "smb3", "smbfs"];

/// Mount roots to look under when the mount table cannot be read.
///
/// `/mnt` and `/media` are the conventional ones, and `/net` is what an autofs
/// `-hosts` map produces. A candidate still has to be a directory: an empty mount
/// point with nothing mounted on it is not a share.
const ROOTS: [&str; 3] = ["/mnt", "/media", "/net"];

#[derive(Debug)]
pub struct SystemMountConnector;

impl Connector for SystemMountConnector {
    fn label(&self) -> &'static str {
        "system mounts only"
    }

    fn existing_root(&self, target: &ShareTarget) -> Option<PathBuf> {
        if let Some(table) = mounts::read_table()
            && let Some(root) = mounts::find(&table, &target.host, &target.share, &FILESYSTEMS)
        {
            return Some(root);
        }
        // The mount table is the authority; the conventional locations are a
        // fallback for a system whose `mount` output this parser does not know.
        candidates(target).into_iter().find(|path| path.is_dir())
    }

    fn connect(&self, target: &ShareTarget, _credential: &Credential) -> Result<Connected> {
        Err(SmbError::Unsupported {
            share: target.describe(),
            reason: format!(
                "an unprivileged process cannot mount a CIFS share on this platform. Ask for it \
                 to be mounted by the system — `mount.cifs {} <mountpoint>` from `/etc/fstab`, or \
                 an `autofs` map — and then open the database by its path. An existing mount is \
                 looked for in the mount table, and under {}.",
                target.describe(),
                ROOTS.join(", ")
            ),
        })
    }

    fn disconnect(&self, attachment: &Attachment) -> Result<()> {
        // Nothing was ever connected here, so nothing can be disconnected. Reaching
        // this means an `Attachment` claimed `ours` on a platform that cannot make
        // one, which is a bug worth a log line rather than a silent success.
        tracing::warn!(
            event = "db.share.detach.unsupported",
            share = %attachment.target.describe(),
        );
        Ok(())
    }
}

/// The mount points a share of this name could plausibly be at.
fn candidates(target: &ShareTarget) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = ROOTS
        .iter()
        .map(|root| PathBuf::from(root).join(&target.share))
        .collect();
    // An autofs `-hosts` map mounts every share of a server under the server's name.
    out.push(PathBuf::from("/net").join(&target.host).join(&target.share));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> ShareTarget {
        crate::store::smb::parse("smb://fileserver/ti-share/keys.sqlite3")
            .unwrap()
            .target
    }

    #[test]
    fn the_refusal_names_the_alternative_rather_than_just_failing() {
        let error = SystemMountConnector
            .connect(&target(), &Credential::anonymous())
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("mount.cifs"), "{message}");
        assert!(
            message.contains("fstab") || message.contains("autofs"),
            "{message}"
        );
        assert!(message.contains("//fileserver/ti-share"), "{message}");
        assert!(matches!(error, SmbError::Unsupported { .. }));
    }

    #[test]
    fn an_already_mounted_share_is_looked_for_where_they_actually_are() {
        let paths = candidates(&target());
        assert!(paths.contains(&PathBuf::from("/mnt/ti-share")));
        assert!(paths.contains(&PathBuf::from("/media/ti-share")));
        assert!(paths.contains(&PathBuf::from("/net/fileserver/ti-share")));
    }

    #[test]
    fn a_mount_point_that_is_not_there_is_not_offered() {
        // No test environment has this share mounted, so the probe must answer
        // "no" rather than hand back the first candidate it thought of.
        assert_eq!(SystemMountConnector.existing_root(&target()), None);
    }

    #[test]
    fn a_share_this_connector_never_made_is_not_torn_down_loudly() {
        // Reaching `disconnect` at all is a bug elsewhere; it must still not fail
        // the close path an operator is waiting on.
        let attachment = Attachment {
            target: target(),
            root: PathBuf::from("/mnt/ti-share"),
            identity: "guest".into(),
            ours: false,
        };
        assert!(SystemMountConnector.disconnect(&attachment).is_ok());
    }
}
