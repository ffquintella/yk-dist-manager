//! What the application leaves on disk about its own life: a rotating log file,
//! and the marker that outlives a start which never finished.
//!
//! `features/logging.md` phase 2. The reason it matters more than it sounds: a
//! desktop application that logs to stderr has, in practice, no log. On Windows
//! the release binary is linked into the *windows* subsystem precisely so no
//! console flashes at launch (`src/main.rs`), so there is no stderr to read at
//! all; on Linux and macOS the operator launches from a desktop entry or the
//! Dock and never sees one either. Everything the log knows about a failure is
//! therefore worth nothing unless it is written down here.
//!
//! ## Where
//!
//! One directory under [`crate::paths::data_dir`], so the log cannot drift away
//! from the settings file and the default database:
//!
//! | Platform | Directory |
//! |---|---|
//! | Windows | `%APPDATA%\yk-dist-manager\logs\` (`…\AppData\Roaming\…`) |
//! | Linux | `~/.local/share/yk-dist-manager/logs/` |
//! | macOS | `~/Library/Application Support/yk-dist-manager/logs/` |
//!
//! `$YKDM_LOG_DIR` overrides it, and `$YKDM_DATA_DIR` moves it with everything
//! else — which is what the tests use, because a test that wrote to the real
//! directory would be scribbling in the operator's own log.
//!
//! Linux convention would put a log under `$XDG_STATE_HOME` and macOS under
//! `~/Library/Logs`. Both are deliberately not used: `--diagnose` has to be able
//! to *name* this path to an operator on the telephone, and one directory holding
//! the register, the settings and the log is one sentence rather than three.
//!
//! ## Rotation, and why it is size- and not time-based
//!
//! [`MAX_BYTES`] per file, [`KEEP`] older generations, so the ceiling is fixed at
//! roughly six megabytes however long the application is left open. Time-based
//! rotation gets this wrong in both directions for a tool used at a reception
//! desk: a day with one hand-over produces an empty file, and the day a bootstrap
//! loops on a failing reader produces an unbounded one.
//!
//! ## No buffering, on purpose
//!
//! Lines are written straight through to the file with no `BufWriter`. The line
//! this module exists for is the *last* one before a crash, and a buffered writer
//! is exactly the thing that loses it.
//!
//! ## Two copies of the application at once
//!
//! Nothing here is locked between processes. Two instances started together can
//! overwrite each other's marker, and the first to finish removes the marker the
//! second is still relying on — so the worst cases are one spurious report of an
//! unfinished start, or one missed. Both are acceptable: this is a diagnostic,
//! it never blocks a launch, and the alternative — a lock on the path that says
//! whether the *last* launch worked — is a way for the log to stop an
//! application from starting, which is the failure it exists to explain.
//!
//! ## This is not the audit trail
//!
//! Same split as [`crate::logbuf`] and for the same reason
//! (`features/audit-trail.md`): the log is operational and rotates away, the
//! audit trail is accountability and never changes. Nothing rotated out of here
//! was evidence of anything.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The file being written now. Older generations are this plus `.1` … `.N`.
pub const FILE_NAME: &str = "yk-dist-manager.log";

/// Rotate once a file would pass this size.
pub const MAX_BYTES: u64 = 1024 * 1024;

/// How many rotated generations are kept behind the current file.
pub const KEEP: usize = 5;

/// The marker written while a start is in progress and removed once a window
/// exists. Finding it at the next start is the only evidence of a crash that
/// killed the process outright — one no panic hook can catch.
pub const MARKER_NAME: &str = "startup-in-progress";

/// The directory the log and the startup marker live in.
///
/// `$YKDM_LOG_DIR` overrides it outright; otherwise it is `logs/` under
/// [`crate::paths::data_dir`], which `$YKDM_DATA_DIR` already redirects.
pub fn directory() -> PathBuf {
    if let Ok(explicit) = std::env::var("YKDM_LOG_DIR")
        && !explicit.trim().is_empty()
    {
        return PathBuf::from(explicit);
    }
    crate::paths::data_dir().join("logs")
}

/// The file the application is writing to right now.
pub fn path() -> PathBuf {
    directory().join(FILE_NAME)
}

/// The name of generation `n` behind the current file: `1` is the most recent.
pub fn rotated_name(n: usize) -> String {
    format!("{FILE_NAME}.{n}")
}

/// The stages of the start-up procedure, in the order they are reached.
///
/// Named rather than free text because two places have to agree on them: the
/// marker written on the way up, and [`explain`], which turns the one found at
/// the *next* start into a sentence an operator can act on.
pub mod stage {
    /// Logging is up and the command line has been read.
    pub const START: &str = "start";
    /// About to ask the platform about the camera — the macOS pre-flight, which
    /// must happen on the main thread before anything touches AVFoundation.
    pub const CAMERA_PREFLIGHT: &str = "camera-preflight";
    /// About to read the settings file.
    pub const SETTINGS: &str = "settings";
    /// About to ask the windowing system and the graphics driver for a window.
    pub const WINDOW: &str = "window";
    /// The window exists; building the application, which opens the register.
    pub const APP: &str = "app-construction";
}

/// What a start that died at `stage` most likely died of.
///
/// The whole point of the marker: "the previous start did not finish" is not
/// actionable, and "it never got past asking the graphics driver for a window"
/// is. Deliberately hedged — this is the *usual* cause, not a diagnosis.
pub fn explain(stage: &str) -> &'static str {
    match stage {
        self::stage::START => "it stopped before it could do anything at all",
        self::stage::CAMERA_PREFLIGHT => {
            "it stopped in the camera pre-flight — the barcode camera or its permission prompt; \
             a build without the `camera` feature cannot reach this"
        }
        self::stage::SETTINGS => {
            "it stopped reading the settings file — a half-written or unreadable settings file"
        }
        self::stage::WINDOW => {
            "it stopped asking the windowing system for a window — usually the graphics driver, \
             a remote or headless session, or a display that is no longer attached"
        }
        self::stage::APP => {
            "it stopped building the application, which is where the register is opened — an \
             unreachable share, a locked or corrupt database file"
        }
        _ => "it stopped at a stage this build does not recognise",
    }
}

/// An append-only log file that rotates by size.
#[derive(Debug)]
pub struct LogFile {
    path: PathBuf,
    max_bytes: u64,
    keep: usize,
    handle: File,
    /// Bytes in the current file, carried across runs from its length at open.
    written: u64,
}

impl LogFile {
    /// Open (creating the directory if need be) with the shipped limits.
    pub fn open(directory: &Path) -> io::Result<Self> {
        Self::with_limits(directory, MAX_BYTES, KEEP)
    }

    /// Open with explicit limits. The tests use this; nothing else should.
    pub fn with_limits(directory: &Path, max_bytes: u64, keep: usize) -> io::Result<Self> {
        std::fs::create_dir_all(directory)?;
        let path = directory.join(FILE_NAME);
        let handle = append_to(&path)?;
        let written = handle.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path,
            max_bytes,
            keep,
            handle,
            written,
        })
    }

    /// The file currently being written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one already-formatted line, rotating first if it would not fit.
    ///
    /// A line longer than the whole budget is still written whole: splitting it
    /// would break the one-event-one-line property every reader of this file
    /// depends on.
    pub fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        if self.written > 0 && self.written.saturating_add(line.len() as u64) > self.max_bytes {
            self.rotate()?;
        }
        self.handle.write_all(line)?;
        self.written = self.written.saturating_add(line.len() as u64);
        Ok(())
    }

    /// Shuffle the generations along and start a new current file.
    fn rotate(&mut self) -> io::Result<()> {
        let directory = self.path.parent().unwrap_or(Path::new("."));
        // Oldest first, or the shuffle overwrites what it is about to move.
        let _ = std::fs::remove_file(directory.join(rotated_name(self.keep)));
        for n in (1..self.keep).rev() {
            let from = directory.join(rotated_name(n));
            if from.exists() {
                let _ = std::fs::rename(from, directory.join(rotated_name(n + 1)));
            }
        }
        if self.keep > 0 {
            std::fs::rename(&self.path, directory.join(rotated_name(1)))?;
        } else {
            std::fs::remove_file(&self.path)?;
        }
        self.handle = append_to(&self.path)?;
        self.written = 0;
        Ok(())
    }
}

fn append_to(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// One line for `--diagnose`: where the log is and how much of it there is.
///
/// Never fails and never creates anything — `--diagnose` is read-only by
/// contract, and an operator asking where the log is must not thereby make one.
pub fn describe() -> String {
    describe_in(&directory())
}

/// [`describe`] for a directory named outright, which is what the tests use:
/// a test that read `$YKDM_LOG_DIR` would be mutating process-global state that
/// every other test in the same binary shares.
pub fn describe_in(directory: &Path) -> String {
    let current = directory.join(FILE_NAME);
    let rotated = (1..=KEEP)
        .filter(|n| directory.join(rotated_name(*n)).is_file())
        .count();
    match std::fs::metadata(&current) {
        Ok(meta) => format!(
            "{} ({} KiB, {rotated} rotated)",
            current.display(),
            meta.len().div_ceil(1024)
        ),
        Err(_) => format!("{} (nothing written yet)", current.display()),
    }
}

/// Record that start-up has reached `stage`.
///
/// Best effort by design: a log directory that cannot be written must cost the
/// operator a diagnostic, never a launch.
pub fn note_stage(stage: &str) {
    let _ = note_stage_in(&directory(), stage);
}

/// [`note_stage`] for a directory named outright, and reporting its failure.
pub fn note_stage_in(directory: &Path, stage: &str) -> io::Result<()> {
    std::fs::create_dir_all(directory)?;
    let now = chrono::Local::now();
    std::fs::write(
        directory.join(MARKER_NAME),
        format!(
            "stage={stage} version={} commit={} at=[{}] {}\n",
            crate::VERSION,
            crate::COMMIT,
            now.format("%d/%m/%Y"),
            now.format("%H:%M:%S"),
        ),
    )
}

/// Record that start-up finished: there is a window and an application behind it.
pub fn finished() {
    finished_in(&directory());
}

/// [`finished`] for a directory named outright.
pub fn finished_in(directory: &Path) {
    let _ = std::fs::remove_file(directory.join(MARKER_NAME));
}

/// The marker a previous start left behind, if it left one.
///
/// Read *before* [`note_stage`] overwrites it, which is the whole ordering
/// constraint this module places on `main`.
pub fn previous_attempt() -> Option<String> {
    previous_attempt_in(&directory())
}

/// [`previous_attempt`] for a directory named outright.
pub fn previous_attempt_in(directory: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(directory.join(MARKER_NAME)).ok()?;
    let raw = raw.trim();
    (!raw.is_empty()).then(|| raw.to_owned())
}

/// The `stage=` field of a marker, for the sentence [`explain`] then produces.
pub fn stage_of(marker: &str) -> Option<&str> {
    marker
        .split_whitespace()
        .find_map(|field| field.strip_prefix("stage="))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().expect("a temporary directory")
    }

    #[test]
    fn opening_creates_the_directory_rather_than_failing() {
        // First launch on a fresh workstation: nothing exists yet, and the log
        // is the one thing that must work before anything else does.
        let home = temp();
        let directory = home.path().join("logs");
        let mut log = LogFile::open(&directory).expect("opens");
        log.write_line(b"first\n").expect("writes");
        assert_eq!(
            std::fs::read_to_string(directory.join(FILE_NAME)).unwrap(),
            "first\n"
        );
    }

    #[test]
    fn a_second_run_appends_rather_than_truncating() {
        // Losing the previous session's log at launch would destroy the record
        // of the crash that caused the relaunch.
        let home = temp();
        LogFile::open(home.path())
            .unwrap()
            .write_line(b"one\n")
            .unwrap();
        LogFile::open(home.path())
            .unwrap()
            .write_line(b"two\n")
            .unwrap();
        let body = std::fs::read_to_string(home.path().join(FILE_NAME)).unwrap();
        assert_eq!(body, "one\ntwo\n");
    }

    #[test]
    fn the_file_rotates_once_it_would_pass_the_limit() {
        let home = temp();
        let mut log = LogFile::with_limits(home.path(), 16, 2).unwrap();
        log.write_line(b"0123456789\n").unwrap(); // 11 bytes
        log.write_line(b"abcdefghij\n").unwrap(); // would be 22 — rotates first

        assert_eq!(
            std::fs::read_to_string(home.path().join(FILE_NAME)).unwrap(),
            "abcdefghij\n",
            "the current file holds the newest line"
        );
        assert_eq!(
            std::fs::read_to_string(home.path().join(rotated_name(1))).unwrap(),
            "0123456789\n",
            "and the previous one moved to .1"
        );
    }

    #[test]
    fn rotation_keeps_a_bounded_number_of_generations() {
        // The ceiling is the point: a register left open all day cannot fill a
        // workstation's disk with its own log.
        let home = temp();
        let mut log = LogFile::with_limits(home.path(), 8, 2).unwrap();
        for n in 0..6 {
            log.write_line(format!("line{n}\n").as_bytes()).unwrap();
        }
        assert!(home.path().join(FILE_NAME).is_file());
        assert!(home.path().join(rotated_name(1)).is_file());
        assert!(home.path().join(rotated_name(2)).is_file());
        assert!(
            !home.path().join(rotated_name(3)).is_file(),
            "the oldest generation is dropped, not kept forever"
        );
    }

    #[test]
    fn rotation_moves_the_generations_along_in_order() {
        // The bug this guards: shuffling oldest-last overwrites .2 with .1 and
        // then renames the current file onto both.
        let home = temp();
        let mut log = LogFile::with_limits(home.path(), 8, 3).unwrap();
        log.write_line(b"aaaa\n").unwrap();
        log.write_line(b"bbbb\n").unwrap();
        log.write_line(b"cccc\n").unwrap();

        assert_eq!(
            std::fs::read_to_string(home.path().join(FILE_NAME)).unwrap(),
            "cccc\n"
        );
        assert_eq!(
            std::fs::read_to_string(home.path().join(rotated_name(1))).unwrap(),
            "bbbb\n"
        );
        assert_eq!(
            std::fs::read_to_string(home.path().join(rotated_name(2))).unwrap(),
            "aaaa\n"
        );
    }

    #[test]
    fn a_line_longer_than_the_whole_budget_is_still_written_whole() {
        // One event, one line — a reader that greps this file must never meet
        // half an event.
        let home = temp();
        let mut log = LogFile::with_limits(home.path(), 8, 1).unwrap();
        let long = "x".repeat(64);
        log.write_line(format!("{long}\n").as_bytes()).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.path().join(FILE_NAME))
                .unwrap()
                .trim(),
            long
        );
    }

    #[test]
    fn the_marker_survives_a_start_that_never_finished() {
        // Given a start that reached the window stage
        let home = temp();
        note_stage_in(home.path(), stage::WINDOW).unwrap();

        // When the next start looks
        let found = previous_attempt_in(home.path()).expect("the marker is there");

        // Then it says where the last one died, and in what build
        assert_eq!(stage_of(&found), Some(stage::WINDOW));
        assert!(found.contains(crate::VERSION), "{found}");
        assert!(found.contains("at=["), "and when: {found}");
    }

    #[test]
    fn a_start_that_finished_leaves_nothing_behind() {
        let home = temp();
        note_stage_in(home.path(), stage::APP).unwrap();
        assert!(previous_attempt_in(home.path()).is_some());
        finished_in(home.path());
        assert!(
            previous_attempt_in(home.path()).is_none(),
            "a clean start must not accuse the next one"
        );
    }

    #[test]
    fn every_stage_explains_itself_and_an_unknown_one_still_does() {
        // A marker written by a newer build must not produce an empty sentence.
        for stage in [
            stage::START,
            stage::CAMERA_PREFLIGHT,
            stage::SETTINGS,
            stage::WINDOW,
            stage::APP,
        ] {
            assert!(!explain(stage).is_empty(), "{stage} has no explanation");
        }
        assert!(explain("stage-from-the-future").contains("does not recognise"));
    }

    #[test]
    fn describing_the_log_neither_creates_it_nor_fails() {
        let home = temp();
        let described = describe_in(home.path());
        assert!(described.contains("nothing written yet"), "{described}");
        assert!(
            !home.path().join(FILE_NAME).exists(),
            "--diagnose is read-only"
        );

        let mut log = LogFile::with_limits(home.path(), 8, 2).unwrap();
        log.write_line(b"aaaa\n").unwrap();
        log.write_line(b"bbbb\n").unwrap();
        let described = describe_in(home.path());
        assert!(described.contains("KiB"), "{described}");
        assert!(described.contains("1 rotated"), "{described}");
    }

    #[test]
    fn a_marker_without_a_stage_is_read_as_having_none() {
        assert_eq!(stage_of("version=0.0.0"), None);
        assert_eq!(stage_of("stage=window version=0.0.0"), Some("window"));
    }
}
