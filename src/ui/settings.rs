//! Settings: operator identity, organisation, database location and health.

use crate::app::{DbRequest, YkDistApp};
use crate::store::Location;

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::screen_header(
        ui,
        "Settings",
        "Operator identity and the database file. Nothing here is a secret store.",
    );

    // Deferred: the identity is persisted after the grid closure, not per keypress.
    let mut identity_changed = false;

    egui::Grid::new("settings")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Operator");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut app.operator)
                        .char_limit(crate::domain::MAX_TEXT)
                        .desired_width(280.0),
                )
                .lost_focus()
            {
                identity_changed = true;
            }
            ui.end_row();

            ui.label("Organisation");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut app.org)
                        .char_limit(crate::domain::MAX_TEXT)
                        .desired_width(280.0),
                )
                .lost_focus()
            {
                identity_changed = true;
            }
            ui.end_row();

            ui.label("Database file");
            ui.add(
                egui::Label::new(
                    egui::RichText::new(app.config.path.display().to_string()).monospace(),
                )
                .selectable(true),
            );
            ui.end_row();

            ui.label("Locking mode");
            ui.label(match app.store.as_ref().map(|s| s.location()) {
                Some(Location::NetworkShare) => {
                    "network share — rollback journal, synchronous=FULL, 20s busy timeout"
                }
                Some(Location::LocalDisk) => "local disk — WAL, synchronous=NORMAL",
                None => "—",
            });
            ui.end_row();

            ui.label("Password protection");
            ui.label(match app.store.as_ref().map(|s| s.is_encrypted()) {
                Some(true) => "on (SQLCipher)",
                Some(false) => "off",
                None => "—",
            });
            ui.end_row();

            ui.label("Device transport");
            ui.label(app.backend.describe());
            ui.end_row();
        });

    if identity_changed {
        app.persist_settings();
    }

    ui.add_space(12.0);
    ui.label(egui::RichText::new("Database").strong());
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if ui
            .button("Switch database…")
            .on_hover_text("close this one and choose another")
            .clicked()
        {
            app.db_request = Some(DbRequest::Close);
        }
        if ui.button("Open another…").clicked() {
            app.db_request = Some(DbRequest::PickExisting);
        }
        if ui.button("Create new…").clicked() {
            app.db_request = Some(DbRequest::PickNew);
        }
    });

    let recent = app.settings.recent_with_availability();
    if recent.len() > 1 {
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Recent:").small().weak());
        for (path, available) in recent {
            ui.label(
                egui::RichText::new(format!(
                    "  {}{}",
                    path.display(),
                    if available { "" } else { "  (not reachable)" }
                ))
                .small()
                .weak()
                .monospace(),
            );
        }
    }

    ui.add_space(12.0);
    ui.label(egui::RichText::new("Maintenance").strong());
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if ui.button("Integrity check").clicked()
            && let Some(store) = &app.store
        {
            match store.integrity_check() {
                Ok(result) => app.status = format!("integrity_check: {result}"),
                Err(e) => app.status = format!("integrity check failed: {e}"),
            }
        }
        if ui.button("Backup next to the database").clicked() {
            backup(app);
        }
        if ui.button("Reload from database").clicked() {
            app.refresh();
            app.status = "views reloaded".into();
        }
    });

    ui.add_space(14.0);
    ui.separator();
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(format!(
            "yk-dist-manager {} — the database file is the whole deployment; copying it \
             (or the backup below) copies everything.",
            crate::VERSION
        ))
        .weak()
        .small(),
    );
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
