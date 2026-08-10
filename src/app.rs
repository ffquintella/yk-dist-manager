//! Application state and the egui shell.
//!
//! The app has two states: **locked** (waiting for the database password) and
//! **open**. Once open, every screen reads from the cached vectors refreshed by
//! [`YkDistApp::refresh`] — the GUI never queries SQLite inside a paint pass.

use std::path::PathBuf;

use crate::audit::AuditEntry;
use crate::device::{DeviceInfo, YkmanBackend, YubiKeyBackend};
use crate::domain::{
    BootstrapRun, DeliveryMethod, DistributionRecord, Holder, KeyStatus, StepOutcome, StepStatus,
    YubiKeyRecord,
};
use crate::store::{Location, Store, StoreConfig};
use crate::template::{BootstrapTemplate, PlannedCommand, RenderContext};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Inventory,
    Holders,
    Distribution,
    Bootstrap,
    Audit,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 6] = [
        Tab::Inventory,
        Tab::Holders,
        Tab::Distribution,
        Tab::Bootstrap,
        Tab::Audit,
        Tab::Settings,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Tab::Inventory => "Inventory",
            Tab::Holders => "Holders",
            Tab::Distribution => "Distribution",
            Tab::Bootstrap => "Bootstrap",
            Tab::Audit => "Audit",
            Tab::Settings => "Settings",
        }
    }
}

/// New-holder form.
#[derive(Default)]
pub struct HolderForm {
    pub full_name: String,
    pub email: String,
    pub unit: String,
    pub registration: String,
    pub error: Option<String>,
}

/// Hand-over form.
pub struct DistForm {
    pub key_index: usize,
    pub holder_index: usize,
    pub method: DeliveryMethod,
    pub receipt_ref: String,
    pub notes: String,
    pub link_last_run: bool,
    pub error: Option<String>,
}

impl Default for DistForm {
    fn default() -> Self {
        Self {
            key_index: 0,
            holder_index: 0,
            method: DeliveryMethod::InPerson,
            receipt_ref: String::new(),
            notes: String::new(),
            link_last_run: true,
            error: None,
        }
    }
}

/// Bootstrap wizard state.
#[derive(Default)]
pub struct Wizard {
    pub serial: String,
    pub holder_index: usize,
    pub template_index: usize,
    /// Per-step opt-out, indexed like the selected template's steps.
    pub step_enabled: Vec<bool>,
    pub plan: Vec<PlannedCommand>,
    pub error: Option<String>,
}

pub struct YkDistApp {
    pub config: StoreConfig,
    pub store: Option<Store>,
    pub password_input: String,
    pub open_error: Option<String>,
    pub tab: Tab,
    /// Operator credential recorded on every distribution and audit entry.
    pub operator: String,
    pub org: String,
    pub backend: Box<dyn YubiKeyBackend>,
    pub detected: Vec<DeviceInfo>,
    pub status: String,

    pub keys: Vec<YubiKeyRecord>,
    pub holders: Vec<Holder>,
    pub distributions: Vec<DistributionRecord>,
    pub runs: Vec<BootstrapRun>,
    pub audit_view: Vec<AuditEntry>,
    pub templates: Vec<BootstrapTemplate>,

    pub holder_form: HolderForm,
    pub dist_form: DistForm,
    pub wizard: Wizard,
}

impl YkDistApp {
    pub fn new(path: PathBuf) -> Self {
        let config = StoreConfig::new(path);
        let mut app = Self {
            config,
            store: None,
            password_input: String::new(),
            open_error: None,
            tab: Tab::Inventory,
            operator: default_operator(),
            org: "FGV".into(),
            backend: Box::new(YkmanBackend::default()),
            detected: Vec::new(),
            status: String::new(),
            keys: Vec::new(),
            holders: Vec::new(),
            distributions: Vec::new(),
            runs: Vec::new(),
            audit_view: Vec::new(),
            templates: Vec::new(),
            holder_form: HolderForm::default(),
            dist_form: DistForm::default(),
            wizard: Wizard::default(),
        };
        // Try without a password first: an unencrypted file opens straight away,
        // an encrypted one falls through to the unlock screen.
        app.try_open(None);
        app
    }

    /// Attempt to open the configured database.
    pub fn try_open(&mut self, password: Option<String>) {
        let config = self.config.clone().with_password(password);
        match Store::open(&config) {
            Ok(store) => {
                self.config = config;
                match store.seed_builtin_templates() {
                    Ok(n) if n > 0 => {
                        tracing::info!(event = "template.seeded", count = n as i64);
                    }
                    Ok(_) => {}
                    Err(e) => tracing::error!(event = "template.seed.failed", reason = %e),
                }
                self.status = store.describe();
                self.store = Some(store);
                self.open_error = None;
                self.record("app.opened", "database", "");
                self.refresh();
            }
            Err(e) => {
                tracing::error!(event = "db.open.failed", reason = %e);
                self.open_error = Some(e.to_string());
                self.store = None;
            }
        }
    }

    /// Reload every cached view from the database.
    pub fn refresh(&mut self) {
        let Some(store) = &self.store else { return };
        match (
            store.keys(),
            store.holders(),
            store.distributions(),
            store.runs(),
            store.templates(),
            store.audit_entries(500),
        ) {
            (Ok(keys), Ok(holders), Ok(dists), Ok(runs), Ok(templates), Ok(audit)) => {
                self.keys = keys;
                self.holders = holders;
                self.distributions = dists;
                self.runs = runs;
                self.templates = templates;
                self.audit_view = audit;
            }
            _ => {
                self.status = "could not read the database — see the log".into();
                tracing::error!(event = "db.read.failed");
            }
        }
        if self.templates.is_empty() {
            self.templates = BootstrapTemplate::builtin();
        }
    }

    /// Append an audit entry. Audit coverage is mandatory: every state change
    /// goes through here (see AGENTS.md, "Audit coverage").
    pub fn record(&mut self, event: &str, target: &str, details: &str) {
        let Some(store) = &self.store else { return };
        match store.append_audit(&self.operator, event, target, details) {
            Ok(entry) => tracing::info!(
                event = "audit.appended",
                seq = entry.seq,
                what = entry.event.as_str()
            ),
            Err(e) => {
                // Failing to audit is never silent.
                tracing::error!(event = "audit.append.failed", what = event, reason = %e);
                self.status = format!("AUDIT FAILURE: {e}");
            }
        }
    }

    /// Read the attached key(s) and add or refresh the inventory record.
    pub fn detect_keys(&mut self) {
        match self.backend.info(None) {
            Ok(info) => {
                self.detected = vec![info.clone()];
                let Some(store) = &self.store else { return };
                let existing = store.key_by_serial(info.serial).ok().flatten();
                let record = match existing {
                    Some(mut record) => {
                        record.refresh_from_device(&info);
                        record
                    }
                    None => YubiKeyRecord::from_device(&info),
                };
                let is_new = existing_is_new(store, info.serial);
                if let Err(e) = store.upsert_key(&record) {
                    self.status = format!("could not save the key: {e}");
                    return;
                }
                let event = if is_new { "key.added" } else { "key.refreshed" };
                let details = format!("model={} firmware={}", record.model, record.firmware);
                self.record(event, &format!("serial:{}", record.serial), &details);
                self.status = format!("{} {} read", record.model, record.serial);
                self.wizard.serial = record.serial.to_string();
                self.refresh();
            }
            Err(e) => {
                self.detected.clear();
                self.status = format!("detection failed: {e}");
                tracing::warn!(event = "device.detect.failed", reason = %e);
            }
        }
    }

    /// Selected template, if any.
    pub fn selected_template(&self) -> Option<&BootstrapTemplate> {
        self.templates.get(self.wizard.template_index)
    }

    /// Build the (dry-run) plan for the wizard's current selection.
    pub fn build_plan(&mut self) {
        self.wizard.plan.clear();
        self.wizard.error = None;

        let Some(template) = self.selected_template().cloned() else {
            self.wizard.error = Some("no template selected".into());
            return;
        };
        let Ok(serial) = self.wizard.serial.trim().parse::<u32>() else {
            self.wizard.error = Some("enter or detect a key serial first".into());
            return;
        };
        let Some(holder) = self.holders.get(self.wizard.holder_index).cloned() else {
            self.wizard.error = Some("register a holder first".into());
            return;
        };

        let mut ctx = RenderContext::for_holder(&holder, serial, &self.operator, &self.org);
        if let Some(key) = self.keys.iter().find(|k| k.serial == serial) {
            ctx.key_model = key.model.clone();
        }

        // Honour per-step opt-outs before planning.
        let mut effective = template.clone();
        if self.wizard.step_enabled.len() == effective.steps.len() {
            for (step, enabled) in effective.steps.iter_mut().zip(&self.wizard.step_enabled) {
                step.enabled = *enabled;
            }
        }

        match crate::template::plan(&effective, &ctx) {
            Ok(plan) => {
                self.status = format!("plan built: {} step(s), nothing executed", plan.len());
                self.wizard.plan = plan;
            }
            Err(e) => self.wizard.error = Some(e.to_string()),
        }
    }

    /// Persist the plan as a `Planned` bootstrap run: evidence of intent, with
    /// every step marked `Skipped` because nothing was executed.
    ///
    /// Execution against real hardware lands in Wave 2 of the roadmap and is
    /// deliberately not wired up yet.
    pub fn record_dry_run(&mut self) {
        if self.wizard.plan.is_empty() {
            self.wizard.error = Some("build a plan first".into());
            return;
        }
        let Ok(serial) = self.wizard.serial.trim().parse::<u32>() else {
            self.wizard.error = Some("invalid serial".into());
            return;
        };
        let Some(template) = self.selected_template().cloned() else {
            return;
        };
        let holder_id = self.holders.get(self.wizard.holder_index).map(|h| h.id);

        let steps: Vec<StepOutcome> = self
            .wizard
            .plan
            .iter()
            .map(|cmd| {
                let mut outcome = StepOutcome::planned(
                    cmd.step_id.clone(),
                    cmd.kind,
                    format!("[{}] {}", cmd.transport().label(), cmd.transport_detail()),
                );
                outcome.status = StepStatus::Skipped;
                outcome
            })
            .collect();

        let mut run = BootstrapRun::new(
            serial,
            holder_id,
            template.id.clone(),
            template.version.clone(),
            self.operator.clone(),
            steps,
        );
        // A dry run sets no secret; a real run records the decided model
        // (custody model B) — see features/secrets-custody.md.
        run.custody = crate::domain::CustodyModel::NoSecretSet.note(None);

        let Some(store) = &self.store else { return };
        if let Err(e) = store.insert_run(&run) {
            self.wizard.error = Some(format!("could not save the run: {e}"));
            return;
        }
        let details = format!(
            "template={} version={} steps={}",
            run.template_id,
            run.template_version,
            run.steps.len()
        );
        self.record("bootstrap.dry_run", &format!("serial:{serial}"), &details);
        self.status = format!("dry run recorded for serial {serial}");
        self.refresh();
    }

    /// Register the holder currently in the form.
    pub fn submit_holder(&mut self) {
        self.holder_form.error = None;
        let form = &self.holder_form;
        match Holder::new(&form.full_name, &form.email, &form.unit, &form.registration) {
            Ok(holder) => {
                let Some(store) = &self.store else { return };
                if let Err(e) = store.insert_holder(&holder) {
                    self.holder_form.error = Some(e.to_string());
                    return;
                }
                let display = holder.display();
                self.record("holder.registered", &holder.email.clone(), &display);
                self.holder_form = HolderForm::default();
                self.status = format!("holder registered: {display}");
                self.refresh();
            }
            Err(e) => self.holder_form.error = Some(e.to_string()),
        }
    }

    /// Record a hand-over from the distribution form.
    pub fn submit_distribution(&mut self) {
        self.dist_form.error = None;
        let Some(key) = self.keys.get(self.dist_form.key_index).cloned() else {
            self.dist_form.error = Some("no key selected".into());
            return;
        };
        let Some(holder) = self.holders.get(self.dist_form.holder_index).cloned() else {
            self.dist_form.error = Some("no holder selected".into());
            return;
        };

        let run_id = if self.dist_form.link_last_run {
            self.runs
                .iter()
                .find(|r| r.key_serial == key.serial)
                .map(|r| r.id)
        } else {
            None
        };

        let record = DistributionRecord {
            id: uuid::Uuid::new_v4(),
            key_id: key.id,
            key_serial: key.serial,
            holder_id: holder.id,
            holder_display: holder.display(),
            distributed_at: chrono::Utc::now(),
            distributed_by: self.operator.clone(),
            method: self.dist_form.method,
            receipt_ref: self.dist_form.receipt_ref.trim().to_owned(),
            bootstrap_run_id: run_id,
            returned_at: None,
            returned_to: None,
            notes: self.dist_form.notes.trim().to_owned(),
        };

        let Some(store) = &self.store else { return };
        if let Err(e) = store.insert_distribution(&record) {
            self.dist_form.error = Some(e.to_string());
            return;
        }
        if let Err(e) = store.set_key_status(key.serial, KeyStatus::Distributed) {
            // The hand-over is recorded; the status refusal is surfaced, not hidden.
            self.dist_form.error = Some(format!("recorded, but status not updated: {e}"));
        }
        let details = format!(
            "holder={} method={} receipt={}",
            record.holder_display,
            record.method.label(),
            if record.receipt_ref.is_empty() {
                "(none)"
            } else {
                &record.receipt_ref
            }
        );
        self.record(
            "key.distributed",
            &format!("serial:{}", record.key_serial),
            &details,
        );
        self.status = format!("serial {} distributed", record.key_serial);
        self.dist_form.receipt_ref.clear();
        self.dist_form.notes.clear();
        self.refresh();
    }

    /// Close an open distribution.
    pub fn return_key(&mut self, id: uuid::Uuid, serial: u32) {
        let Some(store) = &self.store else { return };
        if let Err(e) = store.mark_returned(id, &self.operator) {
            self.status = format!("could not record the return: {e}");
            return;
        }
        if let Err(e) = store.set_key_status(serial, KeyStatus::Returned) {
            self.status = format!("returned, but status not updated: {e}");
        }
        self.record("key.returned", &format!("serial:{serial}"), "");
        self.refresh();
    }

    /// Verify the audit chain and report the result in the status bar.
    pub fn verify_audit(&mut self) {
        let Some(store) = &self.store else { return };
        match store.verify_audit() {
            Ok(count) => {
                self.status = format!("audit chain intact — {count} entries verified");
                tracing::info!(event = "audit.verified", entries = count as i64);
            }
            Err(e) => {
                self.status = format!("AUDIT CHAIN BROKEN: {e}");
                tracing::error!(event = "audit.verify.failed", reason = %e);
            }
        }
    }
}

fn existing_is_new(store: &Store, serial: u32) -> bool {
    !matches!(store.key_by_serial(serial), Ok(Some(_)))
}

fn default_operator() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

impl eframe::App for YkDistApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.store.is_none() {
            crate::ui::unlock::show(self, ui);
            return;
        }

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading("YubiKey Distribution Manager");
                ui.separator();
                for tab in Tab::ALL {
                    if ui.selectable_label(self.tab == tab, tab.label()).clicked() {
                        self.tab = tab;
                    }
                }
            });
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("operator: {}", self.operator));
                ui.separator();
                if let Some(store) = &self.store {
                    ui.label(match store.location() {
                        Location::NetworkShare => "db: network share",
                        Location::LocalDisk => "db: local",
                    });
                    ui.separator();
                }
                ui.label(&self.status);
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::both().show(ui, |ui| match self.tab {
                Tab::Inventory => crate::ui::inventory::show(self, ui),
                Tab::Holders => crate::ui::holders::show(self, ui),
                Tab::Distribution => crate::ui::distribution::show(self, ui),
                Tab::Bootstrap => crate::ui::bootstrap::show(self, ui),
                Tab::Audit => crate::ui::audit::show(self, ui),
                Tab::Settings => crate::ui::settings::show(self, ui),
            });
        });
    }
}
