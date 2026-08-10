//! Where the application keeps its own files.
//!
//! One place, so the database default and the settings file cannot drift apart.

use std::path::PathBuf;

/// Per-user data directory for this application.
///
/// `$YKDM_DATA_DIR` overrides it, which is what the tests use.
pub fn data_dir() -> PathBuf {
    if let Ok(explicit) = std::env::var("YKDM_DATA_DIR")
        && !explicit.trim().is_empty()
    {
        return PathBuf::from(explicit);
    }

    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    let mut path = PathBuf::from(home);
    if cfg!(target_os = "macos") {
        path.push("Library/Application Support/yk-dist-manager");
    } else if cfg!(target_os = "windows") {
        path.push("AppData/Roaming/yk-dist-manager");
    } else {
        path.push(".local/share/yk-dist-manager");
    }
    path
}

/// The conventional file name for a new database, offered by the create dialog.
pub const DEFAULT_DATABASE_NAME: &str = "yk-dist-manager.sqlite3";

/// The extension the file chooser filters on.
pub const DATABASE_EXTENSIONS: [&str; 3] = ["sqlite3", "sqlite", "db"];
