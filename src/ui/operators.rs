//! Who may use this register: signing in, and managing the list
//! (`features/operator-auth-and-roles.md` phases 6, 7 and 8).
//!
//! Three screens in one, chosen by what the register is in:
//!
//! * **Unenrolled** — no operators at all. The screen says plainly that the
//!   actor on every audit entry is a workstation label rather than an identity,
//!   and offers the one deliberate act that changes it: creating the first
//!   administrator.
//! * **Signed out** — operators exist. Sign in, or read why you cannot.
//! * **Signed in** — the session, and (for an administrator) the list.
//!
//! Everything here hides what a role may not do. That is a courtesy and not the
//! control: [`crate::store::Store`] refuses every one of these writes on its own,
//! by a SQLite authorizer and by `Store::require`, so a button painted by mistake
//! still cannot change the register.

use elegance::{Accent, Button, CalloutTone, Select};

use crate::app::YkDistApp;
use crate::domain::MAX_TEXT;
use crate::operator::{Role, SessionState};

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::screen_header(
        ui,
        "Operators",
        "Who is using this register, and what their role lets them do.",
    );

    if app.store.is_none() {
        super::notice(
            ui,
            CalloutTone::Neutral,
            "No register is open, so there is nobody to sign in to.",
        );
        return;
    }

    match &app.session {
        SessionState::Unenrolled { .. } => unenrolled(app, ui),
        SessionState::SignedOut => sign_in_card(app, ui),
        SessionState::SignedIn(session) if session.is_locked() => sign_in_card(app, ui),
        SessionState::SignedIn(_) => {
            session_card(app, ui);
            ui.add_space(16.0);
            // A sensitive operation asked for the credential again (phase 5).
            // The prompt lives here and the refusal that opened it can have come
            // from any other screen, which is why `require_reverification`
            // brings the operator to this one: a status line pointing at a
            // screen they have to find is not a prompt.
            if app.sign_in.reverifying.is_some() {
                sign_in_card(app, ui);
                ui.add_space(16.0);
            }
            if app
                .session
                .authority()
                .may(crate::operator::Action::ManageOperators)
            {
                enrol_card(app, ui);
                ui.add_space(16.0);
            }
            list_card(app, ui);
        }
    }
}

/// The declared gap, on the screen rather than only in the compliance document.
fn unenrolled(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::notice(
        ui,
        CalloutTone::Warning,
        "This register has no operators. Every audit entry is signed with this workstation's \
         signed-in user, which is a label and not authentication — the trail is only as strong \
         as physical control of this machine. Nothing is refused, and the register works exactly \
         as it did before.",
    );

    ui.add_space(16.0);
    super::titled_card(ui, "Create the first administrator", |ui| {
        super::hint(
            ui,
            "This switches authorisation on for this register, for everybody who opens it, on \
             every workstation. From then on there is no way back in without an account: the \
             first administrator is the only one who can enrol anybody else, so it must be \
             somebody who will still be here next month. The action is recorded on the audit \
             trail as the moment authorisation began.",
        );
        ui.add_space(12.0);

        super::form_columns(ui, |left, right, _width| {
            super::capped_input(
                left,
                &mut app.operator_panel.first_username,
                MAX_TEXT,
                |input| {
                    input
                        .label("Username")
                        .hint("lower-case, no spaces — this is the actor on every audit entry")
                        .id_salt("first-admin-username")
                },
            );
            super::capped_input(
                right,
                &mut app.operator_panel.first_display_name,
                MAX_TEXT,
                |input| {
                    input
                        .label("Name")
                        .hint("the name a person is called by")
                        .id_salt("first-admin-display")
                },
            );
        });

        ui.add_space(10.0);
        super::form_columns(ui, |left, right, _width| {
            super::capped_input(
                left,
                &mut app.operator_panel.first_password,
                MAX_TEXT,
                |input| {
                    input
                        .label("Password")
                        .password(true)
                        .id_salt("first-admin-password")
                },
            );
            super::capped_input(
                right,
                &mut app.operator_panel.first_password_again,
                MAX_TEXT,
                |input| {
                    input
                        .label("Password again")
                        .password(true)
                        .id_salt("first-admin-password-again")
                },
            );
        });

        ui.add_space(8.0);
        let assessment = super::password_meter(ui, &app.operator_panel.first_password);
        ui.add_space(6.0);
        super::hint(ui, &crate::operator::credential::parameter_summary());

        ui.add_space(6.0);
        super::hint(
            ui,
            "This is not the database password. That one keeps a copy of the file unreadable; \
             this one says who you are. Both are needed, and neither does the other's job.",
        );

        if let Some(error) = &app.operator_panel.error {
            ui.add_space(10.0);
            super::error_label(ui, error);
        }

        ui.add_space(12.0);
        let ready = !app.operator_panel.first_username.trim().is_empty()
            && !app.operator_panel.first_display_name.trim().is_empty()
            && assessment.is_acceptable();
        if ui
            .add(Button::new("Create the first administrator").enabled(ready))
            .on_hover_text("switches authorisation on for this register, permanently")
            .clicked()
        {
            app.enrol_first_administrator();
        }
    });
}

fn sign_in_card(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let locked = app
        .session
        .session()
        .map(|session| session.display_name.clone());
    let reverifying = app.sign_in.reverifying;

    let title = match (&locked, reverifying) {
        (_, Some(_)) => "Confirm it is you",
        (Some(_), None) => "Locked",
        (None, None) => "Sign in",
    };

    super::titled_card(ui, title, |ui| {
        if let Some(action) = reverifying {
            super::notice(
                ui,
                CalloutTone::Warning,
                &format!(
                    "Before you {}: a session records when somebody signed in, not whether they \
                     are still at the workstation. Present your credential again.",
                    action.describe()
                ),
            );
            ui.add_space(12.0);
        } else if let Some(name) = &locked {
            super::hint(
                ui,
                &format!(
                    "{name}'s session is locked because the workstation was idle. Nothing was \
                     lost — signing in again carries on where it stopped."
                ),
            );
            ui.add_space(12.0);
        }

        if reverifying.is_none() && locked.is_none() {
            super::capped_input(ui, &mut app.sign_in.username, MAX_TEXT, |input| {
                input.label("Username").id_salt("sign-in-username")
            });
            ui.add_space(10.0);
        }

        super::capped_input(ui, &mut app.sign_in.password, MAX_TEXT, |input| {
            input
                .label("Password")
                .password(true)
                .id_salt("sign-in-password")
        });

        if let Some(error) = &app.sign_in.error {
            ui.add_space(10.0);
            super::error_label(ui, error);
        }

        ui.add_space(12.0);
        let is_locked = locked.is_some();
        ui.horizontal_wrapped(|ui| {
            if reverifying.is_some() {
                if ui.add(Button::new("Confirm")).clicked() {
                    app.complete_reverification();
                }
                if ui.add(Button::new("Cancel").outline()).clicked() {
                    app.sign_in.reset();
                }
            } else {
                if ui.add(Button::new("Sign in")).clicked() {
                    app.sign_in.username = who_is_signing_in(app, is_locked);
                    app.sign_in_with_password();
                }
                if locked.is_some() && ui.add(Button::new("Sign out instead").outline()).clicked() {
                    app.sign_out("explicit");
                }
            }
        });

        // The preferred method (phase 3): the unit that distributes security keys
        // is the unit most able to hold one.
        //
        // Offered **unconditionally**, and deliberately: a button that appeared
        // only for an account with a key registered would answer "is there an
        // operator called ana, and has she got a key" to anybody at the
        // keyboard, which is the exact question every refusal on this screen is
        // worded to avoid. An account with no key gets the same words as a wrong
        // credential.
        if reverifying.is_none() {
            ui.add_space(10.0);
            super::hint(
                ui,
                "With a security key, the field above is the key's PIN — not a password on this \
                 register. The key has to be attached.",
            );
            ui.add_space(6.0);
            if ui
                .add(Button::new("Sign in with a security key").outline())
                .clicked()
            {
                app.sign_in.username = who_is_signing_in(app, locked.is_some());
                app.sign_in_with_attached_key();
            }
        }

        ui.add_space(10.0);
        super::hint(
            ui,
            "Three wrong attempts lock the account for a minute, five for a quarter of an hour, \
             seven for an hour. An administrator can clear a lockout. Nothing you type here is \
             logged, recorded or kept.",
        );
    });
}

/// Whose sign-in this is.
///
/// A **locked** session already knows the name — re-opening it is the same person
/// coming back to the desk, not a chance to become somebody else — so the typed
/// field is only consulted for a fresh sign-in.
fn who_is_signing_in(app: &YkDistApp, locked: bool) -> String {
    if locked {
        app.session
            .session()
            .map(|session| session.username.clone())
            .unwrap_or_default()
    } else {
        app.sign_in.username.clone()
    }
}

fn session_card(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let Some(session) = app.session.session() else {
        return;
    };
    let describe = session.describe();
    let role = session.role;
    let now = chrono::Utc::now();
    let idle = session.idle_for(now).as_secs() / 60;
    let reverified = session.reverification_remaining_at(now).as_secs();

    super::titled_card(ui, "This session", |ui| {
        super::hint(ui, &describe);
        ui.add_space(6.0);
        super::faint(ui, role.description());
        ui.add_space(10.0);
        super::hint(
            ui,
            &format!(
                "Idle {idle} minute(s). Locks at {}, ends at {}.",
                humanise(crate::operator::session::LOCK_AFTER),
                humanise(crate::operator::session::TIMEOUT_AFTER),
            ),
        );
        if reverified > 0 {
            ui.add_space(6.0);
            super::hint(
                ui,
                &format!(
                    "A sensitive operation can be started for another {reverified} second(s) \
                     without confirming again."
                ),
            );
        }

        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(Button::new("Lock").outline())
                .on_hover_text("leaves everything on screen; signing in again carries on")
                .clicked()
            {
                app.lock_session("explicit");
            }
            if ui.add(Button::new("Sign out").outline()).clicked() {
                app.sign_out("explicit");
            }
        });
    });
}

fn enrol_card(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::titled_card(ui, "Enrol an operator", |ui| {
        super::form_columns(ui, |left, right, _width| {
            super::capped_input(
                left,
                &mut app.operator_panel.new_username,
                MAX_TEXT,
                |input| {
                    input
                        .label("Username")
                        .hint("lower-case, no spaces")
                        .id_salt("enrol-username")
                },
            );
            super::capped_input(
                right,
                &mut app.operator_panel.new_display_name,
                MAX_TEXT,
                |input| input.label("Name").id_salt("enrol-display"),
            );
        });

        ui.add_space(10.0);
        let mut chosen = app.operator_panel.new_role;
        ui.add(
            Select::new("enrol-role", &mut chosen)
                .label("Role")
                .options(Role::ALL.map(|role| (role, role.label())))
                .width(260.0),
        );
        app.operator_panel.new_role = chosen;
        ui.add_space(6.0);
        super::faint(ui, chosen.description());

        ui.add_space(10.0);
        super::form_columns(ui, |left, right, _width| {
            super::capped_input(
                left,
                &mut app.operator_panel.new_password,
                MAX_TEXT,
                |input| {
                    input
                        .label("Password")
                        .password(true)
                        .id_salt("enrol-password")
                },
            );
            super::capped_input(
                right,
                &mut app.operator_panel.new_password_again,
                MAX_TEXT,
                |input| {
                    input
                        .label("Password again")
                        .password(true)
                        .id_salt("enrol-password-again")
                },
            );
        });

        ui.add_space(8.0);
        let assessment = super::password_meter(ui, &app.operator_panel.new_password);

        if let Some(error) = &app.operator_panel.error {
            ui.add_space(10.0);
            super::error_label(ui, error);
        }

        ui.add_space(12.0);
        let ready = !app.operator_panel.new_username.trim().is_empty()
            && !app.operator_panel.new_display_name.trim().is_empty()
            && assessment.is_acceptable();
        if ui.add(Button::new("Enrol").enabled(ready)).clicked() {
            app.enrol_operator();
        }
    });
}

fn list_card(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let operators = app.operator_panel.operators.clone();
    let may_manage = app
        .session
        .authority()
        .may(crate::operator::Action::ManageOperators);
    let mut role_change: Option<(uuid::Uuid, Role)> = None;
    let mut active_change: Option<(uuid::Uuid, bool)> = None;
    let mut unlock: Option<String> = None;
    let mut register: Option<uuid::Uuid> = None;
    let mut confirm_register: Option<(uuid::Uuid, u32)> = None;
    let mut reset: Option<uuid::Uuid> = None;
    let mut confirm_reset: Option<uuid::Uuid> = None;
    let mut remove: Option<uuid::Uuid> = None;
    let mut confirm_remove: Option<uuid::Uuid> = None;
    let mut cancel_panels = false;

    super::titled_card(ui, "Operators on this register", |ui| {
        if operators.is_empty() {
            super::hint(ui, "Nobody yet.");
            return;
        }
        super::table_header(ui, &["Username", "Name", "Role", "State", ""]);
        for operator in &operators {
            ui.horizontal(|ui| {
                super::mono(ui, &operator.username);
                ui.label(&operator.display_name);
                ui.label(operator.role.label());
                if !operator.active {
                    super::faint(ui, "disabled");
                } else if !operator.can_sign_in() {
                    super::faint(ui, "no credential — cannot sign in");
                } else {
                    super::faint(
                        ui,
                        &operator
                            .methods()
                            .iter()
                            .map(|method| method.label())
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                }
                if may_manage {
                    let mut chosen = operator.role;
                    ui.add(
                        Select::new(format!("role-{}", operator.id), &mut chosen)
                            .options(Role::ALL.map(|role| (role, role.label())))
                            .width(150.0),
                    );
                    if chosen != operator.role {
                        role_change = Some((operator.id, chosen));
                    }
                    if super::row_button(ui, if operator.active { "Disable" } else { "Enable" })
                        .clicked()
                    {
                        active_change = Some((operator.id, !operator.active));
                    }
                    if super::row_button(ui, "Clear lockout").clicked() {
                        unlock = Some(operator.username.clone());
                    }
                    let label = if operator.credential.is_some() {
                        "Replace security key"
                    } else {
                        "Register a security key"
                    };
                    if super::row_button(ui, label).clicked() {
                        register = Some(operator.id);
                    }
                    if super::row_button(ui, "Set password").clicked() {
                        reset = Some(operator.id);
                    }
                    if super::row_button(ui, "Remove").clicked() {
                        remove = Some(operator.id);
                    }
                }
            });
        }

        // Registering a key is a two-step act on purpose: the serial says *which*
        // key, and typing it is the same confirmation the factory reset asks for.
        // A credential written to the wrong key is one an operator cannot sign in
        // with and cannot easily find.
        if let Some(id) = app.operator_panel.registering {
            let who = operators
                .iter()
                .find(|operator| operator.id == id)
                .map(|operator| operator.display_name.clone())
                .unwrap_or_default();
            ui.add_space(12.0);
            super::notice(
                ui,
                CalloutTone::Info,
                &format!(
                    "Attach {who}'s own key, then type its serial and its PIN. A resident \
                     credential is created on the key, bound to “{}” as the \
                     relying party, and this register keeps only its public id — the \
                     private key never leaves the key.",
                    app.org
                ),
            );
            ui.add_space(10.0);
            super::capped_input(ui, &mut app.operator_panel.key_serial, MAX_TEXT, |input| {
                input.label("Serial").id_salt("operator-key-serial")
            });
            ui.add_space(10.0);
            super::capped_input(ui, &mut app.operator_panel.key_pin, MAX_TEXT, |input| {
                input
                    .label("The key's PIN")
                    .password(true)
                    .id_salt("operator-key-pin")
            });
            ui.add_space(12.0);
            let serial: Option<u32> = app.operator_panel.key_serial.trim().parse().ok();
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(Button::new("Register it").enabled(serial.is_some()))
                    .clicked()
                    && let Some(serial) = serial
                {
                    confirm_register = Some((id, serial));
                }
                if ui.add(Button::new("Cancel").outline()).clicked() {
                    register = None;
                    confirm_register = None;
                    app.operator_panel.registering = None;
                    app.operator_panel.wipe();
                }
            });
        }

        // Replacing somebody else's password is how an operator who forgot
        // theirs gets back in. It is not a way to read the old one — there is
        // nothing to read — and the audit trail says a credential changed and
        // who changed it, which is the part that has to be true.
        if let Some(id) = app.operator_panel.resetting {
            let who = display_name_of(&operators, id);
            ui.add_space(12.0);
            super::notice(
                ui,
                CalloutTone::Info,
                &format!(
                    "Choose a new password for {who}. Tell them in person rather than by \
                     e-mail, and have them change it: from this moment until they do, an \
                     administrator knows their password. The change is recorded on the audit \
                     trail, and their lockout is lifted with it."
                ),
            );
            ui.add_space(10.0);
            super::form_columns(ui, |left, right, _width| {
                super::capped_input(
                    left,
                    &mut app.operator_panel.reset_password,
                    MAX_TEXT,
                    |i| {
                        i.label("New password")
                            .password(true)
                            .id_salt("reset-password")
                    },
                );
                super::capped_input(
                    right,
                    &mut app.operator_panel.reset_password_again,
                    MAX_TEXT,
                    |i| {
                        i.label("New password again")
                            .password(true)
                            .id_salt("reset-password-again")
                    },
                );
            });
            ui.add_space(8.0);
            let assessment = super::password_meter(ui, &app.operator_panel.reset_password);
            if let Some(error) = &app.operator_panel.error {
                ui.add_space(10.0);
                super::error_label(ui, error);
            }
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(Button::new("Set it").enabled(assessment.is_acceptable()))
                    .clicked()
                {
                    confirm_reset = Some(id);
                }
                if ui.add(Button::new("Cancel").outline()).clicked() {
                    cancel_panels = true;
                }
            });
        }

        // Deleting is the one act on this screen that removes rather than
        // records, so it is previewed and confirmed by name. The store refuses
        // it for anybody with history whatever this panel offers.
        if let Some(id) = app.operator_panel.removing {
            let who = display_name_of(&operators, id);
            ui.add_space(12.0);
            super::notice(
                ui,
                CalloutTone::Warning,
                &format!(
                    "Remove {who} from this register? The account, its password and its \
                     registered security key go, and the username becomes free again. It is \
                     refused for an operator who is the actor on any audit entry — if they \
                     have used this register, disable them instead."
                ),
            );
            if let Some(error) = &app.operator_panel.error {
                ui.add_space(10.0);
                super::error_label(ui, error);
            }
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(Button::new("Remove them").accent(Accent::Red))
                    .clicked()
                {
                    confirm_remove = Some(id);
                }
                if ui.add(Button::new("Cancel").outline()).clicked() {
                    cancel_panels = true;
                }
            });
        }

        ui.add_space(10.0);
        super::hint(
            ui,
            "An operator who has used this register is disabled rather than deleted: deleting \
             one would leave every audit entry they wrote naming somebody the register has \
             never heard of. An account that never wrote anything — a mistyped enrolment, a \
             person who did not join — can be removed outright. A register's last administrator \
             can be neither demoted, disabled nor removed.",
        );
    });

    if let Some((id, role)) = role_change
        && app.require_reverification(crate::operator::Action::ManageOperators)
    {
        app.change_operator_role(id, role);
    }
    if let Some((id, active)) = active_change
        && app.require_reverification(crate::operator::Action::ManageOperators)
    {
        app.set_operator_active(id, active);
    }
    if let Some(username) = unlock
        && app.require_reverification(crate::operator::Action::ManageOperators)
    {
        app.clear_operator_lockout(&username);
    }
    if let Some(id) = register {
        app.operator_panel.registering = Some(id);
        app.operator_panel.key_serial.clear();
        app.operator_panel.wipe();
        app.operator_panel.error = None;
    }
    if let Some((id, serial)) = confirm_register
        && app.require_reverification(crate::operator::Action::ManageOperators)
    {
        app.register_key_for_operator_on_hardware(id, serial);
    }
    if let Some(id) = reset {
        app.operator_panel.resetting = Some(id);
        app.operator_panel.removing = None;
        app.operator_panel.wipe();
        app.operator_panel.error = None;
    }
    if let Some(id) = confirm_reset
        && app.require_reverification(crate::operator::Action::ManageOperators)
    {
        app.set_operator_password(id);
    }
    if let Some(id) = remove {
        app.operator_panel.removing = Some(id);
        app.operator_panel.resetting = None;
        app.operator_panel.wipe();
        app.operator_panel.error = None;
    }
    if let Some(id) = confirm_remove
        && app.require_reverification(crate::operator::Action::ManageOperators)
    {
        app.remove_operator(id);
    }
    if cancel_panels {
        app.operator_panel.resetting = None;
        app.operator_panel.removing = None;
        app.operator_panel.wipe();
        app.operator_panel.error = None;
    }
}

/// The name to put in a sentence about an operator, by id.
fn display_name_of(operators: &[crate::operator::Operator], id: uuid::Uuid) -> String {
    operators
        .iter()
        .find(|operator| operator.id == id)
        .map(|operator| operator.display_name.clone())
        .unwrap_or_default()
}

fn humanise(duration: std::time::Duration) -> String {
    let minutes = duration.as_secs() / 60;
    format!("{minutes} minute(s)")
}
