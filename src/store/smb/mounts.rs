//! Finding a share that is **already mounted**, by reading the mount table.
//!
//! Every connector probes before it connects, so an operator who already has the
//! share — from Finder, a login script, `fstab` — is not asked for a credential and
//! does not have their mount taken down when the register closes.
//!
//! The probe reads `/sbin/mount`'s output rather than guessing `/Volumes/<share>`,
//! because the share name is not unique: two servers can both export `public`, and
//! the automounter answers that by appending `-1`. The only reliable check is the
//! `//user@HOST/share` device string the mount itself carries.
//!
//! Kept in its own module, with no platform gate, so the parser is compiled and
//! tested everywhere rather than only on the machine it happens to run on.

use std::path::PathBuf;

/// The mount table. Takes no arguments and no user input, so there is nothing to
/// sanitise and nothing to pass.
pub const MOUNT: &str = "/sbin/mount";

/// Read the mount table, or nothing if it cannot be read.
///
/// A workstation whose mount table cannot be listed is not a fatal condition: it
/// only means the probe cannot find an existing mount, and the connector goes on to
/// make one.
pub fn read_table() -> Option<String> {
    let output = std::process::Command::new(MOUNT).output().ok()?;
    if !output.status.success() {
        tracing::warn!(event = "db.share.mount_table.failed", status = %output.status);
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Find a mount of this host and share, of one of `filesystems`, in `mount` output.
///
/// The lines look like
///
/// ```text
/// //felipe@FILESERVER/ti-share on /Volumes/ti-share (smbfs, nodev, nosuid, mounted by felipe)
/// //nas/public on /mnt/public (cifs, rw, relatime, vers=3.1.1)
/// ```
///
/// so the parse is: split on ` on `, require one of the named filesystems in the
/// options, and compare the host and share **case-insensitively** — the SMB client
/// upper-cases the server name, and the operator did not.
pub fn find(table: &str, host: &str, share: &str, filesystems: &[&str]) -> Option<PathBuf> {
    for line in table.lines() {
        let Some((device, rest)) = line.split_once(" on ") else {
            continue;
        };
        let Some((mount_point, options)) = rest.rsplit_once(" (") else {
            continue;
        };
        let filesystem = options.split(',').next().unwrap_or_default().trim();
        if !filesystems.contains(&filesystem) {
            continue;
        }
        let Some((mounted_host, mounted_share)) = split_device(device) else {
            continue;
        };
        if mounted_host.eq_ignore_ascii_case(host) && mounted_share.eq_ignore_ascii_case(share) {
            return Some(PathBuf::from(mount_point));
        }
    }
    None
}

/// `//user@HOST/share` → `(HOST, share)`.
///
/// The user is ignored: it is not part of the share's identity, and a share mounted
/// by another account is still that share.
fn split_device(device: &str) -> Option<(&str, &str)> {
    let body = device.trim().strip_prefix("//")?;
    let (authority, share) = body.split_once('/')?;
    let host = match authority.rsplit_once('@') {
        Some((_user, host)) => host,
        None => authority,
    };
    // Only the first component of the share path names the share.
    let share = share.split('/').next().unwrap_or_default();
    (!host.is_empty() && !share.is_empty()).then_some((host, share))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `smbfs` and `cifs` mount formats as `mount(8)` documents them, among the
    /// local filesystems of a real workstation.
    const TABLE: &str = "\
/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)
devfs on /dev (devfs, local, nobrowse)
map auto_home on /System/Volumes/Data/home (autofs, automounted, nobrowse)
/dev/disk7s1 on /Volumes/DadosFFQ (apfs, local, nodev, nosuid, journaled, noowners)
//felipe@FILESERVER/ti-share on /Volumes/ti-share (smbfs, nodev, nosuid, mounted by felipe)
//GUEST@NAS/public on /Volumes/public-1 (smbfs, nodev, nosuid, mounted by felipe)
//nas/backups on /mnt/backups (cifs, rw, relatime, vers=3.1.1, username=felipe)
";

    const SMB: [&str; 2] = ["smbfs", "cifs"];

    #[test]
    fn a_share_is_found_regardless_of_the_case_the_operator_typed() {
        assert_eq!(
            find(TABLE, "fileserver", "ti-share", &SMB),
            Some(PathBuf::from("/Volumes/ti-share")),
            "the SMB client upper-cases the server name; the operator does not"
        );
        assert_eq!(
            find(TABLE, "FileServer", "TI-Share", &SMB),
            Some(PathBuf::from("/Volumes/ti-share"))
        );
    }

    #[test]
    fn the_mount_point_is_read_from_the_table_and_not_guessed_from_the_share_name() {
        // `/Volumes/public` is what a guess would produce; the automounter
        // actually used `/Volumes/public-1`, because the name was taken.
        assert_eq!(
            find(TABLE, "nas", "public", &SMB),
            Some(PathBuf::from("/Volumes/public-1"))
        );
    }

    #[test]
    fn a_linux_cifs_mount_is_recognised_too() {
        assert_eq!(
            find(TABLE, "nas", "backups", &SMB),
            Some(PathBuf::from("/mnt/backups"))
        );
        // …and not when only smbfs is asked for.
        assert_eq!(find(TABLE, "nas", "backups", &["smbfs"]), None);
    }

    #[test]
    fn a_share_that_is_not_mounted_is_not_invented() {
        assert_eq!(find(TABLE, "fileserver", "other-share", &SMB), None);
        assert_eq!(find(TABLE, "otherserver", "ti-share", &SMB), None);
    }

    #[test]
    fn a_local_filesystem_is_never_mistaken_for_a_share() {
        // `DadosFFQ` is an APFS volume, and a share of that name must not resolve
        // to it: the journal mode and the whole locking story depend on the
        // difference between a local file and a shared one.
        assert_eq!(find(TABLE, "dadosffq", "DadosFFQ", &SMB), None);
    }

    #[test]
    fn a_device_string_without_a_share_is_ignored_rather_than_panicking() {
        assert_eq!(split_device("//HOST"), None);
        assert_eq!(split_device("map auto_home"), None);
        assert_eq!(split_device("//HOST/share"), Some(("HOST", "share")));
        assert_eq!(split_device("//u@HOST/share"), Some(("HOST", "share")));
        // A device string that reaches into the share still names the share.
        assert_eq!(
            split_device("//u@HOST/share/inner"),
            Some(("HOST", "share"))
        );
    }

    #[test]
    fn a_truncated_table_is_survived() {
        for broken in ["", "no separators here", "//h/s on /mnt", "x on y (smbfs"] {
            assert_eq!(find(broken, "h", "s", &SMB), None, "{broken}");
        }
    }
}
