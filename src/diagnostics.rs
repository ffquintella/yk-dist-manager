//! `--diagnose`: what this build is, where it will look, and what it can reach.
//!
//! Two audiences. An operator with a problem can paste the output into a ticket
//! instead of describing it. And packaging can be *verified* — `make verify-bundle`
//! runs the bundled binary with this flag and checks that macOS sees a real bundle,
//! which is the difference between the camera working and refusing.
//!
//! Everything here is pure or read-only: no database is opened, no key is touched,
//! no camera is started.

use std::fmt::Write as _;

/// What the binary was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invocation {
    /// Normal start: open the GUI.
    Gui,
    /// Print the version and exit.
    Version,
    /// Print the diagnostic report and exit.
    Diagnose,
    /// Print usage and exit.
    Help,
    /// An argument we do not understand; the text is echoed back.
    Unknown(&'static str),
}

/// Parse the command line. Deliberately tiny — this is a GUI application with three
/// switches, not a CLI, and a dependency for that would be silly.
pub fn parse_args<'a, I>(args: I) -> Invocation
where
    I: IntoIterator<Item = &'a str>,
{
    for arg in args {
        match arg {
            "--version" | "-V" => return Invocation::Version,
            "--diagnose" | "--doctor" => return Invocation::Diagnose,
            "--help" | "-h" => return Invocation::Help,
            other if other.starts_with('-') => {
                // Leaked so the variant can stay `Copy`; this path ends the process.
                return Invocation::Unknown(Box::leak(other.to_owned().into_boxed_str()));
            }
            _ => {}
        }
    }
    Invocation::Gui
}

pub const USAGE: &str = "\
yk-dist-manager — YubiKey distribution and bootstrap manager

Usage: yk-dist-manager [OPTIONS]

Options:
  --diagnose     Print build, path and capability information, then exit
  --version, -V  Print the version, then exit
  --help, -h     Print this help, then exit

Environment:
  YKDM_DB                      Database file to open (overrides the remembered one)
  YKDM_SETTINGS                Settings file (recent databases, operator identity)
  YKDM_DATA_DIR                Per-user data directory
  YKDM_LOG                     Log filter, e.g. `debug`
  YKDM_ALLOW_UNBUNDLED_CAMERA  Attempt the camera outside an app bundle (may abort)
  YKDM_SYNC_QUIET_MS           Cloud-sync folder: how long the database file must be
                               unchanged before it counts as downloaded (default 1500)
  YKDM_SYNC_TIMEOUT_MS         Cloud-sync folder: how long to wait for the sync client
                               before saying so and carrying on (default 15000)
";

/// The features this build was compiled with.
pub fn compiled_features() -> Vec<&'static str> {
    let mut features = Vec::new();
    if cfg!(feature = "file-dialog") {
        features.push("file-dialog");
    }
    if cfg!(feature = "barcode") {
        features.push("barcode");
    }
    if cfg!(feature = "camera") {
        features.push("camera");
    }
    if cfg!(feature = "encrypted-db") {
        features.push("encrypted-db");
    }
    if cfg!(feature = "native-piv") {
        features.push("native-piv");
    }
    if cfg!(feature = "native-fido") {
        features.push("native-fido");
    }
    if cfg!(feature = "native-otp") {
        features.push("native-otp");
    }
    if features.is_empty() {
        features.push("(none)");
    }
    features
}

/// The `Info.plist` beside the running executable, if there is one.
///
/// `…/Foo.app/Contents/MacOS/binary` → `…/Foo.app/Contents/Info.plist`.
pub fn bundle_plist_path() -> Option<std::path::PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let macos_dir = executable.parent()?;
    if macos_dir.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos_dir.parent()?;
    let plist = contents.join("Info.plist");
    plist.is_file().then_some(plist)
}

/// Whether that `Info.plist` declares the camera usage description macOS requires.
///
/// Read as text rather than parsed: the only question is whether the key is present,
/// and a plist parser would be a dependency for one `contains`.
pub fn declares_camera_usage() -> bool {
    bundle_plist_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|body| body.contains("NSCameraUsageDescription"))
        .unwrap_or(false)
}

/// Inputs the report needs from elsewhere, passed in so the formatting is testable.
#[derive(Debug, Clone)]
pub struct Report {
    pub version: &'static str,
    /// The commit this build came from, or `unknown`
    /// (`features/packaging-and-release.md` phase 2).
    ///
    /// The field a support request most needs and least often has: two operators
    /// running "0.13.0" may be running different code, and the answer to "is this
    /// the build we released?" is this line rather than the version above it.
    pub commit: &'static str,
    pub executable: String,
    pub bundled: bool,
    pub plist: Option<String>,
    pub camera_usage_declared: bool,
    pub camera_authorised: bool,
    pub features: Vec<&'static str>,
    pub database: String,
    pub database_exists: bool,
    /// The database sits in a synchronising folder — a real risk, not a nitpick.
    pub database_on_cloud_sync: bool,
    /// Who holds the single-writer lock, when one is there. The first question to
    /// ask about a cloud-hosted database that will not open.
    pub database_lock: Option<String>,
    /// Copies a sync client could not merge, sitting next to the database.
    pub database_conflicts: Vec<String>,
    /// How this build reaches an SMB share, and whether it can connect one itself.
    ///
    /// The first question about a register on a file server that will not open is
    /// which of the two situations the workstation is in: this build connects the
    /// share, or the share has to be there already.
    pub smb_connector: String,
    pub smb_can_connect: bool,
    /// SMB shares this workstation has opened the register from, and as whom.
    /// Never a password — there is none stored to report.
    pub smb_shares: Vec<String>,
    pub settings: String,
    /// Which transport would read the hardware right now, and why
    /// (`features/native-device-transport.md` phase 6).
    ///
    /// The decision, not the feature list: the two disagree exactly when the
    /// interesting fault is present — a build that *has* the native transport on a
    /// machine whose reader does not answer — and that is the case a support request
    /// is usually about.
    pub transport: String,
    pub ykman: Option<String>,
    pub cameras: Vec<String>,
}

impl Report {
    /// Gather everything, touching nothing that could fail loudly.
    pub fn gather() -> Self {
        let database = crate::store::Store::default_path();
        let settings = crate::settings::AppSettings::load();
        let effective = settings.last_database.clone().unwrap_or(database);

        Self {
            version: crate::VERSION,
            commit: crate::COMMIT,
            executable: std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "(unknown)".into()),
            bundled: crate::scan::preflight::inside_macos_bundle(),
            plist: bundle_plist_path().map(|p| p.display().to_string()),
            camera_usage_declared: declares_camera_usage(),
            camera_authorised: crate::scan::preflight::authorised(),
            features: compiled_features(),
            database_exists: effective.is_file(),
            database_on_cloud_sync: crate::store::looks_like_cloud_sync(&effective),
            // Read, never taken: `--diagnose` must not lock a database another
            // operator is working in, and must not create a lock file of its own.
            database_lock: crate::store::cloud::read_holder(&crate::store::cloud::lock_path(
                &effective,
            ))
            .map(|holder| holder.to_string()),
            database_conflicts: crate::store::cloud::conflict_copies(&effective)
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
            database: effective.display().to_string(),
            smb_connector: crate::store::smb::platform_connector().label().to_owned(),
            smb_can_connect: crate::store::smb::can_connect(),
            // Read from the settings file, not probed: `--diagnose` must not open a
            // connection to a file server, and there is no password here to leak
            // because none was ever stored.
            smb_shares: settings
                .recent_shares
                .iter()
                .map(|entry| {
                    if entry.user.is_empty() {
                        format!("{} as {}", entry.location, entry.access.label())
                    } else {
                        format!("{} as {}", entry.location, entry.user)
                    }
                })
                .collect(),
            settings: crate::settings::AppSettings::path().display().to_string(),
            // Probed, because the compiled feature list cannot answer this. Read-only:
            // enumerating readers opens a PC/SC context and writes nothing, which is
            // the same rule the hardware tests hold to.
            transport: crate::device::select::decide(
                settings.transport,
                crate::device::select::probe(settings.transport),
            )
            .describe(),
            ykman: which_ykman(),
            cameras: list_cameras(),
        }
    }

    /// Render the report. Plain text, `key: value`, so it pastes into a ticket.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "yk-dist-manager {}", self.version);
        let _ = writeln!(out);
        let _ = writeln!(out, "commit:            {}", self.commit);
        let _ = writeln!(out, "executable:        {}", self.executable);
        let _ = writeln!(out, "features:          {}", self.features.join(", "));
        let _ = writeln!(out);
        let _ = writeln!(out, "bundle:            {}", yes_no(self.bundled));
        if let Some(plist) = &self.plist {
            let _ = writeln!(out, "Info.plist:        {plist}");
        }
        let _ = writeln!(
            out,
            "camera usage key:  {}",
            yes_no(self.camera_usage_declared)
        );
        let _ = writeln!(out, "camera authorised: {}", yes_no(self.camera_authorised));
        let _ = writeln!(
            out,
            "cameras:           {}",
            if self.cameras.is_empty() {
                "(none found)".to_owned()
            } else {
                self.cameras.join(", ")
            }
        );
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "database:          {} ({})",
            self.database,
            if self.database_exists {
                "exists"
            } else {
                "not there yet"
            }
        );
        if self.database_on_cloud_sync {
            let _ = writeln!(
                out,
                "                   WARNING: this is a cloud-sync folder. A sync client can \
                 copy the file mid-write, and it resolves a clash by keeping both copies — so \
                 two operators can end up with divergent registers and no merge. One workstation \
                 at a time may open it: the single-writer lock file next to the database is what \
                 enforces that. A network share, or a local file with a scheduled backup, is \
                 still safer."
            );
        }
        let _ = writeln!(
            out,
            "database lock:     {}",
            self.database_lock
                .clone()
                .unwrap_or_else(|| "(not held)".into())
        );
        if !self.database_conflicts.is_empty() {
            let _ = writeln!(
                out,
                "                   ALARM: {} sync conflict copy/copies next to the database: \
                 {}. The register may have forked; compare them before trusting either.",
                self.database_conflicts.len(),
                self.database_conflicts.join(", ")
            );
        }
        let _ = writeln!(
            out,
            "smb shares:        {} ({})",
            self.smb_connector,
            if self.smb_can_connect {
                "this build can connect a share itself"
            } else {
                "the share must be mounted by the system on this platform"
            }
        );
        for share in &self.smb_shares {
            let _ = writeln!(out, "                   {share}");
        }
        let _ = writeln!(out, "settings:          {}", self.settings);
        let _ = writeln!(out, "device transport:  {}", self.transport);
        let _ = writeln!(
            out,
            "ykman:             {}",
            self.ykman.clone().unwrap_or_else(|| "(not on PATH)".into())
        );

        let _ = writeln!(out);
        let _ = writeln!(out, "camera scanning:   {}", self.camera_verdict());
        out
    }

    /// One line saying whether camera scanning will work, and why not if it will not.
    ///
    /// This is the line `make verify-bundle` and a support ticket both care about.
    pub fn camera_verdict(&self) -> String {
        if !self.features.contains(&"camera") {
            return "unavailable — this build has no `camera` feature".into();
        }
        if cfg!(target_os = "macos") {
            if !self.bundled {
                return "refused — not running from an .app bundle, so macOS has no \
                        NSCameraUsageDescription to attribute the request to (use a USB \
                        barcode reader, or run the bundled application)"
                    .into();
            }
            if !self.camera_usage_declared {
                return "refused — the bundle's Info.plist does not declare \
                        NSCameraUsageDescription"
                    .into();
            }
            if !self.camera_authorised {
                return "not yet authorised — the prompt appears on first use, or allow it \
                        under System Settings → Privacy & Security → Camera"
                    .into();
            }
        }
        if self.cameras.is_empty() {
            return "no camera found".into();
        }
        "ready".into()
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// Where `ykman` is, if anywhere. Read-only, and does not run it.
fn which_ykman() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("ykman"))
        .find(|candidate| candidate.is_file())
        .map(|found| found.display().to_string())
}

/// Enumerate cameras. Listing devices does not open one, so it is safe here.
fn list_cameras() -> Vec<String> {
    #[cfg(feature = "camera")]
    {
        crate::scan::camera::CameraScanner::available_cameras()
            .into_iter()
            .map(|(index, name)| format!("{index}: {name}"))
            .collect()
    }
    #[cfg(not(feature = "camera"))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Report {
        Report {
            version: "0.0.0-test",
            commit: "0123456789ab",
            executable: "/Applications/Test.app/Contents/MacOS/test".into(),
            bundled: true,
            plist: Some("/Applications/Test.app/Contents/Info.plist".into()),
            camera_usage_declared: true,
            camera_authorised: true,
            features: vec!["camera"],
            database: "/tmp/keys.sqlite3".into(),
            database_exists: true,
            database_on_cloud_sync: false,
            database_lock: None,
            database_conflicts: Vec::new(),
            smb_connector: "NetFS (macOS)".into(),
            smb_can_connect: true,
            smb_shares: Vec::new(),
            settings: "/tmp/settings.json".into(),
            transport: "native — a reader answered, and this build talks to it in process".into(),
            ykman: Some("/opt/homebrew/bin/ykman".into()),
            cameras: vec!["0: FaceTime HD Camera".into()],
        }
    }

    #[test]
    fn arguments_are_recognised() {
        assert_eq!(parse_args(["--version"]), Invocation::Version);
        assert_eq!(parse_args(["-V"]), Invocation::Version);
        assert_eq!(parse_args(["--diagnose"]), Invocation::Diagnose);
        assert_eq!(parse_args(["--doctor"]), Invocation::Diagnose);
        assert_eq!(parse_args(["--help"]), Invocation::Help);
        assert_eq!(parse_args(["-h"]), Invocation::Help);
        assert_eq!(parse_args(Vec::<&str>::new()), Invocation::Gui);
    }

    #[test]
    fn an_unknown_switch_is_reported_rather_than_ignored() {
        assert_eq!(parse_args(["--wat"]), Invocation::Unknown("--wat"));
        // A bare word is not a switch: macOS passes `-psn_…` and Finder can pass
        // document paths, so only `-` prefixes are rejected.
        assert_eq!(parse_args(["something"]), Invocation::Gui);
    }

    #[test]
    fn a_ready_build_says_so() {
        assert_eq!(report().camera_verdict(), "ready");
    }

    #[test]
    fn an_unbundled_macos_build_explains_the_refusal() {
        let mut report = report();
        report.bundled = false;
        let verdict = report.camera_verdict();
        if cfg!(target_os = "macos") {
            assert!(
                verdict.contains("not running from an .app bundle"),
                "{verdict}"
            );
            assert!(
                verdict.contains("USB barcode reader"),
                "the alternative must be offered: {verdict}"
            );
        }
    }

    #[test]
    fn a_bundle_without_the_usage_key_is_called_out() {
        let mut report = report();
        report.camera_usage_declared = false;
        if cfg!(target_os = "macos") {
            assert!(
                report
                    .camera_verdict()
                    .contains("does not declare NSCameraUsageDescription")
            );
        }
    }

    #[test]
    fn an_unauthorised_bundle_points_at_the_setting() {
        let mut report = report();
        report.camera_authorised = false;
        if cfg!(target_os = "macos") {
            let verdict = report.camera_verdict();
            assert!(verdict.contains("not yet authorised"), "{verdict}");
            assert!(verdict.contains("Privacy & Security"), "{verdict}");
        }
    }

    #[test]
    fn a_build_without_the_feature_says_that_first() {
        let mut report = report();
        report.features = vec!["file-dialog"];
        report.bundled = false;
        assert!(
            report.camera_verdict().contains("no `camera` feature"),
            "the missing feature outranks every other reason"
        );
    }

    #[test]
    fn no_camera_is_distinguished_from_no_permission() {
        let mut report = report();
        report.cameras.clear();
        assert_eq!(report.camera_verdict(), "no camera found");
    }

    #[test]
    fn the_report_is_pasteable_and_names_the_essentials() {
        let text = report().render();
        for expected in [
            "yk-dist-manager 0.0.0-test",
            "bundle:            yes",
            "camera usage key:  yes",
            "/tmp/keys.sqlite3",
            "/tmp/settings.json",
            "0: FaceTime HD Camera",
            "camera scanning:   ready",
        ] {
            assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
        }
    }

    #[test]
    fn a_cloud_sync_database_is_warned_about_in_the_report() {
        let mut warned = report();
        warned.database_on_cloud_sync = true;
        let text = warned.render();
        assert!(
            text.contains("WARNING: this is a cloud-sync folder"),
            "{text}"
        );
        assert!(
            text.contains("keeping both copies"),
            "the report must say what goes wrong, not just that something might"
        );

        // And it stays quiet when there is nothing to warn about.
        assert!(!report().render().contains("WARNING"));
    }

    #[test]
    fn the_report_says_who_holds_the_single_writer_lock() {
        // The state of the lock is the first question about a cloud-hosted
        // database that will not open, so the line is always there.
        assert!(report().render().contains("database lock:     (not held)"));

        let mut held = report();
        held.database_lock = Some("felipe on MAC-01 (pid 42), holding since …".into());
        assert!(held.render().contains("MAC-01"));
    }

    #[test]
    fn sync_conflict_copies_are_an_alarm_in_the_report() {
        let mut forked = report();
        forked.database_conflicts = vec!["/tmp/keys (1).sqlite3".into()];
        let text = forked.render();
        assert!(text.contains("ALARM"), "{text}");
        assert!(text.contains("keys (1).sqlite3"), "{text}");
        assert!(
            text.contains("may have forked"),
            "the report must say what the copies mean"
        );
    }

    #[test]
    fn the_report_says_how_this_build_reaches_a_share() {
        // The first question about a register on a file server that will not open:
        // does this build connect the share, or must the system have mounted it?
        let mut connected = report();
        connected.smb_shares = vec![r"//fileserver/ti-share/keys.sqlite3 as FGV\felipe".into()];
        let text = connected.render();
        assert!(text.contains("NetFS (macOS)"), "{text}");
        assert!(text.contains("can connect a share itself"), "{text}");
        assert!(
            text.contains("//fileserver/ti-share/keys.sqlite3"),
            "{text}"
        );
        assert!(text.contains(r"FGV\felipe"), "{text}");

        let mut limited = report();
        limited.smb_connector = "system mounts only".into();
        limited.smb_can_connect = false;
        assert!(limited.render().contains("must be mounted by the system"));
    }

    #[test]
    fn the_report_names_the_transport_that_would_actually_be_used() {
        // The feature list already says what was compiled. What a support request
        // needs is the *decision*, because the two disagree exactly when the
        // interesting fault is present: a native build on a machine whose reader does
        // not answer.
        let rendered = report().render();
        assert!(
            rendered.contains("device transport:"),
            "the report has to name the transport: {rendered}"
        );
        assert!(
            rendered.contains("a reader answered"),
            "and the reason with it, or it cannot be diagnosed from a paste: {rendered}"
        );

        // And a real gather decides something rather than leaving it blank — this is
        // the field an operator is asked to paste.
        assert!(!Report::gather().transport.trim().is_empty());
    }

    #[test]
    fn the_report_names_the_commit_this_build_came_from() {
        let rendered = report().render();
        assert!(
            rendered.contains("commit:            0123456789ab"),
            "a support report has to identify the build, not just the version: {rendered}"
        );

        // And a real build answers it: either a commit or the word `unknown`, never
        // an empty field somebody has to interpret.
        let gathered = Report::gather();
        assert!(!gathered.commit.trim().is_empty());
        assert!(
            crate::build_id().starts_with(crate::VERSION),
            "the build id opens with the version, so it reads as one thing"
        );
    }

    #[test]
    fn a_missing_ykman_is_stated_not_omitted() {
        let mut report = report();
        report.ykman = None;
        assert!(report.render().contains("(not on PATH)"));
    }

    #[test]
    fn usage_lists_every_switch_and_environment_variable() {
        for expected in [
            "--diagnose",
            "--version",
            "--help",
            "YKDM_DB",
            "YKDM_SETTINGS",
            "YKDM_ALLOW_UNBUNDLED_CAMERA",
        ] {
            assert!(USAGE.contains(expected), "USAGE omits {expected}");
        }
    }

    #[test]
    fn the_feature_list_matches_this_build() {
        let features = compiled_features();
        assert_eq!(features.contains(&"camera"), cfg!(feature = "camera"));
        assert_eq!(
            features.contains(&"file-dialog"),
            cfg!(feature = "file-dialog")
        );
    }
}
