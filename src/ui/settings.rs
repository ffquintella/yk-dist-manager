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
    password_card(app, ui);
    ui.add_space(16.0);
    template_signing(app, ui);
    ui.add_space(16.0);
    signature_tracking(app, ui);
    ui.add_space(16.0);
    transport_card(app, ui);
    ui.add_space(16.0);
    maintenance(app, ui);

    ui.add_space(16.0);
    super::hint(
        ui,
        &format!(
            "yk-dist-manager {} — the database file is the whole deployment; copying it \
             (or a backup) copies everything.",
            crate::build_id()
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

        // Where a lost key is reported (`features/key-lifecycle-and-revocation.md`
        // phase 7). One field, two documents: it is the address the incident note
        // names, and the one the sealed-envelope slip tells the holder to use. Left
        // empty, the note says no address is configured instead of printing
        // something nobody can act on.
        ui.add_space(12.0);
        if super::capped_input(
            ui,
            &mut app.settings.report_incidents_to,
            MAX_TEXT,
            |input| {
                input
                    .label("Report incidents to")
                    .hint(
                        "the security team's address — printed on an incident note and on the \
                           holder's slip",
                    )
                    .id_salt("settings-report-incidents-to")
            },
        )
        .lost_focus()
        {
            identity_changed = true;
        }
        super::hint(
            ui,
            "A possible credential compromise is an incident the norm expects to reach the \
             ESI. This is the address that ends up on the note the tool prepares — who sends \
             it, and by when, is your unit's process.",
        );

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

                // Which share, and as whom. The identity belongs on screen while
                // the register is open: "who am I writing to this file as" is not
                // something an operator should have to remember from the chooser.
                if let Some(share) = app.share.as_ref() {
                    ui.label("Network share");
                    ui.vertical(|ui| {
                        ui.add(elegance::Badge::new(
                            "connected by this application",
                            elegance::BadgeTone::Ok,
                        ));
                        super::mono(ui, &share.target().location());
                        super::faint(ui, &share.describe());
                    });
                    ui.end_row();
                }

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
            // Only offered when there is a connection this session made: a share
            // the operator mounted themselves is not this application's to take
            // down, and the button would be a lie.
            if app.share.is_some()
                && ui
                    .add(Button::new("Close and disconnect the share").outline())
                    .on_hover_text(
                        "close the database, then disconnect from the file server — in that \
                         order",
                    )
                    .clicked()
            {
                app.db_request = Some(DbRequest::DisconnectShare);
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

/// Encryption at rest: set a password, change it, or take it off.
///
/// `features/db-password-and-encryption.md` phases 2, 5 and 6 — the screen that
/// makes them reachable. The operation is one operation
/// ([`crate::store::Store::change_password`]): export the register under the new
/// key, prove the copy opens, then swap. Setting a first password and removing the
/// last one are the two ends of the same thing.
///
/// The card is deliberately reticent. This is the only control in the application
/// that can make the register unopenable, so it does not sit as a bare button
/// between *Integrity check* and *Backup*: it has to be opened, the password is
/// typed twice, the meter grades it, and the sentence about there being no
/// recovery is on screen while the operator types rather than in a manual.
fn password_card(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let Some(store) = app.store.as_ref() else {
        return;
    };
    let encrypted = store.is_encrypted();
    let read_only = store.is_read_only();
    let available = cfg!(feature = "encrypted-db");
    let mut request: Option<DbRequest> = None;
    let mut dismiss = false;

    super::titled_card(ui, "Password protection", |ui| {
        if !available {
            super::notice(
                ui,
                CalloutTone::Neutral,
                "This build cannot encrypt a database: rebuild with `--features encrypted-db`. \
                 The register is a plain SQLite file, so its confidentiality is whatever the \
                 folder or share it sits in provides.",
            );
            return;
        }

        super::hint(
            ui,
            if encrypted {
                "This register is encrypted (SQLCipher). The password is held only while it is \
                 being used to open the file — it is not stored anywhere, and there is no way \
                 to recover it."
            } else {
                "This register is a plain SQLite file. A password makes a copy of it — a backup \
                 on a share, a sync client's conflict copy, a stolen laptop — useless on its \
                 own. It does not protect the register while the application has it open, and \
                 it is not per-operator access control."
            },
        );

        if read_only {
            ui.add_space(10.0);
            super::notice(
                ui,
                CalloutTone::Neutral,
                "This session opened the register for reading only, so it cannot re-key it. \
                 Open it with the single-writer lock first.",
            );
            return;
        }

        ui.add_space(12.0);

        if !app.password_form.open {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(Button::new(if encrypted {
                        "Change the password…"
                    } else {
                        "Set a password…"
                    }))
                    .on_hover_text(
                        "exports the register under the new password and swaps the file, once \
                         the copy has been verified",
                    )
                    .clicked()
                {
                    app.password_form.open = true;
                }
                if encrypted
                    && ui
                        .add(Button::new("Remove the password…").outline())
                        .on_hover_text("leaves a plain, unencrypted file")
                        .clicked()
                {
                    app.password_form.open = true;
                    app.password_form.removing = true;
                }
            });
            if encrypted {
                ui.add_space(8.0);
                super::hint(
                    ui,
                    "Every backup already taken keeps the password it was taken with; a new \
                     password does not reach the old copies.",
                );
            }
            return;
        }

        // The form.
        if let Some(error) = app.password_form.error.clone() {
            super::error_label(ui, &error);
            ui.add_space(10.0);
        }

        if app.password_form.removing {
            super::notice(
                ui,
                CalloutTone::Warning,
                "This exports the register into a plain, unencrypted file and swaps it into \
                 place. From then on anybody who can read the file — on the share, in a backup, \
                 in a sync client's cache — can read the whole register: every serial, every \
                 holder's name, e-mail and unit, and the whole audit trail. The change is \
                 audited, and a backup of the encrypted file is taken first.",
            );
            ui.add_space(14.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(Button::new("Remove the password").accent(Accent::Red))
                    .clicked()
                {
                    request = Some(DbRequest::SetPassword { remove: true });
                }
                if ui.add(Button::new("Keep it").outline()).clicked() {
                    dismiss = true;
                }
            });
            return;
        }

        super::notice(
            ui,
            CalloutTone::Warning,
            "There is no recovery and no administrator: a password nobody can produce is a \
             register nobody can read, and every backup taken from now on needs the new one. \
             Put it where the unit keeps its passwords before pressing the button. A backup of \
             the current file is taken automatically first, and it opens with the password the \
             register has now.",
        );
        ui.add_space(12.0);

        super::form_columns(ui, |left, right, _width| {
            super::capped_input(left, &mut app.password_form.new, MAX_TEXT, |input| {
                input
                    .label("New password")
                    .hint("a passphrase of several unrelated words")
                    .password(true)
                    .id_salt("db-new-password")
            });
            super::capped_input(right, &mut app.password_form.confirm, MAX_TEXT, |input| {
                input
                    .label("Again")
                    .hint("typed twice, because a typo here is not recoverable")
                    .password(true)
                    .id_salt("db-confirm-password")
            });
        });

        ui.add_space(10.0);
        let assessment = super::password_meter(ui, &app.password_form.new);
        let matching = app.password_form.new == app.password_form.confirm;
        if !matching && !app.password_form.confirm.is_empty() {
            ui.add_space(6.0);
            super::error_label(ui, "the two passwords are not the same");
        }

        ui.add_space(14.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(
                    Button::new(if encrypted {
                        "Change the password"
                    } else {
                        "Encrypt this database"
                    })
                    .accent(Accent::Red)
                    .enabled(assessment.is_acceptable() && matching),
                )
                .on_hover_text(
                    "the register is exported, verified and swapped — never re-keyed \
                                in place",
                )
                .clicked()
            {
                request = Some(DbRequest::SetPassword { remove: false });
            }
            if ui.add(Button::new("Cancel").outline()).clicked() {
                dismiss = true;
            }
        });
    });

    // Deferred out of the card closure, like every other mutation on these
    // screens: the request is performed after the paint pass, and it re-opens the
    // register — which cannot happen while the frame that drew it is still being
    // painted.
    if dismiss {
        app.password_form.dismiss();
    }
    if let Some(request) = request {
        app.db_request = Some(request);
    }
}

/// Whether a responsibility term is expected, and how long one may sit unsigned
/// (`features/receipts-and-terms.md` phase 4).
fn signature_tracking(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let mut policy = app.settings.signatures.clone();
    let mut changed = false;

    super::titled_card(ui, "Responsibility terms", |ui| {
        if ui
            .add(elegance::Checkbox::new(
                &mut policy.required,
                "This unit hands over a responsibility term with every key",
            ))
            .changed()
        {
            changed = true;
        }

        if !policy.required {
            ui.add_space(6.0);
            super::hint(
                ui,
                "Off: no hand-over is reported as missing a term, and the Distribution screen                  stops asking. A real case for an internal pilot or a batch of test keys — and                  worth turning back on before the first real hand-over, because the term is where                  the holder acknowledges the obligations the loss procedure depends on.",
            );
            return;
        }

        ui.add_space(10.0);
        let mut days = policy.overdue_after_days as f32;
        if ui
            .add(
                elegance::Slider::new(
                    &mut days,
                    crate::receipt::SignaturePolicy::MIN_DAYS as f32..=60.0,
                )
                .label("Overdue after")
                .suffix(" days"),
            )
            .changed()
        {
            policy.overdue_after_days = days.round() as u32;
            changed = true;
        }

        ui.add_space(6.0);
        super::hint(
            ui,
            "One threshold, not one per delivery method: set it to what the slowest channel this              unit actually uses takes to come back signed. Two thresholds would mean working out              which one applies to the row in front of you, which is how a warning stops being              read.",
        );

        if let Err(refusal) = policy.check() {
            ui.add_space(8.0);
            super::error_label(ui, &refusal);
            changed = false;
        }

        ui.add_space(10.0);
        let tally = app.outstanding_paperwork();
        match tally.describe() {
            Some(line) => super::faint(ui, &format!("Right now: {line}.")),
            None => super::faint(ui, "Right now: nothing outstanding."),
        }
    });

    if changed && policy.check().is_ok() {
        app.settings.signatures = policy;
        app.settings.save_quietly();
        // A threshold that just moved can make terms overdue that were not, and the
        // trail records that once each — so the check runs here rather than waiting
        // for the next time the register is opened.
        app.check_overdue_signatures();
    }
}

/// Which transport reads and writes the hardware
/// (`features/native-device-transport.md` phase 6).
///
/// Automatic is the answer for every normal workstation. The override exists for the
/// case the probe cannot see: PC/SC claimed by another application, or two transports
/// disagreeing about the same key while somebody works out why. It is a diagnostic
/// control, so it says what it is currently using and why — a picker that only showed
/// the *request* would hide a native choice that is silently falling back.
fn transport_card(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let before = app.settings.transport;
    let mut chosen = before;

    super::titled_card(ui, "Device transport", |ui| {
        ui.add(
            Select::new("settings-transport", &mut chosen)
                .label("Read keys through")
                .options(
                    crate::device::Transport::ALL.map(|transport| (transport, transport.label())),
                )
                .width(260.0),
        );

        ui.add_space(8.0);
        super::faint(ui, &format!("Now using: {}.", app.transport.describe()));

        if app.transport.disabled {
            ui.add_space(8.0);
            super::notice(
                ui,
                CalloutTone::Warning,
                "The requested transport is not usable in this session, so the fallback above is \
                 what reads the hardware. Anything a key reports — and anything written to one — \
                 comes through that fallback, not through what is selected here.",
            );
        }

        ui.add_space(8.0);
        super::hint(
            ui,
            "Automatic prefers the native transport when this build has it compiled in and the \
             reader is reachable, and falls back to the ykman command otherwise. Changing this \
             restarts device detection and is recorded in the trail, because which transport \
             wrote to a key is part of that key's story.",
        );
    });

    if chosen != before {
        app.set_transport(chosen);
    }
}

/// Whose signature this deployment accepts on a bootstrap template, and whether
/// one is required (`features/bootstrap-templates.md` phase 5).
///
/// Public keys only. This application verifies signatures and cannot make one —
/// a signing key is a secret, and AGENTS.md §2 forbids it holding one — so what
/// lives here is the answer to "whose approval counts on this workstation", which
/// is a per-deployment question and belongs in the settings file.
fn template_signing(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let mut changed = false;
    let mut remove: Option<usize> = None;

    super::titled_card(ui, "Template signatures", |ui| {
        let mut required = app.settings.templates_must_be_signed;
        if ui
            .add(elegance::Checkbox::new(
                &mut required,
                "Refuse to run a bootstrap from a template whose signature does not verify",
            ))
            .changed()
        {
            app.settings.templates_must_be_signed = required;
            changed = true;
        }

        ui.add_space(6.0);
        if app.settings.templates_must_be_signed && app.settings.template_keys.is_empty() {
            // The one combination that cannot work, said before it is discovered
            // in front of a key: required signatures and nobody to trust means
            // every template is refused, including the ones this build ships.
            super::error_label(
                ui,
                "signatures are required and no trusted key is listed, so no template can be run \
                 at all — the ones shipped with the application are unsigned. Add the key that \
                 signs your procedures, or leave the requirement off until there is one.",
            );
        } else {
            super::hint(
                ui,
                "Off is pilot mode: unsigned templates may be run, the Templates screen says so, \
                 and each such run is recorded as `template.unsigned_used`. On, a run is refused \
                 unless the procedure's signature verifies against one of the keys below.",
            );
        }

        ui.add_space(12.0);
        if app.settings.template_keys.is_empty() {
            super::faint(ui, "No trusted key. Every template reads as unsigned.");
        } else {
            super::table(
                ui,
                "template-keys",
                &["Key id", "Public key", "Whose it is", ""],
                |ui| {
                    for (index, key) in app.settings.template_keys.iter().enumerate() {
                        super::mono(ui, &key.id);
                        // Truncated: 64 hex characters identify nothing to a human,
                        // and the first and last few are what somebody compares
                        // against a printed fingerprint. Counted in characters
                        // rather than sliced by byte, because a hand-edited
                        // settings file can put anything in this field.
                        super::mono(ui, &shortened(&key.public_key))
                            .on_hover_text(key.public_key.clone());
                        super::faint(ui, &key.comment);
                        if super::row_button_danger(ui, "Forget")
                            .on_hover_text(
                                "stop accepting signatures from this key — templates it signed \
                                 will read as signed by an unknown key",
                            )
                            .clicked()
                        {
                            remove = Some(index);
                        }
                        ui.end_row();
                    }
                },
            );
        }

        ui.add_space(12.0);
        add_template_key(app, ui, &mut changed);
    });

    if let Some(index) = remove {
        let key = app.settings.template_keys.remove(index);
        tracing::info!(event = "template.key.forgotten", key = key.id.as_str());
        changed = true;
    }
    if changed {
        app.settings.save_quietly();
    }
}

/// `1a2b3c4d…9f8e7d6c` — enough to recognise a key, not enough to read one out.
fn shortened(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 20 {
        return value.to_owned();
    }
    format!(
        "{}…{}",
        chars[..8].iter().collect::<String>(),
        chars[chars.len() - 8..].iter().collect::<String>()
    )
}

/// The "add a key" form: an id, the public key, and who it belongs to.
fn add_template_key(app: &mut YkDistApp, ui: &mut egui::Ui, changed: &mut bool) {
    let mut add = false;
    super::form_columns(ui, |left, right, _width| {
        super::capped_input(left, &mut app.key_form.id, MAX_TEXT, |input| {
            input
                .label("Key id")
                .hint("the label the signature carries, e.g. esi-templates-2026")
                .id_salt("template-key-id")
        });
        super::capped_input(right, &mut app.key_form.comment, MAX_TEXT, |input| {
            input
                .label("Whose key it is")
                .hint("for the next operator to read")
                .id_salt("template-key-comment")
        });
    });
    super::capped_input(ui, &mut app.key_form.public_key, MAX_TEXT, |input| {
        input
            .label("Public key")
            .hint("64 hex characters — an Ed25519 public key")
            .id_salt("template-key-hex")
    });
    super::hint(
        ui,
        "A public key, never a private one: this application verifies signatures and cannot make \
         them. Whoever signs your procedures holds the private half and never gives it to this \
         tool.",
    );

    if let Some(error) = app.key_form.error.clone() {
        ui.add_space(8.0);
        super::error_label(ui, &error);
    }

    ui.add_space(10.0);
    if ui
        .add(Button::new("Trust this key").accent(Accent::Green))
        .clicked()
    {
        add = true;
    }

    if add {
        app.add_template_key();
        *changed = app.key_form.error.is_none();
    }
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
