//! Application settings: which database was last used, the recent ones, and the
//! operator identity.
//!
//! These live **outside** the database, because they are what lets an operator
//! choose *which* database to open. The file is a small JSON document in the
//! per-user data directory.
//!
//! **It never contains a database password.** Passwords are typed at unlock and
//! held only for the duration of the open call; storing one here would make the
//! encryption pointless, since the file sits next to the database.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How many databases are remembered in the picker.
pub const MAX_RECENT: usize = 8;

/// The palettes the operator can pick from, in the order they are offered.
/// These are the four built-in `egui-elegance` themes: two dark, two light.
pub const THEMES: [&str; 4] = ["slate", "charcoal", "frost", "paper"];

/// What the application opens with, and what an unrecognised name falls back
/// to. Dark, cool blue.
pub const DEFAULT_THEME: &str = "slate";

/// Resolve a stored palette name to one of [`THEMES`].
///
/// A name that is unknown, differently cased, or simply absent resolves to
/// [`DEFAULT_THEME`] — a settings file written by a newer build that offers
/// more palettes must still open here, with a theme rather than an error.
pub fn normalise_theme(name: &str) -> &'static str {
    let wanted = name.trim();
    THEMES
        .into_iter()
        .find(|known| known.eq_ignore_ascii_case(wanted))
        .unwrap_or(DEFAULT_THEME)
}

/// An SMB share the operator has used, and how they reached it.
///
/// **Never a password.** The user name and the access mode are remembered because
/// retyping them at every hand-over is how a wrong share gets opened; the password
/// is typed every time, for the same reason the database password is not stored —
/// this file sits next to the register.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShareEntry {
    /// Canonical location, `//server/share/path/to/database.sqlite3`.
    pub location: String,
    /// Which identity was used last time.
    pub access: crate::store::smb::Access,
    /// `DOMAIN\user` or `user`, when the access mode was a named account.
    pub user: String,
}

/// Window geometry and the screen that was open, remembered between sessions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    pub width: f32,
    pub height: f32,
    /// Screen the operator was last on, by its stable name rather than an index,
    /// so adding a tab does not reopen the wrong one.
    pub tab: String,
    pub maximised: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: 1180.0,
            height: 820.0,
            tab: "Inventory".into(),
            maximised: false,
        }
    }
}

impl WindowState {
    /// The smallest window the screens still lay out in.
    pub const MIN_WIDTH: f32 = 800.0;
    pub const MIN_HEIGHT: f32 = 600.0;
    /// A ceiling that catches a stored value from a monitor that is no longer
    /// attached — a 6000px window on a laptop is a window the operator cannot
    /// reach the close button of.
    pub const MAX_WIDTH: f32 = 6000.0;
    pub const MAX_HEIGHT: f32 = 4000.0;

    /// The size to actually open at.
    ///
    /// Clamped rather than trusted: the file is editable, the monitor may have
    /// changed, and a NaN from a half-written write should not produce a window
    /// with no dimensions.
    pub fn size(&self) -> (f32, f32) {
        let sane = |value: f32, min: f32, max: f32, fallback: f32| {
            if value.is_finite() {
                value.clamp(min, max)
            } else {
                fallback
            }
        };
        let default = Self::default();
        (
            sane(self.width, Self::MIN_WIDTH, Self::MAX_WIDTH, default.width),
            sane(
                self.height,
                Self::MIN_HEIGHT,
                Self::MAX_HEIGHT,
                default.height,
            ),
        )
    }

    /// The screen to reopen, given the ones that exist in this build.
    ///
    /// A remembered tab that no longer exists falls back to the first, so
    /// renaming or removing a screen cannot leave the app opening onto nothing.
    pub fn tab_or_first<'a>(&self, available: &[&'a str]) -> &'a str {
        available
            .iter()
            .find(|name| **name == self.tab)
            .copied()
            .or_else(|| available.first().copied())
            .unwrap_or("Inventory")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Opened automatically at startup, when it is still there.
    pub last_database: Option<PathBuf>,
    /// Most recently used first. Never contains duplicates.
    pub recent_databases: Vec<PathBuf>,
    /// SMB shares the operator has opened the register from, most recent first.
    ///
    /// Kept apart from `recent_databases`, which holds *local* paths: the path a
    /// share resolves to is a mount point that changes between workstations and
    /// between sessions, so remembering the path would remember the wrong thing.
    /// The share is what is stable.
    pub recent_shares: Vec<ShareEntry>,
    /// Operator name recorded on hand-overs and audit entries.
    pub operator: String,
    /// Organisation, used in certificate subjects.
    pub org: String,
    /// Where the window was, and which screen was open, last time.
    ///
    /// `features/gui-shell.md` phase 5. Cosmetic, and deliberately so: nothing
    /// about the register depends on it, a missing or absurd value falls back to
    /// the default rather than refusing to start, and it is never a reason to
    /// fail an open.
    pub window: WindowState,
    /// How the signing certificate's `rfc822Name` SAN is produced.
    ///
    /// Here rather than in a template because it follows from *which CA* the
    /// deployment uses (roadmap open question 1), not from which procedure is
    /// being run: every template on a given deployment wants the same answer.
    /// See [`crate::san`].
    pub san: crate::san::SanPolicy,
    /// Chosen palette, one of [`THEMES`]. Cosmetic only — nothing about the
    /// record depends on it.
    pub theme: String,
}

impl AppSettings {
    /// `$YKDM_SETTINGS`, else `<data dir>/settings.json`.
    pub fn path() -> PathBuf {
        if let Ok(explicit) = std::env::var("YKDM_SETTINGS")
            && !explicit.trim().is_empty()
        {
            return PathBuf::from(explicit);
        }
        crate::paths::data_dir().join("settings.json")
    }

    /// Load, falling back to defaults. A missing file is normal (first run); a
    /// corrupt one is logged and replaced rather than fatal — losing the recent
    /// list must never stop the operator from working.
    pub fn load() -> Self {
        let path = Self::path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::first_run(),
            Err(e) => {
                tracing::warn!(event = "settings.read.failed", path = %path.display(), reason = %e);
                return Self::first_run();
            }
        };

        match serde_json::from_str::<Self>(&raw) {
            Ok(mut settings) => {
                settings.normalise();
                settings
            }
            Err(e) => {
                tracing::error!(event = "settings.parse.failed", path = %path.display(), reason = %e);
                Self::first_run()
            }
        }
    }

    fn first_run() -> Self {
        Self {
            last_database: None,
            recent_databases: Vec::new(),
            recent_shares: Vec::new(),
            operator: default_operator(),
            org: String::new(),
            window: WindowState::default(),
            san: crate::san::SanPolicy::default(),
            theme: DEFAULT_THEME.to_owned(),
        }
    }

    /// The chosen palette, always one of [`THEMES`].
    pub fn theme(&self) -> &'static str {
        normalise_theme(&self.theme)
    }

    /// Record the operator's palette choice.
    pub fn set_theme(&mut self, name: &str) {
        self.theme = normalise_theme(name).to_owned();
    }

    /// Write atomically: a crash mid-write must not leave an unreadable file.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, body)?;
        std::fs::rename(&temporary, &path)
    }

    /// Save, logging rather than propagating: failing to remember a path is not
    /// worth interrupting a hand-over for.
    pub fn save_quietly(&self) {
        if let Err(e) = self.save() {
            tracing::warn!(event = "settings.write.failed", reason = %e);
        }
    }

    /// Record a database as the current one and move it to the front of the
    /// recent list.
    pub fn remember(&mut self, database: &Path) {
        let database = database.to_path_buf();
        self.recent_databases.retain(|known| known != &database);
        self.recent_databases.insert(0, database.clone());
        self.recent_databases.truncate(MAX_RECENT);
        self.last_database = Some(database);
    }

    /// Drop a database from the recent list (e.g. the operator no longer wants a
    /// share they cannot reach listed).
    pub fn forget(&mut self, database: &Path) {
        self.recent_databases.retain(|known| known != database);
        if self.last_database.as_deref() == Some(database) {
            self.last_database = None;
        }
    }

    /// Recent entries paired with whether the file is reachable right now.
    ///
    /// A missing entry is kept, not silently dropped: an unreachable share is
    /// usually a network problem, not a decision to stop using that database.
    pub fn recent_with_availability(&self) -> Vec<(PathBuf, bool)> {
        self.recent_databases
            .iter()
            .map(|path| (path.clone(), path.is_file()))
            .collect()
    }

    /// Record an SMB share as the most recently used, with the identity that
    /// reached it. The password is not a parameter here, and cannot be.
    pub fn remember_share(&mut self, entry: ShareEntry) {
        if entry.location.trim().is_empty() {
            return;
        }
        self.recent_shares
            .retain(|known| known.location != entry.location);
        self.recent_shares.insert(0, entry);
        self.recent_shares.truncate(MAX_RECENT);
    }

    /// Drop a share from the list. The share itself is not touched.
    pub fn forget_share(&mut self, location: &str) {
        self.recent_shares
            .retain(|known| known.location != location);
    }

    fn normalise(&mut self) {
        let mut seen = std::collections::BTreeSet::new();
        self.recent_databases
            .retain(|path| !path.as_os_str().is_empty() && seen.insert(path.clone()));
        self.recent_databases.truncate(MAX_RECENT);

        let mut shares = std::collections::BTreeSet::new();
        self.recent_shares.retain(|entry| {
            !entry.location.trim().is_empty() && shares.insert(entry.location.clone())
        });
        self.recent_shares.truncate(MAX_RECENT);
        if self.operator.trim().is_empty() {
            self.operator = default_operator();
        }
        self.theme = normalise_theme(&self.theme).to_owned();
    }
}

/// The logged-in user, which is the operator until somebody types another name.
///
/// Shared with [`crate::store::cloud`] so the name in a cloud lock file is the
/// same name the audit trail records.
fn default_operator() -> String {
    crate::store::cloud::local_operator()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembering_moves_an_entry_to_the_front_without_duplicating_it() {
        let mut settings = AppSettings::default();
        settings.remember(Path::new("/a.sqlite3"));
        settings.remember(Path::new("/b.sqlite3"));
        settings.remember(Path::new("/a.sqlite3"));

        assert_eq!(
            settings.recent_databases,
            vec![PathBuf::from("/a.sqlite3"), PathBuf::from("/b.sqlite3")]
        );
        assert_eq!(
            settings.last_database.as_deref(),
            Some(Path::new("/a.sqlite3"))
        );
    }

    #[test]
    fn the_recent_list_is_capped() {
        let mut settings = AppSettings::default();
        for n in 0..(MAX_RECENT + 5) {
            settings.remember(Path::new(&format!("/db-{n}.sqlite3")));
        }
        assert_eq!(settings.recent_databases.len(), MAX_RECENT);
        // The newest survives, the oldest is gone.
        assert_eq!(
            settings.recent_databases[0],
            PathBuf::from(&format!("/db-{}.sqlite3", MAX_RECENT + 4))
        );
        assert!(
            !settings
                .recent_databases
                .contains(&PathBuf::from("/db-0.sqlite3"))
        );
    }

    #[test]
    fn an_unknown_palette_name_falls_back_to_the_default() {
        assert_eq!(normalise_theme("frost"), "frost");
        // Case is not the operator's problem, and neither is a hand-edited file.
        assert_eq!(normalise_theme("  Charcoal "), "charcoal");
        assert_eq!(normalise_theme("neon"), DEFAULT_THEME);
        assert_eq!(normalise_theme(""), DEFAULT_THEME);
    }

    #[test]
    fn a_settings_file_without_a_theme_still_loads_with_one() {
        // What `#[serde(default)]` produces for a file written before the
        // field existed.
        let mut settings = AppSettings::default();
        assert!(settings.theme.is_empty());
        settings.normalise();
        assert_eq!(settings.theme, DEFAULT_THEME);
        assert_eq!(settings.theme(), DEFAULT_THEME);
    }

    #[test]
    fn a_remembered_share_keeps_the_identity_and_never_the_password() {
        let mut settings = AppSettings::default();
        settings.remember_share(ShareEntry {
            location: "//fileserver/ti-share/keys.sqlite3".into(),
            access: crate::store::smb::Access::Named,
            user: r"FGV\felipe".into(),
        });
        settings.remember_share(ShareEntry {
            location: "//nas/public/keys.sqlite3".into(),
            access: crate::store::smb::Access::Anonymous,
            user: String::new(),
        });
        // Re-using the first share moves it back to the front without a duplicate.
        settings.remember_share(ShareEntry {
            location: "//fileserver/ti-share/keys.sqlite3".into(),
            access: crate::store::smb::Access::LoggedOnUser,
            user: String::new(),
        });

        assert_eq!(settings.recent_shares.len(), 2);
        assert_eq!(
            settings.recent_shares[0].location,
            "//fileserver/ti-share/keys.sqlite3"
        );
        // The newer choice wins: the operator switched to the signed-in user.
        assert_eq!(
            settings.recent_shares[0].access,
            crate::store::smb::Access::LoggedOnUser
        );

        // The serialised form is the contract that matters: there is nowhere in it
        // for a password to hide.
        let json = serde_json::to_string_pretty(&settings).unwrap();
        assert!(json.contains("recent_shares"), "{json}");
        assert!(json.contains("logged-on-user"), "{json}");
        assert!(!json.to_lowercase().contains("password"), "{json}");
        assert!(!json.to_lowercase().contains("secret"), "{json}");

        settings.forget_share("//fileserver/ti-share/keys.sqlite3");
        assert_eq!(settings.recent_shares.len(), 1);
    }

    #[test]
    fn a_hand_edited_share_list_is_normalised_rather_than_trusted() {
        let raw = r#"{
            "recent_shares": [
                {"location": "//nas/public/keys.sqlite3", "access": "anonymous", "user": ""},
                {"location": "//nas/public/keys.sqlite3", "access": "named", "user": "felipe"},
                {"location": "  ", "access": "logged-on-user", "user": ""}
            ]
        }"#;
        let mut settings: AppSettings = serde_json::from_str(raw).unwrap();
        settings.normalise();
        assert_eq!(settings.recent_shares.len(), 1);
        assert_eq!(
            settings.recent_shares[0].access,
            crate::store::smb::Access::Anonymous,
            "the first entry wins, as it does for databases"
        );

        // A file written before shares existed still loads, with none.
        let old: AppSettings = serde_json::from_str("{\"operator\": \"felipe\"}").unwrap();
        assert!(old.recent_shares.is_empty());
    }

    #[test]
    fn the_share_list_never_exceeds_its_cap() {
        let mut settings = AppSettings::default();
        for n in 0..(MAX_RECENT + 3) {
            settings.remember_share(ShareEntry {
                location: format!("//nas/share-{n}/keys.sqlite3"),
                ..ShareEntry::default()
            });
        }
        assert_eq!(settings.recent_shares.len(), MAX_RECENT);
    }

    #[test]
    fn forgetting_clears_the_last_database_too() {
        let mut settings = AppSettings::default();
        settings.remember(Path::new("/a.sqlite3"));
        settings.forget(Path::new("/a.sqlite3"));
        assert!(settings.recent_databases.is_empty());
        assert!(settings.last_database.is_none());
    }
}
