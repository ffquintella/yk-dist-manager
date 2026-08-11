//! Settings: operator identity, appearance, database location and health.

use elegance::{Accent, Button, CalloutTone, Select};

use crate::app::{DbRequest, YkDistApp};
use crate::domain::MAX_TEXT;
use crate::store::Location;

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::screen_header(
        ui,
        "Settings",
        "Operator identity and the database file. Nothing here is a secret store.",
    );

    identity(app, ui);
    ui.add_space(16.0);
    database_card(app, ui);
    ui.add_space(16.0);
    maintenance(app, ui);

    ui.add_space(16.0);
    super::hint(
        ui,
        &format!(
            "yk-dist-manager {} — the database file is the whole deployment; copying it \
             (or a backup) copies everything.",
            crate::VERSION
        ),
    );
}

/// Who is operating, for whom, and what the app looks like while they do it.
fn identity(app: &mut YkDistApp, ui: &mut egui::Ui) {
    // Deferred: the identity is persisted when a field loses focus, not on
    // every keypress.
    let mut identity_changed = false;

    super::titled_card(ui, "Operator", |ui| {
        super::form_columns(ui, |left, right, _width| {
            if super::capped_input(left, &mut app.operator, MAX_TEXT, |input| {
                input.label("Operator").id_salt("settings-operator")
            })
            .lost_focus()
            {
                identity_changed = true;
            }
            if super::capped_input(right, &mut app.org, MAX_TEXT, |input| {
                input
                    .label("Organisation")
                    .hint("your unit or institution — it reaches the certificate subject")
                    .id_salt("settings-org")
            })
            .lost_focus()
            {
                identity_changed = true;
            }
        });

        // The organisation is not something this application can know, and it is
        // not cosmetic: `{{org}}` is interpolated into the PIV certificate subject
        // and the FIDO2 relying-party id. So the placeholder is called out rather
        // than left to be discovered on a certificate.
        if app.org.trim() == crate::app::DEFAULT_ORG || app.org.trim().is_empty() {
            ui.add_space(8.0);
            super::notice(
                ui,
                CalloutTone::Warning,
                "Set the organisation before bootstrapping a key: it goes into the certificate \
                 subject and the FIDO2 relying-party id of every key this tool prepares.",
            );
        }

        ui.add_space(12.0);
        appearance(app, ui);
    });

    if identity_changed {
        app.persist_settings();
    }
}

/// The palette picker. Cosmetic, and remembered between sessions.
fn appearance(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let before = app.settings.theme();
    let mut chosen = before.to_owned();

    ui.add(
        Select::strings("settings-theme", &mut chosen, crate::settings::THEMES)
            .label("Theme")
            .width(180.0),
    );
    super::hint(
        ui,
        "Slate and Charcoal are dark, Frost and Paper light. The choice is remembered \
         per user and changes nothing about the record.",
    );

    if chosen != before {
        app.settings.set_theme(&chosen);
        app.settings.save_quietly();
    }
}

/// Where the database is, how it is being held open, and how to change it.
fn database_card(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::titled_card(ui, "Database", |ui| {
        // A label/value grid, not a table: no header row, and the path is the
        // one value allowed to be longer than the window.
        egui::ScrollArea::horizontal()
            .id_salt("settings-database-scroll")
            .show(ui, |ui| {
                egui::Grid::new("settings-database")
            .num_columns(2)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                ui.label("File");
                super::mono(ui, &app.config.path.display().to_string());
                ui.end_row();

                ui.label("Locking mode");
                super::faint(
                    ui,
                    match app.store.as_ref().map(|s| s.location()) {
                        Some(Location::NetworkShare) => {
                            "network share — rollback journal, synchronous=FULL, 20s busy timeout"
                        }
                        Some(Location::LocalDisk) => "local disk — WAL, synchronous=NORMAL",
                        Some(Location::CloudSync) => {
                            "cloud-sync folder — rollback journal, synchronous=FULL, plus a \
                             single-writer lock file"
                        }
                        None => "—",
                    },
                );
                ui.end_row();

                // The lock is the whole answer to "can two of us use this?", so
                // it gets a row of its own rather than a footnote.
                if let Some(lease) = app.store.as_ref().and_then(|s| s.lease()) {
                    ui.label("Single-writer lock");
                    ui.vertical(|ui| {
                        ui.add(elegance::Badge::new(
                            "held by this workstation",
                            elegance::BadgeTone::Ok,
                        ));
                        super::faint(ui, &lease.holder().to_string());
                        super::mono(ui, &lease.lock_file().display().to_string());
                    });
                    ui.end_row();
                }

                ui.label("Password protection");
                match app.store.as_ref().map(|s| s.is_encrypted()) {
                    Some(true) => {
                        ui.add(elegance::Badge::new(
                            "on (SQLCipher)",
                            elegance::BadgeTone::Ok,
                        ));
                    }
                    Some(false) => {
                        ui.add(elegance::Badge::new("off", elegance::BadgeTone::Neutral));
                    }
                    None => {
                        super::faint(ui, "—");
                    }
                }
                ui.end_row();

                ui.label("Device transport");
                super::faint(ui, &app.backend.describe());
                ui.end_row();
            });
            });

        // A cloud-sync folder is a data-loss risk, not a note in a table — and
        // what the lock does and does not cover has to be said in the same breath,
        // or "locked" reads as "solved".
        if app.store.as_ref().is_some_and(|s| s.on_cloud_sync()) {
            ui.add_space(10.0);
            let locked = app.store.as_ref().is_some_and(|s| s.lease().is_some());
            if locked {
                super::notice(
                    ui,
                    CalloutTone::Warning,
                    "This database is in a cloud-sync folder. One workstation at a time may open \
                     it: this session holds the lock file next to the database, and another \
                     computer is refused by name until it is released. Close the database (or the \
                     application) before working on it elsewhere, and give the sync client time \
                     to finish uploading. The lock only binds workstations running this tool — a \
                     network share, or a local file with a scheduled backup, is still the safer \
                     home.",
                );
            } else {
                super::error_label(
                    ui,
                    "this database is in a cloud-sync folder and no single-writer lock is held — \
                     a sync client can copy the file mid-write, and resolves a clash by keeping \
                     both copies rather than merging. Reopen it without the lock disabled, move \
                     it to a network share, or keep it local and back it up.",
                );
            }
        }

        // Copies a sync client could not merge: the register may already have
        // forked, and that is not a warning to leave in a log file.
        let conflicts: Vec<String> = app
            .store
            .as_ref()
            .map(|store| {
                store
                    .conflict_copies()
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect()
            })
            .unwrap_or_default();
        if !conflicts.is_empty() {
            ui.add_space(10.0);
            super::error_label(
                ui,
                &format!(
                    "the sync client left {} copy/copies it could not merge next to this \
                     database: {}. Two operators may have written to different versions of the \
                     register — compare them before trusting either.",
                    conflicts.len(),
                    conflicts.join(", ")
                ),
            );
        }

        ui.add_space(14.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(Button::new("Switch database…").outline())
                .on_hover_text("close this one and choose another")
                .clicked()
            {
                app.db_request = Some(DbRequest::Close);
            }
            if ui.add(Button::new("Open another…").outline()).clicked() {
                app.db_request = Some(DbRequest::PickExisting);
            }
            if ui.add(Button::new("Create new…").outline()).clicked() {
                app.db_request = Some(DbRequest::PickNew);
            }
        });

        let recent = app.settings.recent_with_availability();
        if recent.len() > 1 {
            ui.add_space(10.0);
            super::hint(ui, "Recent:");
            for (path, available) in recent {
                super::faint(
                    ui,
                    &format!(
                        "{}{}",
                        path.display(),
                        if available { "" } else { "  (not reachable)" }
                    ),
                );
            }
        }
    });
}

/// Integrity, backup, reload.
fn maintenance(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::titled_card(ui, "Maintenance", |ui| {
        ui.horizontal_wrapped(|ui| {
            if ui.add(Button::new("Integrity check").outline()).clicked()
                && let Some(store) = &app.store
            {
                match store.integrity_check() {
                    Ok(result) => app.status = format!("integrity_check: {result}"),
                    Err(e) => app.status = format!("integrity check failed: {e}"),
                }
            }
            if ui
                .add(Button::new("Backup next to the database").accent(Accent::Green))
                .clicked()
            {
                backup(app);
            }
            if ui
                .add(Button::new("Reload from database").outline())
                .clicked()
            {
                app.refresh();
                app.status = "views reloaded".into();
            }
        });
    });
}

fn backup(app: &mut YkDistApp) {
    let Some(store) = &app.store else { return };
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let target = store
        .path()
        .with_extension(format!("{stamp}.backup.sqlite3"));
    match store.backup_to(&target) {
        Ok(()) => {
            let target_display = target.display().to_string();
            app.record("db.backup", "database", &target_display);
            app.status = format!("backup written to {target_display}");
        }
        Err(e) => app.status = format!("backup failed: {e}"),
    }
}
