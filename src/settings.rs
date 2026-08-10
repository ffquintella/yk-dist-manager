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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Opened automatically at startup, when it is still there.
    pub last_database: Option<PathBuf>,
    /// Most recently used first. Never contains duplicates.
    pub recent_databases: Vec<PathBuf>,
    /// Operator name recorded on hand-overs and audit entries.
    pub operator: String,
    /// Organisation, used in certificate subjects.
    pub org: String,
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
            operator: default_operator(),
            org: String::new(),
        }
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

    fn normalise(&mut self) {
        let mut seen = std::collections::BTreeSet::new();
        self.recent_databases
            .retain(|path| !path.as_os_str().is_empty() && seen.insert(path.clone()));
        self.recent_databases.truncate(MAX_RECENT);
        if self.operator.trim().is_empty() {
            self.operator = default_operator();
        }
    }
}

fn default_operator() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".into())
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
    fn forgetting_clears_the_last_database_too() {
        let mut settings = AppSettings::default();
        settings.remember(Path::new("/a.sqlite3"));
        settings.forget(Path::new("/a.sqlite3"));
        assert!(settings.recent_databases.is_empty());
        assert!(settings.last_database.is_none());
    }
}
