//! The database chooser: open a recent database, pick an existing file, create a
//! new one, or unlock a password-protected one.
//!
//! Shown whenever no database is open — which covers first run, a locked file, an
//! unreachable share, and the operator deliberately closing one to switch.

use std::path::PathBuf;

use crate::app::{DbRequest, YkDistApp};

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    egui::CentralPanel::default().show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(28.0);
            ui.vertical_centered(|ui| {
                ui.heading("YubiKey Distribution Manager");
                ui.label(
                    egui::RichText::new("Choose a distribution database, or create one.").weak(),
                );
            });
            ui.add_space(20.0);

            if let Some(error) = app.open_error.clone() {
                super::error_label(ui, &error);
                ui.add_space(8.0);
            }
            if let Some(error) = app.db_form.error.clone() {
                super::error_label(ui, &error);
                ui.add_space(8.0);
            }

            recent(app, ui);
            ui.add_space(14.0);
            ui.separator();
            ui.add_space(10.0);
            chooser(app, ui);
        });
    });
}

fn recent(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let entries = app.settings.recent_with_availability();
    if entries.is_empty() {
        return;
    }

    ui.label(egui::RichText::new("Recent databases").strong());
    ui.add_space(4.0);

    // Deferred, as everywhere else: the row records an intent and the mutation
    // happens after the grid closure.
    let mut request: Option<DbRequest> = None;
    let mut fill_path: Option<String> = None;

    egui::Grid::new("recent-databases")
        .num_columns(3)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for (path, available) in &entries {
                ui.add(
                    egui::Label::new(egui::RichText::new(path.display().to_string()).monospace())
                        .selectable(true)
                        .truncate(),
                );

                if *available {
                    ui.label(egui::RichText::new("available").small().weak());
                } else {
                    // An unreachable share is usually a mount problem, not a
                    // decision to stop using that database — so it stays listed.
                    ui.label(
                        egui::RichText::new("not reachable")
                            .small()
                            .color(egui::Color32::from_rgb(190, 140, 40)),
                    );
                }

                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(*available, egui::Button::new("open").small())
                        .clicked()
                    {
                        request = Some(DbRequest::Open(path.clone()));
                    }
                    if ui
                        .small_button("use path")
                        .on_hover_text("copy into the field below, to add a password first")
                        .clicked()
                    {
                        fill_path = Some(path.display().to_string());
                    }
                    if ui
                        .small_button("forget")
                        .on_hover_text("remove from this list; the file is not touched")
                        .clicked()
                    {
                        request = Some(DbRequest::Forget(path.clone()));
                    }
                });
                ui.end_row();
            }
        });

    if let Some(path) = fill_path {
        app.db_form.path = path;
    }
    if let Some(request) = request {
        app.db_request = Some(request);
    }
}

fn chooser(app: &mut YkDistApp, ui: &mut egui::Ui) {
    egui::Grid::new("database-form")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Database file");
            ui.add(
                egui::TextEdit::singleline(&mut app.db_form.path)
                    .hint_text("/Volumes/ti-share/yubikeys/yk-dist-manager.sqlite3")
                    .desired_width(460.0),
            );
            ui.end_row();

            ui.label("Password");
            ui.add(
                egui::TextEdit::singleline(&mut app.db_form.password)
                    .password(true)
                    .hint_text("leave empty if the file is not protected")
                    .desired_width(460.0),
            );
            ui.end_row();
        });

    ui.add_space(10.0);

    ui.horizontal_wrapped(|ui| {
        let typed = PathBuf::from(app.db_form.path.trim());
        let has_path = !app.db_form.path.trim().is_empty();

        if ui
            .add_enabled(has_path, egui::Button::new("Open"))
            .on_hover_text("open the database at this path; it must already exist")
            .clicked()
        {
            app.db_request = Some(DbRequest::Open(typed.clone()));
        }
        if ui
            .add_enabled(has_path, egui::Button::new("Create"))
            .on_hover_text("create a new database at this path; refuses if a file is there")
            .clicked()
        {
            app.db_request = Some(DbRequest::Create(typed));
        }

        ui.separator();

        if ui.button("Choose file…").clicked() {
            app.db_request = Some(DbRequest::PickExisting);
        }
        if ui.button("New file…").clicked() {
            app.db_request = Some(DbRequest::PickNew);
        }
    });

    ui.add_space(16.0);
    ui.label(
        egui::RichText::new(
            "Open and Create are deliberately separate: opening a path that does not exist \
             is an error rather than a silently empty database, and creating over an existing \
             file is refused.",
        )
        .small()
        .weak(),
    );
    if !cfg!(feature = "file-dialog") {
        ui.label(
            egui::RichText::new(
                "File dialogs need a build with `--features file-dialog`; the path field always \
                 works.",
            )
            .small()
            .weak(),
        );
    }
    if !cfg!(feature = "encrypted-db") {
        ui.label(
            egui::RichText::new(
                "Password protection needs a build with `--features encrypted-db`.",
            )
            .small()
            .weak(),
        );
    }
}
