//! The database chooser: open a recent database, pick an existing file, create a
//! new one, or unlock a password-protected one.
//!
//! Shown whenever no database is open — which covers first run, a locked file, an
//! unreachable share, and the operator deliberately closing one to switch.

use std::path::PathBuf;

use elegance::{Accent, Button, CalloutTone, Theme};

use crate::app::{DbRequest, YkDistApp};
use crate::domain::MAX_TEXT;

/// The chooser is a single column on a wide window; anything wider than this
/// and the eye has to travel between the label and its field.
const COLUMN: f32 = 620.0;

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    egui::CentralPanel::default().show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            let theme = Theme::current(ui.ctx());
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.add(egui::Label::new(
                    egui::RichText::new("YubiKey Distribution Manager")
                        .size(theme.typography.heading + 10.0)
                        .color(theme.palette.text)
                        .strong(),
                ));
                ui.add_space(4.0);
                ui.add(egui::Label::new(
                    egui::RichText::new("Choose a distribution database, or create one.")
                        .size(theme.typography.body)
                        .color(theme.palette.text_muted),
                ));
            });
            ui.add_space(24.0);

            // Centre the column without stretching the cards to the window.
            let margin = ((ui.available_width() - COLUMN) * 0.5).max(0.0);
            ui.horizontal(|ui| {
                ui.add_space(margin);
                ui.vertical(|ui| {
                    ui.set_width(COLUMN.min(ui.available_width()));

                    if let Some(error) = app.open_error.clone() {
                        super::error_label(ui, &error);
                        ui.add_space(10.0);
                    }
                    if let Some(error) = app.db_form.error.clone() {
                        super::error_label(ui, &error);
                        ui.add_space(10.0);
                    }

                    locked(app, ui);
                    recent(app, ui);
                    chooser(app, ui);
                    ui.add_space(14.0);
                    share(app, ui);
                    ui.add_space(14.0);
                    build_notes(ui);
                });
            });
            ui.add_space(40.0);
        });
    });
}

/// The refusal an operator gets when another workstation has the register.
///
/// A card rather than a line, because it carries an action nobody should take by
/// accident: breaking a lock that a live session still holds is how a sync folder
/// produces two divergent registers, so the button appears only once the holder
/// has gone silent long enough to count as abandoned, and it says whose lock it is
/// breaking.
fn locked(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let Some(locked) = app.db_form.locked.as_ref() else {
        return;
    };
    let path = locked.path.clone();
    let holder = locked.holder.clone();
    let stale = locked.stale;
    let same_host = locked.same_host;
    let mut request = None;

    super::titled_card(ui, "In use by another workstation", |ui| {
        super::mono(ui, &path.display().to_string());
        ui.add_space(6.0);
        super::faint(ui, &holder);
        if same_host {
            super::hint(
                ui,
                "That is this computer — the database is probably open in another window of \
                 this application.",
            );
        }
        ui.add_space(10.0);

        if stale {
            super::notice(
                ui,
                CalloutTone::Warning,
                "That session has not refreshed its lock for over fifteen minutes, so it is \
                 probably gone — a crash, a closed laptop, or a machine switched off. If you can \
                 confirm nobody is working in this database, you can take the lock over. If that \
                 operator is mid-hand-over, taking it over risks two divergent registers.",
            );
            ui.add_space(10.0);
            if ui
                .add(Button::new("Take the lock over").accent(Accent::Red))
                .on_hover_text("records who was holding it, in the audit trail")
                .clicked()
            {
                request = Some(DbRequest::TakeOverLock(path.clone()));
            }
        } else {
            super::notice(
                ui,
                CalloutTone::Neutral,
                "That session is alive — it refreshed its lock in the last few minutes. Wait for \
                 it to close the database, or ask that operator to.",
            );
            ui.add_space(10.0);
            if ui
                .add(Button::new("Try again").outline())
                .on_hover_text("check whether the lock has been released")
                .clicked()
            {
                request = Some(DbRequest::Open(path.clone()));
            }
        }
    });
    ui.add_space(14.0);

    if let Some(request) = request {
        app.db_request = Some(request);
    }
}

fn recent(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let entries = app.settings.recent_with_availability();
    if entries.is_empty() {
        return;
    }

    // Deferred, as everywhere else: the row records an intent and the mutation
    // happens after the grid closure.
    let mut request: Option<DbRequest> = None;
    let mut fill_path: Option<String> = None;

    super::titled_card(ui, "Recent databases", |ui| {
        egui::Grid::new("recent-databases")
            .num_columns(3)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                for (path, available) in &entries {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(path.display().to_string())
                                .monospace()
                                .size(Theme::current(ui.ctx()).typography.monospace),
                        )
                        .selectable(true)
                        .truncate(),
                    );

                    if *available {
                        ui.add(elegance::Badge::new("available", elegance::BadgeTone::Ok));
                    } else {
                        // An unreachable share is usually a mount problem, not a
                        // decision to stop using that database — so it stays listed.
                        ui.add(elegance::Badge::new(
                            "not reachable",
                            elegance::BadgeTone::Warning,
                        ));
                    }

                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                Button::new("open")
                                    .size(elegance::ButtonSize::Small)
                                    .enabled(*available),
                            )
                            .clicked()
                        {
                            request = Some(DbRequest::Open(path.clone()));
                        }
                        if super::row_button(ui, "use path")
                            .on_hover_text("copy into the field below, to add a password first")
                            .clicked()
                        {
                            fill_path = Some(path.display().to_string());
                        }
                        if super::row_button(ui, "forget")
                            .on_hover_text("remove from this list; the file is not touched")
                            .clicked()
                        {
                            request = Some(DbRequest::Forget(path.clone()));
                        }
                    });
                    ui.end_row();
                }
            });
    });
    ui.add_space(14.0);

    if let Some(path) = fill_path {
        app.db_form.path = path;
    }
    if let Some(request) = request {
        app.db_request = Some(request);
    }
}

fn chooser(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::titled_card(ui, "Open or create", |ui| {
        // Both fields take the column: a share path is long, and a truncated
        // path is the kind of thing an operator retypes rather than reads.
        super::capped_input(
            ui,
            &mut app.db_form.path,
            crate::domain::MAX_NOTE,
            |input| {
                input
                    .label("Database file")
                    .hint("/Volumes/ti-share/yubikeys/yk-dist-manager.sqlite3")
                    .id_salt("db-path")
            },
        );
        ui.add_space(8.0);
        super::capped_input(
            ui,
            &mut app.db_form.password,
            crate::domain::MAX_TEXT,
            |input| {
                input
                    .label("Password")
                    .hint("leave empty if the file is not protected")
                    .password(true)
                    .id_salt("db-password")
            },
        );

        ui.add_space(14.0);

        let typed = PathBuf::from(app.db_form.path.trim());
        let has_path = !app.db_form.path.trim().is_empty();

        ui.horizontal_wrapped(|ui| {
            if ui
                .add(Button::new("Open").enabled(has_path))
                .on_hover_text("open the database at this path; it must already exist")
                .clicked()
            {
                app.db_request = Some(DbRequest::Open(typed.clone()));
            }
            if ui
                .add(
                    Button::new("Create")
                        .accent(Accent::Green)
                        .enabled(has_path),
                )
                .on_hover_text("create a new database at this path; refuses if a file is there")
                .clicked()
            {
                app.db_request = Some(DbRequest::Create(typed));
            }

            ui.add_space(8.0);

            if ui.add(Button::new("Choose file…").outline()).clicked() {
                app.db_request = Some(DbRequest::PickExisting);
            }
            if ui.add(Button::new("New file…").outline()).clicked() {
                app.db_request = Some(DbRequest::PickNew);
            }
        });

        ui.add_space(12.0);
        super::hint(
            ui,
            "Open and Create are deliberately separate: opening a path that does not exist \
             is an error rather than a silently empty database, and creating over an existing \
             file is refused.",
        );
    });
}

/// The SMB share card: reach the unit's file server from here.
///
/// A card of its own rather than a second meaning for the path field, because the
/// three things it needs — a share, an identity, and a password that is typed and
/// dropped — have nothing to do with opening a local file, and because the identity
/// is a choice with consequences an operator should see while making it.
fn share(app: &mut YkDistApp, ui: &mut egui::Ui) {
    use crate::store::smb::Access;

    let mut request: Option<DbRequest> = None;

    super::titled_card(ui, "Open from a network share (SMB)", |ui| {
        if let Some(error) = app.share_form.error.clone() {
            super::error_label(ui, &error);
            ui.add_space(10.0);
        }

        remembered_shares(app, ui, &mut request);

        super::capped_input(
            ui,
            &mut app.share_form.location,
            crate::store::smb::MAX_LOCATION,
            |input| {
                input
                    .label("Share and database")
                    .hint(r"smb://fileserver/ti-share/yubikeys/keys.sqlite3")
                    .id_salt("share-location")
            },
        );
        super::hint(
            ui,
            "Also accepted: \\\\fileserver\\ti-share\\yubikeys\\keys.sqlite3 and \
             //fileserver/ti-share/… — whichever your platform gave you. The file inside the \
             share has to be named; a share on its own is not a register.",
        );

        ui.add_space(12.0);

        // Radios rather than a dropdown: each option needs a sentence, and the
        // consequence of picking the wrong one is a register opened as an identity
        // nobody reviewed.
        ui.label("Identity");
        ui.add_space(4.0);
        for option in Access::ALL {
            let label = match option {
                Access::LoggedOnUser => "The account I am signed in with",
                Access::Anonymous => "Guest (no user name, no password)",
                Access::Named => "A named account",
            };
            ui.radio_value(&mut app.share_form.access, option, label);
        }
        super::hint(
            ui,
            if cfg!(windows) {
                "On Windows the signed-in account is the whole mechanism: the share is opened \
                 with this session's own credentials, exactly as Explorer does, and no password \
                 is sent or stored. Choose a named account only when the share needs a different \
                 one."
            } else {
                "\"The account I am signed in with\" uses the credentials macOS already holds \
                 for that server (its Keychain entry). Choose a named account when the share \
                 needs a different one."
            },
        );

        if app.share_form.access == Access::Named {
            ui.add_space(10.0);
            super::form_columns(ui, |left, right, _width| {
                super::capped_input(left, &mut app.share_form.user, MAX_TEXT, |input| {
                    input
                        .label("User")
                        .hint(r"DOMAIN\user, or just user")
                        .id_salt("share-user")
                });
                super::capped_input(right, &mut app.share_form.password, MAX_TEXT, |input| {
                    input
                        .label("Password")
                        .hint("typed every time; never stored")
                        .password(true)
                        .id_salt("share-password")
                });
            });
            super::hint(
                ui,
                "The password is used for this connection and then dropped. It is never written \
                 to the settings file, the database, a log or the audit trail — the share and the \
                 user name are remembered, so only the password has to be retyped.",
            );
        }

        ui.add_space(14.0);

        let has_location = !app.share_form.location.trim().is_empty();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(Button::new("Connect and open").enabled(has_location))
                .on_hover_text("reach the share, then open the database on it; it must exist")
                .clicked()
            {
                request = Some(DbRequest::ConnectShare { create: false });
            }
            if ui
                .add(
                    Button::new("Connect and create")
                        .accent(Accent::Green)
                        .enabled(has_location),
                )
                .on_hover_text(
                    "reach the share, then create a database on it; refuses if a file \
                                is already there",
                )
                .clicked()
            {
                request = Some(DbRequest::ConnectShare { create: true });
            }
        });

        if !crate::store::smb::can_connect() {
            ui.add_space(12.0);
            super::notice(
                ui,
                CalloutTone::Neutral,
                "On this platform an unprivileged process cannot mount a CIFS share, so the \
                 share has to be mounted by the system — from /etc/fstab with mount.cifs, or by \
                 an autofs map. A share that is already mounted is found automatically and \
                 works normally; the path field above also accepts its mount point directly.",
            );
        }
    });

    if let Some(request) = request {
        app.db_request = Some(request);
    }
}

/// Shares this workstation has used before — the share and the identity, never a
/// password.
fn remembered_shares(app: &mut YkDistApp, ui: &mut egui::Ui, request: &mut Option<DbRequest>) {
    if app.settings.recent_shares.is_empty() {
        return;
    }
    let entries = app.settings.recent_shares.clone();

    egui::Grid::new("recent-shares")
        .num_columns(3)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            for entry in &entries {
                super::mono(ui, &entry.location);
                super::faint(
                    ui,
                    &if entry.user.is_empty() {
                        entry.access.label().to_owned()
                    } else {
                        entry.user.clone()
                    },
                );
                ui.horizontal(|ui| {
                    if super::row_button(ui, "use")
                        .on_hover_text("fill the fields below; the password is never remembered")
                        .clicked()
                    {
                        *request = Some(DbRequest::UseShare(entry.location.clone()));
                    }
                    if super::row_button(ui, "forget")
                        .on_hover_text("remove from this list; the share is not touched")
                        .clicked()
                    {
                        *request = Some(DbRequest::ForgetShare(entry.location.clone()));
                    }
                });
                ui.end_row();
            }
        });
    ui.add_space(12.0);
}

/// What this particular build can and cannot do.
fn build_notes(ui: &mut egui::Ui) {
    if !cfg!(feature = "file-dialog") {
        super::notice(
            ui,
            CalloutTone::Neutral,
            "File dialogs need a build with `--features file-dialog`; the path field always \
             works.",
        );
        ui.add_space(6.0);
    }
    if !cfg!(feature = "encrypted-db") {
        super::notice(
            ui,
            CalloutTone::Neutral,
            "Password protection needs a build with `--features encrypted-db`.",
        );
    }
}
