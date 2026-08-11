//! Application state and the egui shell.
//!
//! The app has two states: **locked** (waiting for the database password) and
//! **open**. Once open, every screen reads from the cached vectors refreshed by
//! [`YkDistApp::refresh`] — the GUI never queries SQLite inside a paint pass.

use std::path::{Path, PathBuf};

use crate::audit::AuditEntry;
use crate::device::{DeviceInfo, YkmanBackend, YubiKeyBackend};
use crate::domain::{
    BootstrapRun, DeliveryMethod, DistributionRecord, Holder, KeyStatus, StepOutcome, StepStatus,
    YubiKeyRecord,
};
use crate::domain::{DocumentKind, SerialSource};
use crate::settings::AppSettings;
use crate::store::{Location, Store, StoreConfig};
use crate::template::{BootstrapTemplate, PlannedCommand, RenderContext};

/// Placeholder organisation, used until the operator sets their own in Settings.
///
/// Deliberately not an institution's name: `{{org}}` goes into certificate
/// subjects and the FIDO2 relying-party id, so the value has to come from the unit
/// that runs the tool, and a placeholder that plainly needs replacing is the way
/// to ask for it.
pub const DEFAULT_ORG: &str = "UNSET-ORGANISATION";

/// A database action requested by a click, performed after the paint pass.
///
/// Native file dialogs are modal and blocking, so they must not run inside a
/// paint closure — same reason table mutations are deferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbRequest {
    /// Open a native dialog to choose an existing file.
    PickExisting,
    /// Open a native save dialog to name a new file.
    PickNew,
    /// Open this path; it must exist.
    Open(PathBuf),
    /// Create this path; it must not exist.
    Create(PathBuf),
    /// Close the current database and show the chooser.
    Close,
    /// Drop a path from the recent list.
    Forget(PathBuf),
    /// Break the abandoned single-writer lock on a cloud-hosted database and
    /// open it. Only ever issued by the operator clicking the button that says so.
    TakeOverLock(PathBuf),
}

/// The database chooser's form state.
#[derive(Default)]
pub struct DatabaseForm {
    pub path: String,
    pub password: String,
    pub error: Option<String>,
    /// Set when an open was refused because another workstation holds the
    /// single-writer lock on a cloud-hosted database.
    ///
    /// Kept as state rather than folded into `error` because it carries an
    /// *action*: the chooser offers to take an abandoned lock over, and only when
    /// the holder has gone quiet long enough to be abandoned.
    pub locked: Option<LockedDatabase>,
}

/// Why an open was refused, and by whom.
pub struct LockedDatabase {
    pub path: PathBuf,
    /// The holder, as the refusal describes it. No secret, and no more personal
    /// data than the audit trail already records: operator, host, pid.
    pub holder: String,
    /// The holder has been silent longer than [`crate::store::cloud::STALE_AFTER`],
    /// so taking over is offered.
    pub stale: bool,
    /// The lock belongs to another run on *this* workstation — usually a second
    /// window of this application. A different instruction from "ask the person
    /// at the other desk".
    pub same_host: bool,
}

/// Consignment-term panel state (distribution screen).
pub struct TermPanel {
    pub open: bool,
    /// Which hand-over the term is for.
    pub distribution: Option<uuid::Uuid>,
    /// Language the operator asked for.
    pub language: String,
    /// The rendered term, awaiting review before it is saved or printed.
    pub rendered: Option<String>,
    /// Language actually used, when it differs from the request.
    pub language_used: Option<String>,
    /// Which template version produced the rendered text, so the operator can see
    /// that an edit has taken effect — and which version a saved term came from.
    pub template_used: Option<String>,
    pub error: Option<String>,
}

impl Default for TermPanel {
    fn default() -> Self {
        Self {
            open: false,
            distribution: None,
            language: crate::term::DEFAULT_LANGUAGE.to_owned(),
            rendered: None,
            language_used: None,
            template_used: None,
            error: None,
        }
    }
}

/// Term-template editor state (Terms screen).
///
/// The buffers are a *draft*: nothing reaches the database until the operator
/// saves, and saving stores a new version rather than overwriting the one that
/// may already have been signed.
pub struct TermEditor {
    /// Which term is being edited. Only `consignment` exists today.
    pub id: String,
    /// Language of the template in the buffers.
    pub language: String,
    /// Version the buffers were loaded from; `None` for a language with nothing
    /// stored yet, which is what makes the save an *addition* rather than an edit.
    pub loaded_version: Option<String>,
    /// False until the buffers have been filled from a template.
    pub loaded: bool,
    pub title: String,
    pub body: String,
    /// Language tag typed into "add a language".
    pub new_language: String,
    /// The draft rendered against the sample context, for review before saving.
    pub preview: Option<String>,
    pub error: Option<String>,
    pub notice: Option<String>,
}

impl Default for TermEditor {
    fn default() -> Self {
        Self {
            id: "consignment".into(),
            language: crate::term::DEFAULT_LANGUAGE.to_owned(),
            loaded_version: None,
            loaded: false,
            title: String::new(),
            body: String::new(),
            new_language: String::new(),
            preview: None,
            error: None,
            notice: None,
        }
    }
}

impl TermEditor {
    /// The draft as a template. The version is left empty: the store assigns it
    /// from what the database already holds.
    pub fn draft(&self) -> crate::term::TermTemplate {
        crate::term::TermTemplate {
            id: self.id.trim().to_owned(),
            language: self.language.trim().to_owned(),
            version: String::new(),
            title: self.title.clone(),
            body: self.body.clone(),
        }
    }

    /// True when the buffers differ from the version they came from.
    pub fn is_dirty(&self, templates: &[crate::term::TermTemplate]) -> bool {
        let stored = crate::term::latest_in_language(templates, &self.id, &self.language);
        crate::term::is_edited(stored, &self.title, &self.body)
    }
}

/// Bootstrap-template editor state (Templates screen).
///
/// Everything here is a *draft*: nothing reaches the database until the operator
/// saves, and saving stores a new version rather than overwriting the one a
/// bootstrap run may already have recorded. The editing rules themselves live on
/// [`crate::template::TemplateDraft`], where they are unit-tested.
#[derive(Default)]
pub struct TemplateEditor {
    pub draft: crate::template::TemplateDraft,
    /// False until the editor has been filled from a template once.
    pub loaded: bool,
    /// Which step has its description and parameters expanded.
    pub open_step: Option<usize>,
    /// Position in [`crate::domain::StepKind::ALL`] for "add a step".
    pub new_kind: usize,
    /// `(id, version)` the operator asked to remove, awaiting confirmation. A
    /// removal is a second, separate decision from the click that asked for it.
    pub pending_removal: Option<(String, String)>,
    pub error: Option<String>,
    pub notice: Option<String>,
}

/// Serial-scanning panel state (inventory screen).
#[derive(Default)]
pub struct ScanPanel {
    pub open: bool,
    /// Serial decoded from a barcode, awaiting the operator's confirmation.
    pub candidate: Option<u32>,
    /// Typed serial, for a USB barcode wedge or manual entry.
    pub typed: String,
    /// Observation to store with the key being recorded — the shipment, the
    /// invoice, the box it came in. Kept for the next serial in the same batch
    /// rather than cleared on every add.
    pub note: String,
    pub error: Option<String>,
    /// Texture handle for the camera preview, recreated as frames arrive.
    #[cfg(feature = "camera")]
    pub preview: Option<egui::TextureHandle>,
    #[cfg(feature = "camera")]
    pub scanner: Option<crate::scan::camera::CameraScanner>,
}

/// Inventory-screen state that outlives a single frame: the observation being
/// edited, and the removal waiting for the operator to confirm it.
///
/// Both are *deferred* by design. A row action cannot mutate `app.keys` while
/// the table that produced it is still being painted, and a removal must not
/// happen on the click that asked for it — the confirmation is a second,
/// separate decision.
#[derive(Default)]
pub struct InventoryPanel {
    /// Serial whose observation is open in the editor.
    pub note_serial: Option<u32>,
    /// The observation being edited. Nothing is stored until it is saved.
    pub note_draft: String,
    /// Serial the operator has asked to remove, awaiting confirmation.
    pub pending_removal: Option<u32>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Inventory,
    Holders,
    Distribution,
    Bootstrap,
    Templates,
    Terms,
    Audit,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 8] = [
        Tab::Inventory,
        Tab::Holders,
        Tab::Distribution,
        Tab::Bootstrap,
        Tab::Templates,
        Tab::Terms,
        Tab::Audit,
        Tab::Settings,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Tab::Inventory => "Inventory",
            Tab::Holders => "Holders",
            Tab::Distribution => "Distribution",
            Tab::Bootstrap => "Bootstrap",
            Tab::Templates => "Templates",
            Tab::Terms => "Terms",
            Tab::Audit => "Audit",
            Tab::Settings => "Settings",
        }
    }

    /// Position in [`Tab::ALL`] — what `elegance::TabBar` selects on.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }

    /// Inverse of [`Tab::index`]. An out-of-range index cannot come from the
    /// tab bar, and falls back to the first screen rather than panicking.
    pub fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or(Tab::Inventory)
    }
}

/// New-holder form.
#[derive(Default)]
pub struct HolderForm {
    pub full_name: String,
    pub email: String,
    pub unit: String,
    pub registration: String,
    /// CPF or the local equivalent; optional.
    pub identification_number: String,
    pub phone: String,
    pub address: String,
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
    pub settings: AppSettings,
    pub db_form: DatabaseForm,
    pub db_request: Option<DbRequest>,
    pub scan: ScanPanel,
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
    /// What the wizard may offer: the newest version of each template in use.
    pub templates: Vec<BootstrapTemplate>,
    /// Every template version on record, retired ones included, with its run
    /// count — what the Templates screen manages.
    pub template_catalogue: Vec<crate::template::StoredTemplate>,

    pub holder_form: HolderForm,
    pub dist_form: DistForm,
    pub inventory: InventoryPanel,
    pub wizard: Wizard,
    pub template_editor: TemplateEditor,
    pub term_panel: TermPanel,
    pub term_editor: TermEditor,
    /// Term templates, refreshed with everything else.
    pub term_templates: Vec<crate::term::TermTemplate>,
    /// How many documents each distribution has.
    pub document_counts: std::collections::BTreeMap<uuid::Uuid, usize>,
}

impl YkDistApp {
    /// `explicit` comes from `$YKDM_DB` and wins over the remembered database.
    pub fn new(explicit: Option<PathBuf>) -> Self {
        let settings = AppSettings::load();

        // Precedence: an explicit path, then the database last used, then the
        // per-user default.
        let (path, must_exist) = match (&explicit, &settings.last_database) {
            (Some(path), _) => (path.clone(), false),
            (None, Some(remembered)) => (remembered.clone(), true),
            (None, None) => (Store::default_path(), false),
        };

        let operator = settings.operator.clone();
        // The organisation is the operator's to state, in Settings — this
        // application is not branded to one. It still needs a value rather than an
        // empty string, because `{{org}}` reaches a certificate subject and a
        // FIDO2 relying-party id, and a blank one there is worse than an obvious
        // placeholder the operator will replace.
        let org = if settings.org.trim().is_empty() {
            DEFAULT_ORG.to_owned()
        } else {
            settings.org.clone()
        };

        let config = StoreConfig::new(path);
        let mut app = Self {
            config,
            store: None,
            settings,
            db_form: DatabaseForm::default(),
            db_request: None,
            scan: ScanPanel::default(),
            open_error: None,
            tab: Tab::Inventory,
            operator,
            org,
            backend: Box::new(YkmanBackend::default()),
            detected: Vec::new(),
            status: String::new(),
            keys: Vec::new(),
            holders: Vec::new(),
            distributions: Vec::new(),
            runs: Vec::new(),
            audit_view: Vec::new(),
            templates: Vec::new(),
            template_catalogue: Vec::new(),
            holder_form: HolderForm::default(),
            dist_form: DistForm::default(),
            inventory: InventoryPanel::default(),
            wizard: Wizard::default(),
            template_editor: TemplateEditor::default(),
            term_panel: TermPanel::default(),
            term_editor: TermEditor::default(),
            term_templates: Vec::new(),
            document_counts: std::collections::BTreeMap::new(),
        };

        app.db_form.path = app.config.path.display().to_string();

        // A remembered database that has gone (an unmounted share) must not be
        // re-created as an empty file — show the chooser instead.
        if must_exist && !app.config.path.is_file() {
            app.open_error = Some(format!(
                "{} is not reachable — is the share mounted?",
                app.config.path.display()
            ));
            return app;
        }

        // Try without a password first: an unencrypted file opens straight away,
        // an encrypted one falls through to the chooser.
        app.try_open(None);
        app
    }

    /// Perform a deferred database action. Called once per frame, outside any
    /// paint closure, because native dialogs are modal and blocking.
    pub fn handle_db_request(&mut self) {
        let Some(request) = self.db_request.take() else {
            return;
        };
        match request {
            DbRequest::PickExisting => self.pick_existing_database(),
            DbRequest::PickNew => self.pick_new_database(),
            DbRequest::Open(path) => {
                let password = self.take_password();
                self.open_database(&path, password);
            }
            DbRequest::Create(path) => {
                let password = self.take_password();
                self.create_database(&path, password);
            }
            DbRequest::Close => self.close_database(),
            DbRequest::Forget(path) => {
                self.settings.forget(&path);
                self.settings.save_quietly();
            }
            DbRequest::TakeOverLock(path) => {
                let password = self.take_password();
                self.take_over_lock(&path, password);
            }
        }
    }

    /// Read and clear the typed password. Never kept beyond the open call.
    fn take_password(&mut self) -> Option<String> {
        let password = self.db_form.password.clone();
        self.db_form.password.clear();
        if password.is_empty() {
            None
        } else {
            Some(password)
        }
    }

    /// Open an existing database, remembering it on success.
    pub fn open_database(&mut self, path: &Path, password: Option<String>) {
        // Whatever is open goes first, through the closing protocol: on a sync
        // folder the lock has to come off with the upload finished, and reopening
        // the database that is already open must not be refused by our own lock.
        self.release_current_database();
        let config = self.store_config(path).with_password(password);
        match Store::open_existing(&config) {
            Ok(store) => {
                self.adopt(store, config);
                self.record(
                    "app.opened",
                    "database",
                    &self.config.path.display().to_string(),
                );
                self.refresh();
            }
            Err(e) => self.report_open_failure(path, e),
        }
    }

    /// Create a new database, refusing to overwrite an existing file.
    pub fn create_database(&mut self, path: &Path, password: Option<String>) {
        self.release_current_database();
        let config = self.store_config(path).with_password(password);
        match Store::create_new(&config) {
            Ok(store) => {
                let display = store.path().display().to_string();
                self.adopt(store, config);
                self.record("db.created", "database", &display);
                self.status = format!("created {display}");
                self.refresh();
            }
            Err(e) => {
                tracing::error!(event = "db.create.failed", path = %path.display(), reason = %e);
                self.db_form.error = Some(e.to_string());
            }
        }
    }

    /// Open a cloud-hosted database whose lock was left behind by a session that
    /// is gone.
    ///
    /// A separate entry point, not a retry flag on [`Self::open_database`],
    /// because it is a different decision: the operator is asserting that the
    /// other workstation is not working in the register. The break is audited as
    /// soon as the database is open.
    pub fn take_over_lock(&mut self, path: &Path, password: Option<String>) {
        self.release_current_database();
        let config = self
            .store_config(path)
            .with_password(password)
            .taking_over_stale_lease();
        match Store::open_existing(&config) {
            Ok(store) => {
                let broken = store
                    .lease()
                    .and_then(|lease| lease.report().took_over.as_ref())
                    .map(|previous| previous.to_string())
                    .unwrap_or_else(|| "no lock was there to take".into());
                self.adopt(store, config);
                self.record("db.lock.taken_over", "database", &broken);
                self.record(
                    "app.opened",
                    "database",
                    &self.config.path.display().to_string(),
                );
                self.refresh();
            }
            Err(e) => self.report_open_failure(path, e),
        }
    }

    /// The open configuration this session uses: the operator's name goes into
    /// the lock file, so a refusal on another workstation names a person.
    fn store_config(&self, path: &Path) -> StoreConfig {
        StoreConfig::new(path).with_operator(&self.operator)
    }

    /// Show a failed open, keeping a lock refusal actionable.
    fn report_open_failure(&mut self, path: &Path, error: crate::store::StoreError) {
        tracing::error!(event = "db.open.failed", path = %path.display(), reason = %error);
        let message = error.to_string();

        // A lock refusal is not the operator's mistake and is not fixed by
        // retyping the path: it names the workstation that has the register, and
        // offers the one action that can help when that workstation is gone.
        self.db_form.locked = match &error {
            crate::store::StoreError::Lease(crate::store::LeaseError::Held { holder, stale }) => {
                Some(LockedDatabase {
                    path: path.to_path_buf(),
                    holder: holder.to_string(),
                    stale: *stale,
                    same_host: holder.is_same_host(),
                })
            }
            _ => None,
        };

        self.db_form.error = Some(message.clone());
        self.open_error = Some(message);
    }

    /// Close the current database and return to the chooser.
    ///
    /// For a cloud-hosted file this is the second half of the lock protocol: the
    /// audit entry is written while the connection is still open, then the
    /// connection closes, the file is given time to finish uploading, and only
    /// then is the lock removed. See [`crate::store::cloud`].
    pub fn close_database(&mut self) {
        let released = self.release_current_database();

        self.keys.clear();
        self.holders.clear();
        self.distributions.clear();
        self.runs.clear();
        self.audit_view.clear();
        self.open_error = None;
        self.db_form.error = None;
        self.db_form.locked = None;
        self.db_form.path = self.config.path.display().to_string();
        self.status = match released {
            Some(settled) => format!("no database open — lock released, {}", settled.describe()),
            None => "no database open".into(),
        };
    }

    /// Audit the close, then hand the store back to the closing protocol.
    ///
    /// Split out because *every* path that stops using a database has to do this,
    /// not just the Close button: switching to another database, creating one, and
    /// taking a lock over all leave a database behind, and on a sync folder the
    /// lock has to come off deliberately — after the upload — rather than whenever
    /// the value happens to be dropped.
    fn release_current_database(&mut self) -> Option<crate::store::Settled> {
        // The audit entry has to be written while the connection is still open,
        // and it is the entry that records the lock being given up: the release
        // itself happens with no database left to write to.
        let held = self
            .store
            .as_ref()
            .and_then(|store| store.lease())
            .map(|lease| {
                format!(
                    "releasing the single-writer lock held by {}",
                    lease.holder()
                )
            });
        if self.store.is_some() {
            self.record("db.closed", "database", held.as_deref().unwrap_or(""));
        }
        self.store.take().and_then(|store| store.close())
    }

    /// Adopt a freshly opened store as the current one.
    fn adopt(&mut self, store: Store, config: StoreConfig) {
        match store.seed_builtin_templates() {
            Ok(n) if n > 0 => tracing::info!(event = "template.seeded", count = n as i64),
            Ok(_) => {}
            Err(e) => tracing::error!(event = "template.seed.failed", reason = %e),
        }
        match store.seed_builtin_terms() {
            Ok(n) if n > 0 => tracing::info!(event = "term.seeded", count = n as i64),
            Ok(_) => {}
            Err(e) => tracing::error!(event = "term.seed.failed", reason = %e),
        }
        self.settings.remember(&config.path);
        self.settings.operator = self.operator.clone();
        self.settings.org = self.org.clone();
        self.settings.save_quietly();

        self.db_form.path = config.path.display().to_string();
        self.db_form.error = None;
        self.db_form.locked = None;
        self.open_error = None;
        self.status = store.describe();
        self.config = config;
        self.store = Some(store);

        // A sync client that could not decide leaves copies behind, and that is
        // the failure this location is dangerous for: two divergent registers.
        // It goes in the audit trail as well as the log, because "the register
        // forked on this date" is exactly what a later reader needs.
        let conflicts: Vec<String> = self
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
            let detail = format!(
                "{} sync conflict copy/copies next to the database: {}",
                conflicts.len(),
                conflicts.join(", ")
            );
            self.record("db.sync.conflict_copies", "database", &detail);
            self.status = format!("WARNING: {detail}");
        }
    }

    /// Keep the single-writer lock fresh, and stop working if it was taken away.
    ///
    /// Called once per frame. Cheap: the lock file is only rewritten every
    /// [`crate::store::cloud::RENEW_EVERY`], and a database that needs no lock
    /// answers immediately.
    pub fn tick_lease(&mut self) {
        let Some(store) = self.store.as_mut() else {
            return;
        };
        match store.renew_lease() {
            Ok(crate::store::Renewal::NotDue | crate::store::Renewal::Renewed) => {}
            // Another workstation has taken the register. Continuing to write
            // would produce exactly the two divergent copies the lock exists to
            // prevent, so the database is closed and the operator is told why.
            Ok(crate::store::Renewal::Lost(holder)) => {
                let holder = holder.to_string();
                tracing::error!(event = "db.lock.lost", holder = %holder);
                self.close_database();
                self.status = format!(
                    "ALARM: the single-writer lock was taken over by {holder} — the database was \
                     closed to avoid two divergent registers. Check with that operator before \
                     reopening it."
                );
            }
            Err(e) => {
                tracing::error!(event = "db.lock.renew.failed", reason = %e);
                self.status = format!("could not refresh the database lock: {e}");
            }
        }
    }

    /// Persist the operator identity and organisation.
    pub fn persist_settings(&mut self) {
        self.settings.operator = self.operator.clone();
        self.settings.org = self.org.clone();
        self.settings.save_quietly();
    }

    #[cfg(feature = "file-dialog")]
    fn pick_existing_database(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Open a distribution database")
            .add_filter("SQLite database", &crate::paths::DATABASE_EXTENSIONS);
        if let Some(parent) = self.config.path.parent() {
            dialog = dialog.set_directory(parent);
        }
        if let Some(path) = dialog.pick_file() {
            let password = self.take_password();
            self.open_database(&path, password);
        }
    }

    #[cfg(not(feature = "file-dialog"))]
    fn pick_existing_database(&mut self) {
        self.db_form.error = Some(
            "this build has no file dialog (`--features file-dialog`) — type the path instead"
                .into(),
        );
    }

    #[cfg(feature = "file-dialog")]
    fn pick_new_database(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Create a distribution database")
            .set_file_name(crate::paths::DEFAULT_DATABASE_NAME)
            .add_filter("SQLite database", &crate::paths::DATABASE_EXTENSIONS);
        if let Some(parent) = self.config.path.parent() {
            dialog = dialog.set_directory(parent);
        }
        if let Some(path) = dialog.save_file() {
            let password = self.take_password();
            self.create_database(&path, password);
        }
    }

    #[cfg(not(feature = "file-dialog"))]
    fn pick_new_database(&mut self) {
        self.db_form.error = Some(
            "this build has no file dialog (`--features file-dialog`) — type the path instead"
                .into(),
        );
    }

    /// Record a key from a serial alone: a scanned label, or a typed number.
    ///
    /// The record is deliberately incomplete — no model, no firmware — and marked
    /// with its provenance, so nothing pretends this key has been seen. `note` is
    /// the operator's observation, which is the only field here that no device can
    /// ever supply.
    pub fn add_serial(&mut self, serial: u32, source: SerialSource, note: &str) {
        let note = match crate::domain::optional_note("notes", note) {
            Ok(note) => note,
            Err(e) => {
                self.scan.error = Some(e.to_string());
                return;
            }
        };
        let Some(store) = &self.store else { return };

        match store.key_by_serial(serial) {
            Ok(Some(existing)) => {
                self.status = format!(
                    "serial {serial} is already in the inventory ({})",
                    existing.serial_source.label()
                );
                return;
            }
            Ok(None) => {}
            Err(e) => {
                self.status = format!("could not check the inventory: {e}");
                return;
            }
        }

        let mut record = YubiKeyRecord::from_serial(serial, source);
        record.notes = note.clone();
        if let Err(e) = store.upsert_key(&record) {
            self.status = format!("could not save the key: {e}");
            return;
        }
        self.record(
            "key.added",
            &format!("serial:{serial}"),
            &format!(
                "source={} verified=false note_chars={}",
                source.audit_name(),
                note.chars().count()
            ),
        );
        self.status = format!(
            "serial {serial} recorded ({}) — plug the key in to verify it",
            source.label()
        );
        self.refresh();
    }

    /// Accept the typed serial from the scan panel (USB wedge or manual entry).
    pub fn accept_typed_serial(&mut self) {
        let typed = self.scan.typed.trim().to_owned();
        self.scan.error = None;
        match crate::scan::parse_serial(&typed) {
            Ok(serial) => {
                self.scan.typed.clear();
                let note = self.scan.note.clone();
                self.add_serial(serial, SerialSource::ManualEntry, &note);
            }
            Err(e) => self.scan.error = Some(e.to_string()),
        }
    }

    /// Accept the serial the camera decoded.
    pub fn accept_scanned_serial(&mut self) {
        if let Some(serial) = self.scan.candidate.take() {
            let note = self.scan.note.clone();
            self.add_serial(serial, SerialSource::ScannedLabel, &note);
            #[cfg(feature = "camera")]
            if let Some(scanner) = &self.scan.scanner {
                scanner.clear_serial();
            }
        }
    }

    // ---------------------------------------------- observation and removal

    /// Open the observation editor for a serial, filled from what is stored.
    pub fn edit_key_note(&mut self, serial: u32) {
        self.inventory.error = None;
        self.inventory.pending_removal = None;
        self.inventory.note_draft = self
            .keys
            .iter()
            .find(|key| key.serial == serial)
            .map(|key| key.notes.clone())
            .unwrap_or_default();
        self.inventory.note_serial = Some(serial);
    }

    /// Close the observation editor, discarding the draft.
    pub fn cancel_key_note(&mut self) {
        self.inventory.note_serial = None;
        self.inventory.note_draft.clear();
        self.inventory.error = None;
    }

    /// Store the observation currently in the editor.
    ///
    /// The audit entry says the observation changed and by how much, never what it
    /// says: an audit entry cannot be corrected, and free text sometimes has to be
    /// (see [`crate::domain::key::note_audit_detail`]).
    pub fn save_key_note(&mut self) {
        let Some(serial) = self.inventory.note_serial else {
            return;
        };
        self.inventory.error = None;

        let note = match crate::domain::optional_note("notes", &self.inventory.note_draft) {
            Ok(note) => note,
            Err(e) => {
                self.inventory.error = Some(e.to_string());
                return;
            }
        };
        let before = self
            .keys
            .iter()
            .find(|key| key.serial == serial)
            .map(|key| key.notes.clone())
            .unwrap_or_default();

        let Some(store) = &self.store else { return };
        if let Err(e) = store.set_key_notes(serial, &note) {
            self.inventory.error = Some(e.to_string());
            tracing::warn!(event = "key.note.save.failed", serial, reason = %e);
            return;
        }
        self.record(
            "key.note_changed",
            &format!("serial:{serial}"),
            &crate::domain::key::note_audit_detail(&before, &note),
        );
        self.status = format!("observation saved for serial {serial}");
        self.cancel_key_note();
        self.refresh();
    }

    /// Ask to remove a serial. Nothing is deleted until [`Self::remove_key`].
    pub fn request_key_removal(&mut self, serial: u32) {
        self.inventory.error = None;
        self.inventory.note_serial = None;
        self.inventory.pending_removal = Some(serial);
    }

    /// Abandon a removal the operator asked about and then declined.
    pub fn cancel_key_removal(&mut self) {
        self.inventory.pending_removal = None;
        self.inventory.error = None;
    }

    /// Remove an inventory row the operator has confirmed.
    ///
    /// For an intake mistake only: the store refuses a serial that any hand-over
    /// or bootstrap run refers to, and the refusal is shown rather than swallowed.
    /// The audit entry outlives the row.
    pub fn remove_key(&mut self, serial: u32) {
        self.inventory.error = None;
        let Some(store) = &self.store else { return };
        match store.delete_key(serial) {
            Ok(removed) => {
                self.record(
                    "key.removed",
                    &format!("serial:{serial}"),
                    &removed.removal_audit_detail(),
                );
                self.status = format!("serial {serial} removed from the inventory");
                tracing::info!(event = "key.removed", serial);
                self.inventory.pending_removal = None;
                self.refresh();
            }
            Err(e) => {
                tracing::warn!(event = "key.remove.refused", serial, reason = %e);
                self.inventory.error = Some(e.to_string());
                self.status = format!("refused: {e}");
            }
        }
    }

    /// What history refers to a serial, for the confirmation warning.
    ///
    /// Reads the cached views rather than the database, so it is safe to call
    /// while the table is being painted.
    pub fn key_history_summary(&self, serial: u32) -> (usize, usize) {
        let distributions = self
            .distributions
            .iter()
            .filter(|record| record.key_serial == serial)
            .count();
        let runs = self
            .runs
            .iter()
            .filter(|run| run.key_serial == serial)
            .count();
        (distributions, runs)
    }

    #[cfg(feature = "camera")]
    pub fn start_camera(&mut self) {
        use crate::scan::camera::CameraScanner;

        self.scan.error = None;
        let decoder = Box::new(crate::scan::RxingDecoder::new());
        match CameraScanner::start(0, decoder) {
            Ok(scanner) => {
                self.status = format!("camera: {}", scanner.describe());
                tracing::info!(event = "camera.started", device = scanner.describe());
                self.scan.scanner = Some(scanner);
            }
            Err(e) => {
                self.scan.error = Some(e.to_string());
                tracing::warn!(event = "camera.start.failed", reason = %e);
            }
        }
    }

    #[cfg(feature = "camera")]
    pub fn stop_camera(&mut self) {
        if self.scan.scanner.take().is_some() {
            tracing::info!(event = "camera.stopped");
        }
        self.scan.preview = None;
    }

    /// Pull the latest frame and any decoded serial from the capture thread.
    #[cfg(feature = "camera")]
    pub fn poll_camera(&mut self, ctx: &egui::Context) {
        let Some(scanner) = &self.scan.scanner else {
            return;
        };
        let snapshot = scanner.snapshot();

        if let Some((width, height, rgb)) = snapshot.preview {
            let expected = width as usize * height as usize * 3;
            if rgb.len() >= expected {
                let image = egui::ColorImage::from_rgb([width as usize, height as usize], &rgb);
                match &mut self.scan.preview {
                    Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
                    None => {
                        self.scan.preview = Some(ctx.load_texture(
                            "camera-preview",
                            image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
            }
        }

        if let Some(serial) = snapshot.serial {
            self.scan.candidate = Some(serial);
        }
        if let Some(error) = snapshot.last_error {
            self.scan.error = Some(error);
        }
        // Keep frames flowing while the panel is open.
        ctx.request_repaint_after(std::time::Duration::from_millis(60));
    }

    /// Open the configured database, creating it if the file is not there.
    ///
    /// Used for the default path at first launch. An operator-chosen path goes
    /// through [`Self::open_database`] or [`Self::create_database`], which refuse
    /// to create-by-accident or open-by-accident.
    pub fn try_open(&mut self, password: Option<String>) {
        // Nothing is open on the paths that reach here — first launch, and the
        // unlock screen after a refused password — but the release is what makes
        // that true rather than assumed: opening while a lock of ours is still
        // held would be refused by our own lock file.
        self.release_current_database();
        let config = self.config.clone().with_password(password);
        match Store::open(&config) {
            Ok(store) => {
                self.adopt(store, config);
                self.record("app.opened", "database", "");
                self.refresh();
            }
            Err(e) => {
                tracing::error!(event = "db.open.failed", reason = %e);
                self.open_error = Some(e.to_string());
                self.db_form.error = Some(e.to_string());
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
                // The wizard offers the newest version of each template in use;
                // the older versions stay on record for the runs that applied
                // them (see `template::latest_per_id`).
                self.templates = crate::template::latest_per_id(&templates);
                self.audit_view = audit;
            }
            _ => {
                self.status = "could not read the database — see the log".into();
                tracing::error!(event = "db.read.failed");
            }
        }
        match store.template_catalogue() {
            Ok(catalogue) => self.template_catalogue = catalogue,
            Err(e) => tracing::error!(event = "template.read.failed", reason = %e),
        }
        // Only a database with no template at all falls back to the built-ins:
        // an operator who has retired every template has said something
        // deliberate, and the wizard must not quietly re-offer them.
        if self.template_catalogue.is_empty() {
            self.templates = BootstrapTemplate::builtin();
        }

        match store.term_templates() {
            Ok(terms) if !terms.is_empty() => self.term_templates = terms,
            Ok(_) => self.term_templates = crate::term::TermTemplate::builtin(),
            Err(e) => {
                tracing::error!(event = "term.read.failed", reason = %e);
                self.term_templates = crate::term::TermTemplate::builtin();
            }
        }
        match store.document_counts() {
            Ok(counts) => self.document_counts = counts,
            Err(e) => tracing::error!(event = "document.count.failed", reason = %e),
        }
    }

    // ------------------------------------------ editing a bootstrap template

    /// The exact version the editor's draft was loaded from, or `None` for a
    /// template that has never been stored.
    ///
    /// Deliberately the *loaded* version and not the newest: if another
    /// workstation stores version 3 while this one has version 2 open, an
    /// untouched draft must not start claiming unsaved changes.
    pub fn template_baseline(&self) -> Option<&BootstrapTemplate> {
        let version = self.template_editor.draft.loaded_version.as_deref()?;
        let id = self.template_editor.draft.id.trim();
        self.template_catalogue
            .iter()
            .find(|stored| stored.template.id == id && stored.template.version == version)
            .map(|stored| &stored.template)
    }

    /// True when the draft differs from the version it came from.
    pub fn template_editor_dirty(&self) -> bool {
        self.template_editor
            .draft
            .is_dirty(self.template_baseline())
    }

    /// The newest version on record of a template id, retired ones included.
    pub fn latest_stored_template(&self, id: &str) -> Option<&BootstrapTemplate> {
        self.template_catalogue
            .iter()
            .map(|stored| &stored.template)
            .filter(|template| template.id == id)
            .max_by(|a, b| {
                crate::versioning::version_order(&a.version)
                    .cmp(&crate::versioning::version_order(&b.version))
            })
    }

    /// Open a template in the editor. `version` picks one exactly; `None` opens
    /// the newest on record.
    ///
    /// Reads the cached catalogue rather than the database, so it is safe to call
    /// from a click inside a painted table.
    pub fn load_template(&mut self, id: &str, version: Option<&str>) {
        let found = match version {
            Some(version) => self
                .template_catalogue
                .iter()
                .find(|stored| stored.template.id == id && stored.template.version == version)
                .map(|stored| stored.template.clone()),
            None => self.latest_stored_template(id).cloned(),
        };
        let Some(template) = found else {
            self.template_editor.error = Some(format!("no template `{id}` on record"));
            return;
        };

        self.template_editor.draft = crate::template::TemplateDraft::from_template(&template, true);
        self.template_editor.loaded = true;
        self.template_editor.open_step = None;
        self.template_editor.pending_removal = None;
        self.template_editor.error = None;
        self.template_editor.notice = None;
    }

    /// Start a template from nothing.
    ///
    /// Refuses to discard an unsaved edit, the same way the Terms screen does:
    /// a procedure somebody has just typed is not something to lose to a click.
    pub fn start_template(&mut self) {
        self.template_editor.error = None;
        self.template_editor.notice = None;
        if self.template_editor_dirty() {
            self.template_editor.error = Some(unsaved_template_message(
                &self.template_editor.draft.id,
                "starting a new one",
            ));
            return;
        }
        self.template_editor.draft = crate::template::TemplateDraft::blank();
        self.template_editor.loaded = true;
        self.template_editor.open_step = None;
        self.template_editor.notice = Some(
            "new template — give it an id, a name and its steps; nothing is stored until you \
             save"
                .into(),
        );
    }

    /// Copy a template version into the editor under a fresh id.
    ///
    /// This is how a variant is made — the FIDO-only procedure for a contractor
    /// is the standard one minus PIV — so the copy keeps the steps and takes an
    /// id nothing else is using.
    pub fn duplicate_template(&mut self, id: &str, version: &str) {
        self.template_editor.error = None;
        self.template_editor.notice = None;
        if self.template_editor_dirty() {
            self.template_editor.error = Some(unsaved_template_message(
                &self.template_editor.draft.id,
                "duplicating another",
            ));
            return;
        }
        let Some(source) = self
            .template_catalogue
            .iter()
            .find(|stored| stored.template.id == id && stored.template.version == version)
            .map(|stored| stored.template.clone())
        else {
            self.template_editor.error = Some(format!("no template `{id}` version {version}"));
            return;
        };

        let taken: Vec<String> = self
            .template_catalogue
            .iter()
            .map(|stored| stored.template.id.clone())
            .collect();
        let new_id = crate::template::unique_id(&taken, &format!("{id}-copy"));
        let copy = source.duplicated_as(&new_id, &format!("{} (copy)", source.name));

        self.template_editor.draft = crate::template::TemplateDraft::from_template(&copy, false);
        self.template_editor.loaded = true;
        self.template_editor.open_step = None;
        self.template_editor.notice = Some(format!(
            "copied {id} version {version} as `{new_id}` — nothing is stored until you save"
        ));
    }

    /// Put this build's steps for the draft's id back in the editor.
    pub fn restore_builtin_template(&mut self) {
        let id = self.template_editor.draft.id.trim().to_owned();
        self.template_editor.error = None;
        match BootstrapTemplate::builtin_for(&id) {
            Some(builtin) => {
                let loaded = self.template_editor.draft.loaded_version.clone();
                self.template_editor.draft =
                    crate::template::TemplateDraft::from_template(&builtin, false);
                self.template_editor.draft.loaded_version = loaded;
                self.template_editor.open_step = None;
                self.template_editor.notice = Some(
                    "the built-in steps are in the editor — they are not stored until you save"
                        .into(),
                );
            }
            None => {
                self.template_editor.error =
                    Some(format!("this build ships no template called `{id}`"));
            }
        }
    }

    /// Append a step of the kind selected in the editor.
    pub fn add_template_step(&mut self) {
        self.template_editor.error = None;
        let kind = crate::domain::StepKind::ALL
            .get(self.template_editor.new_kind)
            .copied()
            .unwrap_or(crate::domain::StepKind::Verify);
        match self.template_editor.draft.add_step(kind) {
            Ok(()) => {
                self.template_editor.open_step = Some(self.template_editor.draft.steps.len() - 1);
                self.template_editor.notice = Some(format!(
                    "{} added at the end — move it to where it belongs in the procedure",
                    kind.label()
                ));
            }
            Err(e) => self.template_editor.error = Some(e.to_string()),
        }
    }

    /// Store the draft as a **new version** of its template.
    ///
    /// The version on record is left untouched, because a bootstrap run may
    /// reference it; the newly stored version is what the wizard offers next.
    pub fn save_template(&mut self) {
        self.template_editor.error = None;
        self.template_editor.notice = None;

        let draft = match self.template_editor.draft.to_template() {
            Ok(draft) => draft,
            Err(e) => {
                self.template_editor.error = Some(e.to_string());
                return;
            }
        };
        let previous = self.template_editor.draft.loaded_version.clone();
        let result = {
            let Some(store) = &self.store else {
                self.template_editor.error = Some("no database open".into());
                return;
            };
            store.save_template_version(&draft)
        };

        match result {
            Ok(stored) => {
                let (event, target, details) =
                    crate::template::edit_audit_entry(&stored, previous.as_deref());
                self.record(event, &target, &details);
                self.status = format!(
                    "{} saved as version {} ({} step(s))",
                    stored.id,
                    stored.version,
                    stored.steps.len()
                );
                self.template_editor.notice = Some(match &previous {
                    Some(previous) => format!(
                        "saved as version {} — new runs use it, and version {previous} stays on \
                         record for the runs already made against it",
                        stored.version
                    ),
                    None => format!("`{}` added as version {}", stored.id, stored.version),
                });
                self.template_editor.draft.loaded_version = Some(stored.version);
                self.refresh();
            }
            Err(e) => {
                tracing::warn!(event = "template.save.refused", reason = %e);
                self.template_editor.error = Some(e.to_string());
            }
        }
    }

    /// Withdraw a template version from the wizard, keeping it on record.
    pub fn retire_template(&mut self, id: &str, version: &str) {
        self.template_editor.error = None;
        let Some(store) = &self.store else { return };
        match store.retire_template(id, version) {
            Ok(stored) => {
                self.record(
                    "template.retired",
                    &format!("template:{id}"),
                    &stored.audit_detail(),
                );
                self.status = format!("{id} version {version} retired — no longer offered");
                self.refresh();
            }
            Err(e) => {
                tracing::warn!(event = "template.retire.failed", template = id, reason = %e);
                self.template_editor.error = Some(e.to_string());
            }
        }
    }

    /// Put a retired template version back in use.
    pub fn reinstate_template(&mut self, id: &str, version: &str) {
        self.template_editor.error = None;
        let Some(store) = &self.store else { return };
        match store.reinstate_template(id, version) {
            Ok(stored) => {
                self.record(
                    "template.reinstated",
                    &format!("template:{id}"),
                    &stored.audit_detail(),
                );
                self.status = format!("{id} version {version} is in use again");
                self.refresh();
            }
            Err(e) => {
                tracing::warn!(event = "template.reinstate.failed", template = id, reason = %e);
                self.template_editor.error = Some(e.to_string());
            }
        }
    }

    /// Ask to remove a template version. Nothing is deleted until
    /// [`Self::remove_template`].
    pub fn request_template_removal(&mut self, id: &str, version: &str) {
        self.template_editor.error = None;
        self.template_editor.pending_removal = Some((id.to_owned(), version.to_owned()));
    }

    /// Abandon a removal the operator asked about and then declined.
    pub fn cancel_template_removal(&mut self) {
        self.template_editor.pending_removal = None;
        self.template_editor.error = None;
    }

    /// Delete a template version the operator has confirmed.
    ///
    /// For a procedure typed by mistake only: the store refuses a version any
    /// bootstrap run recorded, and refuses one this build ships (it would come
    /// back on the next open). Both refusals name retirement and are shown rather
    /// than swallowed. The audit entry outlives the row.
    pub fn remove_template(&mut self, id: &str, version: &str) {
        self.template_editor.error = None;
        let Some(store) = &self.store else { return };
        match store.delete_template(id, version) {
            Ok(stored) => {
                self.record(
                    "template.removed",
                    &format!("template:{id}"),
                    &stored.audit_detail(),
                );
                self.status = format!("{id} version {version} removed");
                tracing::info!(event = "template.removed", template = id, version);
                self.template_editor.pending_removal = None;
                // The editor may have been showing exactly what was just deleted.
                if self.template_editor.draft.id.trim() == id
                    && self.template_editor.draft.loaded_version.as_deref() == Some(version)
                {
                    self.template_editor.draft.loaded_version = None;
                }
                self.refresh();
            }
            Err(e) => {
                tracing::warn!(event = "template.remove.refused", template = id, reason = %e);
                self.template_editor.error = Some(e.to_string());
                self.status = format!("refused: {e}");
            }
        }
    }

    // ------------------------------------------------------ consignment terms

    /// Render the consignment term for a hand-over, in the requested language.
    pub fn generate_term(&mut self, distribution_id: uuid::Uuid) {
        use crate::term::{TermContext, choose_template, render_term};

        self.term_panel.open = true;
        self.term_panel.distribution = Some(distribution_id);
        self.term_panel.rendered = None;
        self.term_panel.language_used = None;
        self.term_panel.template_used = None;
        self.term_panel.error = None;

        let Some(record) = self
            .distributions
            .iter()
            .find(|d| d.id == distribution_id)
            .cloned()
        else {
            self.term_panel.error = Some("hand-over not found".into());
            return;
        };
        let Some(holder) = self
            .holders
            .iter()
            .find(|h| h.id == record.holder_id)
            .cloned()
        else {
            self.term_panel.error =
                Some("the holder of this hand-over is no longer in the register".into());
            return;
        };
        let key = self
            .keys
            .iter()
            .find(|k| k.serial == record.key_serial)
            .cloned()
            .unwrap_or_else(|| {
                YubiKeyRecord::from_serial(record.key_serial, SerialSource::ManualEntry)
            });

        let run = record
            .bootstrap_run_id
            .and_then(|id| self.runs.iter().find(|r| r.id == id));
        let applied = run
            .map(|r| r.summary())
            .unwrap_or_else(|| "nothing recorded".to_owned());
        let custody = run
            .and_then(|r| crate::domain::CustodyModel::parse(&r.custody))
            .unwrap_or(crate::domain::CustodyModel::DEFAULT)
            .label()
            .to_owned();

        let ctx = TermContext::from_records(
            &holder,
            &key,
            Some(&record),
            &applied,
            &custody,
            &self.operator,
            &self.org,
        );

        let language = self.term_panel.language.clone();
        let Some(template) = choose_template(&self.term_templates, "consignment", &language) else {
            self.term_panel.error = Some(format!("no consignment term template for `{language}`"));
            return;
        };

        if !template.language.eq_ignore_ascii_case(&language) {
            self.term_panel.language_used = Some(template.language.clone());
        }
        self.term_panel.template_used = Some(format!(
            "{}@{} ({})",
            template.id, template.version, template.language
        ));

        match render_term(template, &ctx) {
            Ok(text) => {
                let details = format!(
                    "holder={} language={} template={}@{}",
                    holder.email, template.language, template.id, template.version
                );
                self.term_panel.rendered = Some(text);
                self.record(
                    "term.generated",
                    &format!("serial:{}", record.key_serial),
                    &details,
                );
            }
            Err(e) => self.term_panel.error = Some(e.to_string()),
        }
    }

    // ------------------------------------------------- editing a term template

    /// Fill the editor buffers from the newest stored version of a language.
    ///
    /// Reads the cached templates, never the database, so it is safe to call from
    /// a click. A language with nothing stored opens on the built-in wording when
    /// the build ships one, and blank otherwise.
    pub fn load_term_template(&mut self, language: &str) {
        use crate::term::{TermTemplate, latest_in_language};

        let id = self.term_editor.id.clone();
        let stored = latest_in_language(&self.term_templates, &id, language).cloned();
        let (template, loaded_version) = match stored {
            Some(template) => {
                let version = template.version.clone();
                (template, Some(version))
            }
            None => (
                TermTemplate::builtin_for(&id, language)
                    .unwrap_or_else(|| TermTemplate::blank(&id, language)),
                None,
            ),
        };

        self.term_editor.language = template.language.clone();
        self.term_editor.title = template.title;
        self.term_editor.body = template.body;
        self.term_editor.loaded_version = loaded_version;
        self.term_editor.loaded = true;
        self.term_editor.preview = None;
        self.term_editor.error = None;
        self.term_editor.notice = None;
    }

    /// Start a term in the language typed into the editor.
    ///
    /// Refuses a language that is already on record — that is an edit, and the
    /// operator reaches it by selecting the language instead — and refuses to
    /// discard an unsaved edit to the language currently open.
    pub fn start_term_language(&mut self) {
        let language = self.term_editor.new_language.trim().to_owned();
        self.term_editor.error = None;
        if language.is_empty() {
            self.term_editor.error = Some("type a language tag, e.g. `es` or `fr-FR`".into());
            return;
        }
        if language.chars().count() > crate::domain::MAX_TEXT {
            self.term_editor.error = Some("that is not a language tag".into());
            return;
        }
        if self.term_editor.is_dirty(&self.term_templates) {
            self.term_editor.error = Some(format!(
                "there are unsaved changes to `{}` — save them, or discard them with \
                 “Reload stored version”, before starting `{language}`",
                self.term_editor.language
            ));
            return;
        }
        let id = self.term_editor.id.clone();
        if crate::term::latest_in_language(&self.term_templates, &id, &language).is_some() {
            self.term_editor.error = Some(format!(
                "`{language}` is already on record — select it to edit it"
            ));
            return;
        }
        self.term_editor.new_language.clear();
        self.load_term_template(&language);
        self.term_editor.notice = Some(format!(
            "new language `{language}` — nothing is stored until you save"
        ));
    }

    /// Replace the buffers with the wording this build ships for the language.
    pub fn restore_builtin_term_template(&mut self) {
        let id = self.term_editor.id.clone();
        let language = self.term_editor.language.clone();
        self.term_editor.error = None;
        match crate::term::TermTemplate::builtin_for(&id, &language) {
            Some(builtin) => {
                self.term_editor.title = builtin.title;
                self.term_editor.body = builtin.body;
                self.term_editor.preview = None;
                self.term_editor.notice = Some(
                    "the built-in wording is in the editor — it is not stored until you save"
                        .into(),
                );
            }
            None => {
                self.term_editor.error = Some(format!(
                    "this build ships no `{id}` template in `{language}`"
                ));
            }
        }
    }

    /// Render the draft against the sample context, so the wording can be read
    /// as a document before it is stored.
    pub fn preview_term_template(&mut self) {
        use crate::term::{TermContext, render_term};

        self.term_editor.error = None;
        self.term_editor.notice = None;
        let draft = self.term_editor.draft();
        if let Err(e) = draft.check() {
            self.term_editor.preview = None;
            self.term_editor.error = Some(e.to_string());
            return;
        }
        match render_term(&draft, &TermContext::sample()) {
            Ok(text) => self.term_editor.preview = Some(text),
            Err(e) => {
                self.term_editor.preview = None;
                self.term_editor.error = Some(e.to_string());
            }
        }
    }

    /// Store the draft as a **new version** of the term.
    ///
    /// The version already on record is left untouched, because it may be the one
    /// a holder signed; the newly stored version is what the next generated term
    /// uses (`term::choose_template` takes the newest).
    pub fn save_term_template(&mut self) {
        self.term_editor.error = None;
        self.term_editor.notice = None;

        let draft = self.term_editor.draft();
        let previous = self.term_editor.loaded_version.clone();
        let result = {
            let Some(store) = &self.store else {
                self.term_editor.error = Some("no database open".into());
                return;
            };
            store.save_term_template_version(&draft)
        };

        match result {
            Ok(stored) => {
                let (event, target, details) =
                    crate::term::edit_audit_entry(&stored, previous.as_deref());
                self.record(event, &target, &details);
                self.status = format!(
                    "{} ({}) saved as version {}",
                    stored.id, stored.language, stored.version
                );
                self.term_editor.notice = Some(match &previous {
                    Some(previous) => format!(
                        "saved as version {} — new terms use it, and version {previous} stays on \
                         record for the terms already signed against it",
                        stored.version
                    ),
                    None => format!("`{}` added as version {}", stored.language, stored.version),
                });
                self.term_editor.loaded_version = Some(stored.version);
                self.term_editor.preview = None;
                self.refresh();
            }
            Err(e) => {
                tracing::warn!(event = "term.template.save.refused", reason = %e);
                self.term_editor.error = Some(e.to_string());
            }
        }
    }

    /// Write the rendered term to a file the operator chooses.
    pub fn save_term(&mut self) {
        let Some(text) = self.term_panel.rendered.clone() else {
            return;
        };
        let serial = self
            .term_panel
            .distribution
            .and_then(|id| self.distributions.iter().find(|d| d.id == id))
            .map(|d| d.key_serial)
            .unwrap_or_default();
        let suggested = format!("termo-{serial}.txt");

        if let Some(path) = self.save_bytes(&suggested, text.as_bytes()) {
            let display = path.display().to_string();
            self.record("term.saved", &format!("serial:{serial}"), &display);
            self.status = format!("term written to {display}");
        }
    }

    /// Attach a signed term (or any accepted document) to a hand-over.
    pub fn attach_document(&mut self, distribution_id: uuid::Uuid, kind: DocumentKind) {
        let Some((filename, content)) = self.read_chosen_file() else {
            return;
        };

        let document = match crate::domain::AttachedDocument::new(
            distribution_id,
            kind,
            &filename,
            content,
            &self.operator,
        ) {
            Ok(document) => document,
            Err(e) => {
                self.term_panel.error = Some(e.to_string());
                self.status = format!("upload refused: {e}");
                return;
            }
        };

        let Some(store) = &self.store else { return };
        if let Err(e) = store.insert_document(&document) {
            self.status = format!("could not file the document: {e}");
            return;
        }

        let serial = self
            .distributions
            .iter()
            .find(|d| d.id == distribution_id)
            .map(|d| d.key_serial)
            .unwrap_or_default();
        let details = format!(
            "kind={} file={} bytes={} sha256={}",
            crate::store::document_kind_str(document.kind),
            document.filename,
            document.size_bytes,
            document.sha256
        );
        self.record(
            "term.signed_uploaded",
            &format!("serial:{serial}"),
            &details,
        );
        self.status = format!(
            "{} filed ({}, {})",
            document.kind.label(),
            document.size_label(),
            document.short_digest()
        );
        self.refresh();
    }

    /// Write a filed document back out to disk.
    pub fn export_document(&mut self, id: uuid::Uuid) {
        let Some(store) = &self.store else { return };
        let document = match store.document_content(id) {
            Ok(document) => document,
            Err(e) => {
                self.status = format!("could not read the document: {e}");
                return;
            }
        };
        if document.verify() == Some(false) {
            self.status = format!(
                "REFUSED: {} does not match the digest recorded when it was filed",
                document.filename
            );
            tracing::error!(event = "document.digest.mismatch", document = %id);
            return;
        }
        let Some(content) = document.content.clone() else {
            return;
        };
        let filename = document.filename.clone();
        let digest = document.sha256.clone();
        if let Some(path) = self.save_bytes(&filename, &content) {
            let display = path.display().to_string();
            self.record("document.exported", &digest, &display);
            self.status = format!("written to {display}");
        }
    }

    // ------------------------------------------------------------ file access

    #[cfg(feature = "file-dialog")]
    fn read_chosen_file(&mut self) -> Option<(String, Vec<u8>)> {
        let path = rfd::FileDialog::new()
            .set_title("Choose the signed term")
            .add_filter(
                "Scanned document",
                &["pdf", "png", "jpg", "jpeg", "tif", "tiff"],
            )
            .pick_file()?;
        match std::fs::read(&path) {
            Ok(content) => Some((
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "document".into()),
                content,
            )),
            Err(e) => {
                self.status = format!("could not read {}: {e}", path.display());
                None
            }
        }
    }

    #[cfg(not(feature = "file-dialog"))]
    fn read_chosen_file(&mut self) -> Option<(String, Vec<u8>)> {
        self.status = "uploading a document needs a build with `--features file-dialog`".into();
        None
    }

    #[cfg(feature = "file-dialog")]
    fn save_bytes(&mut self, suggested: &str, content: &[u8]) -> Option<PathBuf> {
        let path = rfd::FileDialog::new()
            .set_title("Save")
            .set_file_name(suggested)
            .save_file()?;
        match std::fs::write(&path, content) {
            Ok(()) => Some(path),
            Err(e) => {
                self.status = format!("could not write {}: {e}", path.display());
                None
            }
        }
    }

    #[cfg(not(feature = "file-dialog"))]
    fn save_bytes(&mut self, suggested: &str, content: &[u8]) -> Option<PathBuf> {
        // Without a dialog, fall back to writing next to the database — a location
        // the operator already knows.
        let path = self
            .config
            .path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(suggested);
        match std::fs::write(&path, content) {
            Ok(()) => Some(path),
            Err(e) => {
                self.status = format!("could not write {}: {e}", path.display());
                None
            }
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
        match Holder::new(&form.full_name, &form.email, &form.unit, &form.registration).and_then(
            |holder| holder.with_optional(&form.identification_number, &form.phone, &form.address),
        ) {
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

/// The refusal shown when a click would discard an unsaved template edit.
fn unsaved_template_message(id: &str, action: &str) -> String {
    let name = if id.trim().is_empty() {
        "the new template".to_owned()
    } else {
        format!("`{}`", id.trim())
    };
    format!(
        "there are unsaved changes to {name} — save them, or discard them with “Reload stored \
         version”, before {action}"
    )
}

fn existing_is_new(store: &Store, serial: u32) -> bool {
    !matches!(store.key_by_serial(serial), Ok(Some(_)))
}

/// Quitting the application closes the database properly.
///
/// Most operators will never press *Switch database…* — they will close the window,
/// and on a cloud-hosted register that has to run the same protocol as the button:
/// audit the close, close the connection, let the sync client finish the upload, and
/// only then remove the lock. Dropping the [`Store`] alone would release the lock
/// without waiting, which invites the next workstation to open a file still on its
/// way up.
///
/// This is not a guarantee — a `SIGKILL`, a power cut or a panic during teardown
/// leaves the lock behind. That case is what the fifteen-minute staleness and the
/// deliberate take-over exist for.
impl Drop for YkDistApp {
    fn drop(&mut self) {
        if let Some(settled) = self.release_current_database() {
            tracing::info!(
                event = "db.closed",
                detail = settled.describe(),
                reason = "application exit"
            );
        }
    }
}

impl eframe::App for YkDistApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // The palette is the operator's, and installing it is idempotent — the
        // theme is compared against the one in context memory and the style
        // write is skipped when it has not changed.
        crate::ui::install_theme(ui.ctx(), self.settings.theme());

        // Deferred work first: dialogs are modal, and camera frames must not be
        // fetched from inside a paint closure.
        self.handle_db_request();
        // Says "this workstation still has the register" to the other operators
        // sharing a sync folder, and notices when one of them took it anyway.
        self.tick_lease();
        #[cfg(feature = "camera")]
        if self.scan.open {
            let ctx = ui.ctx().clone();
            self.poll_camera(&ctx);
        }

        if self.store.is_none() {
            crate::ui::database::show(self, ui);
            return;
        }

        self.top_bar(ui);
        self.status_bar(ui);

        // The body scrolls vertically only, and the screen fills the window
        // width: a table too wide for the window scrolls inside its own card
        // (`ui::table`) instead of dragging the whole page sideways and leaving
        // every other card narrower than the one that overflowed.
        egui::CentralPanel::default()
            .frame(gutter_frame(egui::Frame::central_panel(ui.style())))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Breathing room around the screen body; the panels
                        // above and below supply their own.
                        ui.add_space(14.0);
                        match self.tab {
                            Tab::Inventory => crate::ui::inventory::show(self, ui),
                            Tab::Holders => crate::ui::holders::show(self, ui),
                            Tab::Distribution => crate::ui::distribution::show(self, ui),
                            Tab::Bootstrap => crate::ui::bootstrap::show(self, ui),
                            Tab::Templates => crate::ui::templates::show(self, ui),
                            Tab::Terms => crate::ui::terms::show(self, ui),
                            Tab::Audit => crate::ui::audit::show(self, ui),
                            Tab::Settings => crate::ui::settings::show(self, ui),
                        }
                        ui.add_space(18.0);
                    });
            });
    }
}

/// Put the shell's [`crate::ui::GUTTER`] on both sides of a panel frame, so the
/// top bar, the screen body and the status bar share one left margin.
fn gutter_frame(frame: egui::Frame) -> egui::Frame {
    // Only the horizontal margin is ours; each panel keeps the vertical padding
    // egui gives it.
    let margin = frame.inner_margin;
    frame.inner_margin(egui::Margin {
        left: crate::ui::GUTTER,
        right: crate::ui::GUTTER,
        ..margin
    })
}

impl YkDistApp {
    /// Product name, build version, and the tab bar.
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let theme = elegance::Theme::current(ui.ctx());

        egui::Panel::top("tabs")
            .frame(gutter_frame(egui::Frame::side_top_panel(ui.style())))
            .show(ui, |ui| {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(
                        egui::RichText::new("YubiKey Distribution Manager")
                            .size(theme.typography.heading + 2.0)
                            .color(theme.palette.text)
                            .strong(),
                    ));
                    ui.add_space(8.0);
                    ui.add(
                        elegance::Badge::new(crate::VERSION, elegance::BadgeTone::Neutral)
                            .preserve_case(),
                    );
                });
                ui.add_space(6.0);

                let mut index = self.tab.index();
                ui.add(elegance::TabBar::new(
                    &mut index,
                    Tab::ALL.map(|tab| tab.label()),
                ));
                self.tab = Tab::from_index(index);
            });
    }

    /// Who is operating, where the database lives, and the last outcome.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        use crate::status::Severity;
        use elegance::IndicatorState;

        let theme = elegance::Theme::current(ui.ctx());
        let severity = crate::status::classify(&self.status);

        egui::Panel::bottom("status")
            .frame(gutter_frame(egui::Frame::side_top_panel(ui.style())))
            .show(ui, |ui| {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    let mut pill = elegance::StatusPill::new()
                        .item(format!("operator: {}", self.operator), IndicatorState::On);
                    if let Some(store) = &self.store {
                        pill = pill.item(
                            match store.location() {
                                Location::NetworkShare => "db: network share",
                                Location::LocalDisk => "db: local",
                                // The lock is the thing worth a place in the
                                // status bar: it is what makes a shared sync
                                // folder safe to work in, and its absence is what
                                // an operator has to know about.
                                Location::CloudSync if store.lease().is_some() => {
                                    "db: cloud-sync (locked)"
                                }
                                Location::CloudSync => "db: cloud-sync (UNLOCKED)",
                            },
                            IndicatorState::On,
                        );
                    }
                    ui.add(pill);

                    if !self.status.is_empty() {
                        ui.add_space(10.0);
                        // An audit failure is not allowed to look like an ordinary
                        // outcome (AGENTS.md, "Audit coverage").
                        let (colour, strong) = match severity {
                            Severity::Alarm => (theme.palette.danger, true),
                            Severity::Warning => (theme.palette.warning, false),
                            Severity::Normal => (theme.palette.text_muted, false),
                        };
                        let mut text = egui::RichText::new(&self.status)
                            .size(theme.typography.small)
                            .color(colour);
                        if strong {
                            text = text.strong();
                        }
                        ui.add(egui::Label::new(text).selectable(true));
                    }
                });
                ui.add_space(6.0);
            });
    }
}
