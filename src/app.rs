//! Application state and the egui shell.
//!
//! The app has two states: **locked** (waiting for the database password) and
//! **open**. Once open, every screen reads from the cached vectors refreshed by
//! [`YkDistApp::refresh`] — the GUI never queries SQLite inside a paint pass.

use std::path::{Path, PathBuf};

use crate::audit::AuditEntry;
use crate::device::{DeviceInfo, YubiKeyBackend};
use crate::domain::lifecycle::Dependency;
use crate::domain::{
    BootstrapRun, DeliveryMethod, DistributionRecord, Holder, KeyStatus, StepOutcome, StepStatus,
    YubiKeyRecord,
};
use crate::domain::{DocumentKind, SerialSource};
use crate::settings::AppSettings;
use crate::store::{Location, Store, StoreConfig};
use crate::template::{BootstrapTemplate, PlannedCommand, RenderContext, Trust};

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
    /// Connect the SMB share on the share card, then open or create the database
    /// on it.
    ///
    /// Carries which of the two, because open and create stay separate all the way
    /// down: a mistyped file name on a share must not silently become a second,
    /// empty register (`features/database-selection.md`).
    ConnectShare { create: bool },
    /// Fill the share card from a remembered share, ready to connect.
    UseShare(String),
    /// Drop a share from the remembered list. The share itself is not touched.
    ForgetShare(String),
    /// Close the database and disconnect the share this session connected.
    DisconnectShare,
    /// Set, change or remove the database password
    /// (`features/db-password-and-encryption.md` phases 2 and 5).
    ///
    /// Carries the *intent* and not the password: the new one lives in
    /// [`PasswordForm`], exactly as the share password lives in [`ShareForm`], so
    /// that no secret is ever inside a `Debug`-derived type that a log line could
    /// print.
    SetPassword {
        /// Take the password off, leaving a plain file.
        remove: bool,
    },
}

impl DbRequest {
    /// Would this request submit a password to an existing register?
    ///
    /// Those are the requests the unlock throttle applies to, and only those.
    /// Creating a database is not a guess at anybody's password, and neither is
    /// forgetting a path or closing what is open — slowing them down would be a
    /// throttle punishing the operator for a wrong password rather than
    /// discouraging the next guess.
    fn attempts_an_unlock(&self) -> bool {
        match self {
            DbRequest::Open(_)
            | DbRequest::PickExisting
            | DbRequest::TakeOverLock(_)
            | DbRequest::ConnectShare { create: false } => true,
            DbRequest::PickNew
            | DbRequest::Create(_)
            | DbRequest::Close
            | DbRequest::Forget(_)
            | DbRequest::ConnectShare { create: true }
            | DbRequest::UseShare(_)
            | DbRequest::ForgetShare(_)
            | DbRequest::DisconnectShare
            | DbRequest::SetPassword { .. } => false,
        }
    }
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

/// How often a share-hosted register is confirmed still reachable.
///
/// Five seconds: long enough that the check is invisible, short enough that an
/// operator who has just walked back to the desk is told before they start typing
/// into a register that is no longer there.
pub const SHARE_CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// The share that went away, and what it takes to get back on it.
///
/// **No password.** A named account's password was used once and dropped, which is
/// the rule the whole share feature is built on; that is exactly why a named-account
/// share cannot be reconnected automatically and the operator is asked.
#[derive(Debug, Clone)]
pub struct LostShare {
    pub location: String,
    /// The identity as it read in the status bar — for the message, not for reuse.
    pub identity: String,
    pub access: crate::store::smb::Access,
    /// `DOMAIN\user`, remembered; never a password.
    pub user: String,
}

/// The SMB share card's form state.
///
/// The password lives here rather than in
/// [`AppSettings`](crate::settings::AppSettings) for the reason the whole feature
/// turns on: it is typed, used for one connection, and cleared. Nothing writes it
/// anywhere.
#[derive(Default)]
pub struct ShareForm {
    /// What the operator typed: `smb://server/share/…`, `\\server\share\…` or
    /// `//server/share/…`.
    pub location: String,
    pub access: crate::store::smb::Access,
    /// `DOMAIN\user` or `user`, for a named account.
    pub user: String,
    /// Cleared as soon as the connection has been attempted.
    pub password: String,
    pub error: Option<String>,
}

impl ShareForm {
    /// Read and clear the typed password. Never kept beyond the connect call.
    fn take_password(&mut self) -> String {
        let password = self.password.clone();
        self.password.clear();
        password
    }
}

/// The "set or change the database password" form, in Settings.
///
/// Same rule as [`ShareForm`]: the typed password lives here for as long as the
/// operator is typing it and is cleared the moment it has been used. It is never
/// in a request, a setting, a log line or an audit entry.
#[derive(Default)]
pub struct PasswordForm {
    /// Is the form open? A password change is not something to offer as a stray
    /// button next to *Integrity check*.
    pub open: bool,
    /// The open form is the *removal* confirmation rather than a new password.
    ///
    /// Removal has no fields to fill in, which is exactly why it needs a step of
    /// its own: a one-click button that turns encryption off would be the easiest
    /// thing on this screen to press by accident.
    pub removing: bool,
    pub new: String,
    /// Typed twice, because a mistyped password that nobody can verify against
    /// anything is a lost register — there is no reset and no administrator.
    pub confirm: String,
    pub error: Option<String>,
}

impl PasswordForm {
    /// Read and clear both fields.
    fn take(&mut self) -> (String, String) {
        let new = std::mem::take(&mut self.new);
        let confirm = std::mem::take(&mut self.confirm);
        (new, confirm)
    }

    /// Close and clear. Called when the form is dismissed and after it is used,
    /// so a typed password never outlives the operation it was typed for.
    pub fn dismiss(&mut self) {
        let _ = self.take();
        self.open = false;
        self.removing = false;
        self.error = None;
    }
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
    /// The operator has ticked "nobody is working in this register", which is what
    /// arms the take-over button while the holder is still refreshing its lock.
    ///
    /// Lives here, next to the refusal it belongs to, so that it is cleared by the
    /// next refusal: every *Try again* that comes back held asks the question
    /// again, rather than leaving a dangerous button armed from a minute ago.
    pub break_confirmed: bool,
}

/// Consignment-term panel state (distribution screen).
pub struct TermPanel {
    pub open: bool,
    /// Which hand-over the term is for.
    pub distribution: Option<uuid::Uuid>,
    /// The unit's own reference for the signed term, being typed.
    ///
    /// Here rather than in the record form because this is the moment it exists: a
    /// posted key's term comes back days after the hand-over was recorded.
    pub reference: String,
    /// Which document is on screen: [`crate::term::CONSIGNMENT_ID`] or
    /// [`crate::term::RETURN_ID`].
    ///
    /// One panel for both, because everything around the wording is identical —
    /// language selection, the text and PDF outputs, filing the signed copy. What
    /// differs is which template id is rendered and which `DocumentKind` the signed
    /// copy is filed as, and both follow from this field.
    pub document: String,
    /// Language the operator asked for.
    pub language: String,
    /// The rendered term, awaiting review before it is saved or printed.
    pub rendered: Option<String>,
    /// The same term as a PDF, produced alongside the text so that what the
    /// operator reviews on screen and what gets printed cannot drift apart.
    pub pdf: Option<Vec<u8>>,
    /// Set when the PDF font cannot carry a character the term uses — the
    /// operator is told before the document is filed, not afterwards.
    pub pdf_note: Option<String>,
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
            document: crate::term::CONSIGNMENT_ID.to_owned(),
            reference: String::new(),
            language: crate::term::DEFAULT_LANGUAGE.to_owned(),
            rendered: None,
            pdf: None,
            pdf_note: None,
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

    /// Path typed for an import or an export, so both work in a build without
    /// `file-dialog` — and so the whole flow is testable without a modal dialog,
    /// which is the reason it is a field rather than a local in the paint code.
    pub file_path: String,
    /// A template read from a file and **not yet stored**: the import is a
    /// preview, then a decision.
    ///
    /// Held here rather than applied on the spot because an import deserves the
    /// same treatment as the CSV import — see what it would do, then say yes. The
    /// preview carries the trust verdict and the diff against what this register
    /// already holds, which together answer "should I store this?".
    pub pending_import: Option<PendingImport>,

    /// Which two versions the compare card is showing.
    ///
    /// `(id, from, to)`. Kept across frames because the diff is recomputed from
    /// the cached catalogue every frame — it is pure data, so there is nothing to
    /// cache, and nothing to go stale after a save.
    pub compare: Option<(String, String, String)>,
}

/// The "trust this key" form on the Settings screen.
///
/// A form of its own rather than three locals in the paint code, so the refusal a
/// malformed key gets survives the frame it was typed in.
#[derive(Default)]
pub struct TemplateKeyForm {
    pub id: String,
    pub public_key: String,
    pub comment: String,
    pub error: Option<String>,
}

/// A template read from a file, waiting for the operator to accept it.
pub struct PendingImport {
    /// Where it came from, for the summary. Not stored anywhere.
    pub source: String,
    pub file: crate::template::TemplateFile,
    /// The signature verdict under *this* deployment's trusted keys.
    pub trust: crate::template::Trust,
    /// What it would change, against the newest version of the same id this
    /// register holds. `None` when the id is new here.
    pub against: Option<(String, crate::template::TemplateDiff)>,
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

/// What has happened to one key since it was handed over, and the forms that
/// record the next thing (`features/key-lifecycle-and-revocation.md` phases 2, 3,
/// 4, 6, 7 and 8).
///
/// Read once when the panel is opened and after every write, rather than every
/// frame: an incident is answered over hours, the panel is open while somebody
/// works through a list, and re-reading five tables sixty times a second to paint
/// the same sentences would make an operator's screen the busiest reader of a
/// register that may be on a share.
pub struct LifecyclePanel {
    /// Serial the panel is about. `None` while it is closed.
    pub serial: Option<u32>,

    /// The loss report being drafted, and the fields it needs.
    pub report_open: bool,
    pub report_kind: crate::domain::IncidentKind,
    /// `YYYY-MM-DD`, empty meaning today
    /// ([`crate::domain::lifecycle::parse_report_date`]).
    pub report_date: String,
    pub reported_by: String,
    pub circumstances: String,

    /// The dependency whose remediation is being recorded — the subject, because
    /// that is what identifies it in the list and in the record.
    pub settling: Option<Dependency>,
    pub revocation_reason: crate::domain::RevocationReason,
    pub reference: String,
    pub detail: String,

    /// The applets an operator is claiming were reset outside this tool.
    pub sanitised_applets: Vec<crate::device::reset::Applet>,
    pub sanitised_open: bool,

    /// The RMA forms: one to send, one to link what came back.
    pub rma_open: bool,
    pub rma_reference: String,
    pub rma_fault: String,
    pub rma_replacement: String,

    /// The incident note, once produced, and which incident it is about.
    pub note: Option<(uuid::Uuid, String)>,

    /// What the register says right now, reloaded after each write.
    pub incidents: Vec<crate::domain::KeyIncident>,
    pub remediations: Vec<crate::domain::Remediation>,
    pub dependencies: Vec<Dependency>,
    pub rma: Vec<crate::domain::RmaCase>,
    pub sanitisation: crate::domain::Sanitisation,

    pub error: Option<String>,
    pub notice: Option<String>,
}

impl Default for LifecyclePanel {
    fn default() -> Self {
        Self {
            serial: None,
            report_open: false,
            report_kind: crate::domain::IncidentKind::Lost,
            report_date: String::new(),
            reported_by: String::new(),
            circumstances: String::new(),
            settling: None,
            revocation_reason: crate::domain::RevocationReason::KeyCompromise,
            reference: String::new(),
            detail: String::new(),
            sanitised_applets: Vec::new(),
            sanitised_open: false,
            rma_open: false,
            rma_reference: String::new(),
            rma_fault: String::new(),
            rma_replacement: String::new(),
            note: None,
            incidents: Vec::new(),
            remediations: Vec::new(),
            dependencies: Vec::new(),
            rma: Vec::new(),
            sanitisation: crate::domain::Sanitisation::default(),
            error: None,
            notice: None,
        }
    }
}

/// The factory reset waiting for the operator to confirm it, and what it found
/// (`features/key-lifecycle-and-revocation.md` phase 5).
///
/// Separate from [`InventoryPanel`] because the two confirmations are about
/// different things and must not be able to be open at once: removing a row
/// deletes a record and leaves the key alone; a reset leaves the record and
/// destroys what is on the key.
#[derive(Default)]
pub struct ResetPanel {
    /// Serial the operator asked to reset. `None` while the panel is closed.
    pub serial: Option<u32>,
    /// Which applets are ticked, in the order they will be reset.
    pub applets: Vec<crate::device::reset::Applet>,
    /// The serial typed back by hand. The button stays disabled until it matches:
    /// a reset destroys credentials on hardware, so agreeing to it should cost
    /// more than a click landing on the wrong row.
    pub typed: String,
    /// What the applets held when the panel opened — read once, read-only, so the
    /// preview names this key's actual loss rather than a generic one.
    pub observed: crate::device::AppletStates,
    /// What the run did, per applet, once it has run.
    pub outcomes: Vec<crate::device::reset::Outcome>,
    pub error: Option<String>,
    /// The power cycle a confirmed FIDO2 reset waits on, and the applets it will
    /// run once the key is back (`crate::device::reinsert`). `None` when no reset
    /// is arming, which is every frame except the few seconds this takes.
    pub handshake: Option<crate::device::reinsert::Handshake>,
    /// The fast presence poll that drives it. Dropped the moment the reset fires,
    /// so nothing enumerates the port while `ykman` is writing to the key.
    pub presence: Option<crate::device::reinsert::PresenceWatch>,
    /// The last presence snapshot, read once per frame like the device watch's.
    pub presence_seen: crate::device::reinsert::Presence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Inventory,
    Holders,
    Distribution,
    Bootstrap,
    Templates,
    Terms,
    Reports,
    Audit,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 9] = [
        Tab::Inventory,
        Tab::Holders,
        Tab::Distribution,
        Tab::Bootstrap,
        Tab::Templates,
        Tab::Terms,
        Tab::Reports,
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
            Tab::Reports => "Reports",
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

/// Bulk-enrolment state (`features/bulk-enrollment.md`).
///
/// The batch drives the **wizard**, rather than reimplementing it: each key still
/// gets its own plan, its own pre-flight, its own confirmation and its own audit
/// entries. A batch mode that had its own quieter run path would be a second way
/// of writing to a key, and the second way is always the one that skips a check.
#[derive(Default)]
pub struct BatchPanel {
    /// Is the operator setting one up, or working through one?
    pub open: bool,
    pub shape: crate::batch::Shape,
    /// How many keys the operator says are in the box.
    pub planned: usize,
    /// The pairing list as typed or loaded, before it is parsed.
    pub pairing_text: String,
    /// The parsed list, once it has been accepted whole.
    pub pairs: Vec<crate::batch::pairing::Pair>,
    /// The batch in progress.
    pub current: Option<crate::batch::Batch>,
    /// Which position the wizard is working on right now.
    pub position: Option<usize>,
    /// Batches somebody could pick up, read when the screen opens.
    pub resumable: Vec<crate::batch::Batch>,
    pub error: Option<String>,
    /// The last thing that happened, kept beside the batch rather than in the
    /// status bar — during a batch the status bar is the *run's*.
    pub notice: Option<String>,
}

/// Reports screen state (`features/reports-and-export.md`).
///
/// The generated report is held here rather than rebuilt every frame, and that
/// is the behaviour rather than an optimisation: a report carries the moment it
/// was generated, and one that silently re-derived itself sixty times a second
/// would answer a different question each time the operator looked away. It is
/// generated when they ask, and it says when.
pub struct ReportPanel {
    pub kind: crate::report::ReportKind,
    pub current: Option<crate::report::Report>,
    pub format: crate::report::export::Format,
    pub error: Option<String>,
    /// How far ahead the certificate-expiry report looks.
    pub expiry_days: i64,
    /// What the audit extract is cut with. Its own filter rather than the Audit
    /// screen's: an extract for the ESI is a deliberate range, and inheriting
    /// whatever was last typed on another screen is how the wrong range gets
    /// sent.
    pub audit_filter: crate::audit::AuditFilter,
    pub from: String,
    pub until: String,
}

impl Default for ReportPanel {
    fn default() -> Self {
        Self {
            kind: crate::report::ReportKind::InventorySummary,
            current: None,
            format: crate::report::export::Format::Csv,
            error: None,
            expiry_days: crate::report::DEFAULT_EXPIRY_WINDOW_DAYS,
            audit_filter: crate::audit::AuditFilter::default(),
            from: String::new(),
            until: String::new(),
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
    /// The run this hand-over is attached to, when the operator arrived from the
    /// post-run summary (`features/gui-bootstrap-wizard.md` phase 6).
    ///
    /// Set explicitly rather than left to `link_last_run`'s "the newest run on this
    /// serial", because the two differ exactly where it matters: a key that was
    /// reset and bootstrapped again has more than one run, and *this* hand-over is
    /// for the one the operator was just looking at.
    pub run_id: Option<uuid::Uuid>,
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
            run_id: None,
            error: None,
        }
    }
}

/// Search, sort and page state for one table.
///
/// The rules live in [`crate::browse`], where they are tested; this is only what
/// the operator has currently typed and clicked.
#[derive(Debug, Clone, Default)]
pub struct Browse<S> {
    pub query: String,
    pub sort: S,
    pub direction: crate::browse::Direction,
    pub page: usize,
}

impl<S: Copy + PartialEq> Browse<S> {
    /// Clicking a column header sorts by it, or reverses it if it was already
    /// the sorted one — the behaviour every table in every tool has, so it needs
    /// no explaining at a desk.
    pub fn sort_by(&mut self, column: S) {
        if self.sort == column {
            self.direction = self.direction.toggled();
        } else {
            self.sort = column;
            self.direction = crate::browse::Direction::Ascending;
        }
        // A new ordering makes the current page meaningless.
        self.page = 0;
    }

    pub fn query(&self) -> crate::browse::Query {
        crate::browse::Query::new(&self.query)
    }
}

/// What the wizard is doing right now.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub enum WizardStage {
    /// Choosing key, holder and template; building a plan.
    #[default]
    Selecting,
    /// The plan is built and the pre-flight has run; the confirmation is on
    /// screen and nothing has been written.
    Confirming,
    /// A run is in progress or has finished.
    Running,
}

/// Persists a run's progress and its audit entries as the executor produces them.
///
/// Owns the `Store` for the duration of the run rather than borrowing it: the
/// executor needs `&mut dyn RunRecorder` while the app is also handing it a
/// `&mut dyn WriteBackend`, and taking the store out and putting it back is
/// simpler to read than threading two disjoint borrows of `self` through.
///
/// A failure here stops the run before the next write. That is deliberate and it
/// is the strictest rule in the engine: a configured key with no record of what
/// was applied is the failure this whole tool exists to prevent.
struct StoreRecorder {
    store: Store,
    operator: String,
    /// The first failure, kept so the status line can name it.
    failure: Option<String>,
}

impl crate::bootstrap::RunRecorder for StoreRecorder {
    fn run_updated(&mut self, run: &BootstrapRun) -> Result<(), String> {
        self.store.insert_run(run).map_err(|e| {
            let message = e.to_string();
            self.failure.get_or_insert(message.clone());
            message
        })
    }

    fn audit(&mut self, event: &str, target: &str, detail: &str) -> Result<(), String> {
        self.store
            .append_audit(&self.operator, event, target, detail)
            .map(|_| ())
            .map_err(|e| {
                let message = e.to_string();
                self.failure.get_or_insert(message.clone());
                message
            })
    }
}

/// The same recorder serves the factory reset, which has no run to persist and
/// the same absolute requirement that its trail be written.
impl crate::device::reset::Recorder for StoreRecorder {
    fn audit(&mut self, event: &str, target: &str, detail: &str) -> Result<(), String> {
        crate::bootstrap::RunRecorder::audit(self, event, target, detail)
    }
}

/// Whether a run is starting or being finished.
///
/// One driver for both, because everything either side of the executor call — the
/// signature gate, the SAN, the transport, the recorder — is the same, and a
/// second copy of it would be a second place for the gate to be forgotten.
enum RunMode {
    Fresh(crate::bootstrap::Confirmation),
    Resume {
        run: crate::domain::BootstrapRun,
        /// The PIV PIN the operator typed, because nothing was retained: a resume
        /// cannot authenticate to the applet with a secret this run no longer has.
        piv_pin: Option<crate::secret::Secret>,
    },
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
    pub stage: WizardStage,
    /// What the pre-flight found for the current plan and key.
    pub findings: Vec<crate::bootstrap::Finding>,
    /// The run in progress or just finished, for the live view and the summary.
    pub run: Option<crate::domain::BootstrapRun>,
    /// Generated secrets, shown once and wiped when the panel is dismissed.
    ///
    /// Not `Clone` and never persisted — see [`crate::secret::ShowOnce`].
    pub secrets: Option<crate::secret::ShowOnce>,
    /// The issued certificate the operator has pasted or loaded, waiting to be
    /// imported (`features/ca-integration.md` phase 1).
    ///
    /// A plain `String` and deliberately not a [`crate::secret::Secret`]: a
    /// certificate is a public document, and treating it as a secret would mean it
    /// could not be shown, checked or kept as evidence — which is the opposite of
    /// what it is for.
    pub certificate_pem: String,
    /// What was read out of that certificate, so the operator sees whose it is
    /// before it reaches the key.
    pub certificate_preview: Option<Result<crate::device::certificate::Summary, String>>,
    /// The exact template version a **resumed** run used, pinned.
    ///
    /// `template_index` indexes `YkDistApp::templates`, which is the newest version
    /// of each id — what a *new* run may be started from. A run being finished days
    /// later may have applied a version that has since been superseded, and that
    /// version is not in that list at all, so the wizard's own selector cannot
    /// express it. Pinning is how (`features/gui-bootstrap-wizard.md` phase 5);
    /// cleared when the wizard is reset, so it cannot leak into the next run.
    pub pinned_template: Option<BootstrapTemplate>,
    /// The PIV PIN, typed for a resume only.
    ///
    /// A `String` in the wizard and a [`crate::secret::Secret`] the moment it is
    /// used: it has to live in a text field to be typed at all. Cleared as soon as
    /// the run that needed it has been driven, and never recorded.
    pub resume_pin: String,
}

impl std::fmt::Debug for Wizard {
    /// Hand-written because [`crate::secret::ShowOnce`] must never be printed by
    /// an accident of derivation. It redacts itself, but the wizard is the type
    /// most likely to end up in a panic message, so this is belt and braces.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wizard")
            .field("serial", &self.serial)
            .field("stage", &self.stage)
            .field("steps", &self.plan.len())
            .field("secrets", &self.secrets.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

pub struct YkDistApp {
    pub config: StoreConfig,
    pub store: Option<Store>,
    pub settings: AppSettings,
    pub db_form: DatabaseForm,
    pub db_request: Option<DbRequest>,
    /// The SMB share this session connected, held for as long as the database on
    /// it is open. `None` for a local file, and `None` for a share the operating
    /// system had already mounted — that one is not this session's to take down.
    pub share: Option<crate::store::smb::ShareConnection>,
    pub share_form: ShareForm,
    /// When the share-hosted register was last confirmed reachable
    /// (`features/smb-share-hosting.md` phase 9).
    pub share_checked: Option<std::time::Instant>,
    /// The share this session lost, while it is lost. `None` the rest of the time.
    pub share_lost: Option<LostShare>,
    /// Setting, changing or removing the database password.
    pub password_form: PasswordForm,
    /// The public key being added to the template trust store.
    pub key_form: TemplateKeyForm,
    /// Wrong passwords at the unlock prompt, and the wait they have earned
    /// (`features/db-password-and-encryption.md` phase 3).
    ///
    /// Session state on purpose: it is not persisted, because a throttle written
    /// to disk is one that can be edited away, and it is not a lockout — there is
    /// no administrator to lift one on a shared register.
    pub throttle: crate::password::Throttle,
    /// How this session reaches an SMB share.
    ///
    /// A factory rather than a value, because each connection consumes one — and
    /// swappable for the same reason [`YkDistApp::backend`] is: the behaviour suite
    /// drives connect → open → write → close → disconnect with no file server
    /// anywhere near it, and a test that mounted a real share would be a test that
    /// needs a network.
    pub share_connector: Box<dyn Fn() -> Box<dyn crate::store::smb::Connector>>,
    pub scan: ScanPanel,
    pub open_error: Option<String>,
    pub tab: Tab,
    /// Operator credential recorded on every distribution and audit entry.
    pub operator: String,
    pub org: String,
    pub backend: Box<dyn YubiKeyBackend>,
    /// Which transport this session reads through, and why
    /// (`features/native-device-transport.md` phase 6). Decided once at startup, and
    /// again when the operator changes it in Settings.
    pub transport: crate::device::TransportChoice,
    pub detected: Vec<DeviceInfo>,
    /// The background watch, while a screen that shows attached keys is open
    /// (`features/device-detection.md` phase 2). `None` means nothing is polling.
    pub watch: Option<crate::device::DeviceWatch>,
    /// The latest snapshot from the watch, read once per frame.
    ///
    /// Copied onto the app rather than read from the watch inside the paint code,
    /// so a screen paints one consistent picture: two reads in one frame could
    /// otherwise straddle a poll and show a key in one card and not in another.
    pub attached: crate::device::Attached,
    /// Which attached key the operator chose, when more than one is attached
    /// (phase 3).
    ///
    /// `None` is not "the first one": with several attached, every operation that
    /// needs a key is refused until one is chosen. Picking one for the operator and
    /// writing a PIN to it is the worst outcome this feature has.
    pub selected_serial: Option<u32>,
    /// The generation the operator has already been shown, so the arrival of a key
    /// can change the screen once instead of on every frame.
    pub seen_generation: u64,
    pub status: String,

    pub keys: Vec<YubiKeyRecord>,
    pub holders: Vec<Holder>,
    pub distributions: Vec<DistributionRecord>,
    pub runs: Vec<BootstrapRun>,
    pub audit_view: Vec<AuditEntry>,
    /// Applet reads made this session, by serial.
    ///
    /// A cache with one rule: a serial that is **not** in here has not been read,
    /// and no screen may say anything about its applets. That is what lets the
    /// Inventory badge a key still on its factory defaults
    /// (`features/step-piv-pin-puk-management-key.md` phase 6) without ever
    /// accusing a key nobody looked at — the same distinction every reader in
    /// `device::applets` turns on.
    ///
    /// Not persisted. It is a read of hardware at a moment, and a stale one on the
    /// register would be worse than none: the key it describes may be in somebody
    /// else's hand by then.
    pub applet_reads: std::collections::HashMap<u32, crate::device::applets::Snapshot>,
    /// What the wizard may offer: the newest version of each template in use.
    pub templates: Vec<BootstrapTemplate>,
    /// Every template version on record, retired ones included, with its run
    /// count — what the Templates screen manages.
    pub template_catalogue: Vec<crate::template::StoredTemplate>,

    pub holder_form: HolderForm,
    pub dist_form: DistForm,
    pub inventory: InventoryPanel,
    /// The factory reset waiting to be confirmed, and what it did.
    pub reset: ResetPanel,
    /// What has happened to one key since the hand-over, and what is still owed.
    pub lifecycle: LifecyclePanel,
    pub wizard: Wizard,
    pub template_editor: TemplateEditor,
    pub term_panel: TermPanel,
    pub term_editor: TermEditor,
    /// The Reports screen: which report, the one generated, and where it goes.
    pub reports: ReportPanel,
    /// A box of keys being bootstrapped in one sitting.
    pub batch: BatchPanel,
    /// Who else has this register open right now
    /// (`features/database-selection.md` phase 6).
    ///
    /// Refreshed with the other views and on the lease tick, so the banner ages
    /// out on its own rather than needing somebody to press Refresh.
    pub presence: crate::store::presence::Presence,
    /// Term templates, refreshed with everything else.
    pub term_templates: Vec<crate::term::TermTemplate>,
    /// How many documents each distribution has.
    pub document_counts: std::collections::BTreeMap<uuid::Uuid, usize>,
    /// What is on file against each hand-over, per kind — what the signature state
    /// is computed from (`features/receipts-and-terms.md` phase 4).
    pub filed_documents: std::collections::BTreeMap<uuid::Uuid, crate::receipt::Filed>,

    /// Search, sort and page state for the three tables (`gui-shell` 3, 4).
    pub browse_keys: Browse<crate::browse::KeySort>,
    pub browse_holders: Browse<crate::browse::HolderSort>,
    pub browse_distributions: Browse<crate::browse::DistributionSort>,
    /// Show only keys in this state, when the operator has narrowed it.
    pub key_status_filter: Option<crate::domain::KeyStatus>,
    /// Show only hand-overs nobody has returned.
    pub outstanding_only: bool,
    /// The About box's contents while it is open, and `None` while it is not
    /// (`features/application-icon.md` phase 7).
    ///
    /// The **rendered diagnostic report**, gathered once when the box is opened
    /// rather than every frame. Gathering reads the filesystem and enumerates the
    /// cameras; doing that sixty times a second for a panel nobody is interacting
    /// with would be absurd, and there is nothing to gain — a support report is a
    /// snapshot of the moment somebody asked for it, which is exactly what this is.
    ///
    /// Opened from the version badge in the top bar, which is already the thing
    /// somebody points at when asked which version they are running.
    pub about: Option<String>,
    /// Recent log lines, for the panel (`gui-shell` 8).
    pub log: crate::logbuf::LogBuffer,
    pub log_panel_open: bool,
    pub log_min_level: crate::logbuf::Level,
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

        // Which transport reads the hardware, decided once here rather than fixed at
        // compile time (`features/native-device-transport.md` phase 6). Before this,
        // `ykman` was hardcoded — so a build with the hardware-verified native
        // transport compiled in still shelled out to a subprocess for every read.
        let transport = crate::device::select::decide(
            settings.transport,
            crate::device::select::probe(settings.transport),
        );
        let backend = crate::device::select::backend_for(&transport);
        tracing::info!(
            event = "device.transport.selected",
            transport = transport.transport.slug(),
            disabled = transport.disabled,
            reason = transport.reason.as_str()
        );

        let config = StoreConfig::new(path);
        let mut app = Self {
            config,
            store: None,
            settings,
            db_form: DatabaseForm::default(),
            db_request: None,
            share: None,
            share_form: ShareForm::default(),
            share_checked: None,
            share_lost: None,
            password_form: PasswordForm::default(),
            key_form: TemplateKeyForm::default(),
            throttle: crate::password::Throttle::new(),
            share_connector: Box::new(crate::store::smb::platform_connector),
            scan: ScanPanel::default(),
            open_error: None,
            tab: Tab::Inventory,
            operator,
            org,
            backend,
            transport,
            watch: None,
            attached: crate::device::Attached::default(),
            selected_serial: None,
            seen_generation: 0,
            detected: Vec::new(),
            status: String::new(),
            keys: Vec::new(),
            holders: Vec::new(),
            distributions: Vec::new(),
            runs: Vec::new(),
            audit_view: Vec::new(),
            applet_reads: std::collections::HashMap::new(),
            templates: Vec::new(),
            template_catalogue: Vec::new(),
            holder_form: HolderForm::default(),
            dist_form: DistForm::default(),
            inventory: InventoryPanel::default(),
            reset: ResetPanel::default(),
            lifecycle: LifecyclePanel::default(),
            wizard: Wizard::default(),
            template_editor: TemplateEditor::default(),
            term_panel: TermPanel::default(),
            term_editor: TermEditor::default(),
            reports: ReportPanel::default(),
            batch: BatchPanel::default(),
            presence: crate::store::presence::Presence::default(),
            term_templates: Vec::new(),
            document_counts: std::collections::BTreeMap::new(),
            filed_documents: std::collections::BTreeMap::new(),
            browse_keys: Browse::default(),
            browse_holders: Browse::default(),
            browse_distributions: Browse::default(),
            key_status_filter: None,
            outstanding_only: false,
            about: None,
            log: crate::logbuf::LogBuffer::new(),
            log_panel_open: false,
            log_min_level: crate::logbuf::Level::Info,
        };

        app.db_form.path = app.config.path.display().to_string();

        // A remembered database that has gone (an unmounted share) must not be
        // re-created as an empty file — show the chooser instead.
        if must_exist && !app.config.path.is_file() {
            // "Is the share mounted?" names the problem and offers nothing, so when
            // this workstation has reached a share from here before, the message
            // points at the card that can reach it again.
            app.open_error = Some(match app.settings.recent_shares.first() {
                Some(share) => format!(
                    "{} is not reachable. If the register is on a file server, connect the share \
                     below — this workstation last used {}.",
                    app.config.path.display(),
                    share.location
                ),
                None => format!(
                    "{} is not reachable — is the share mounted? A network share can be \
                     connected below.",
                    app.config.path.display()
                ),
            });
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

        // The throttle is enforced here rather than only by a disabled button, so
        // that it is one rule in testable code instead of a property of the paint
        // pass: every way of asking to open a register — a click, the recent list,
        // a share, a lock taken over — arrives through this function.
        //
        // Checked before `take_password`, so a refusal does not empty the field
        // the operator will submit again when the wait is over.
        if request.attempts_an_unlock()
            && let Some(wait) = self.throttle.message()
        {
            self.db_form.error = Some(wait.clone());
            self.open_error = Some(wait);
            return;
        }

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
            DbRequest::ConnectShare { create } => self.connect_share(create),
            DbRequest::UseShare(location) => self.fill_share_form(&location),
            DbRequest::ForgetShare(location) => {
                self.settings.forget_share(&location);
                self.settings.save_quietly();
            }
            DbRequest::DisconnectShare => self.close_database(),
            DbRequest::SetPassword { remove } => self.change_database_password(remove),
        }
    }

    /// Fill the share card from a remembered share.
    ///
    /// The password is not among what is filled in: it is not remembered, and the
    /// field is left for the operator to type.
    fn fill_share_form(&mut self, location: &str) {
        let Some(entry) = self
            .settings
            .recent_shares
            .iter()
            .find(|entry| entry.location == location)
            .cloned()
        else {
            return;
        };
        self.share_form.location = entry.location;
        self.share_form.access = entry.access;
        self.share_form.user = entry.user;
        self.share_form.password.clear();
        self.share_form.error = None;
    }

    /// Connect the share on the card, then open or create the database on it.
    ///
    /// Three steps that stay distinguishable on purpose: a parse refusal is the
    /// operator's typing, a connection refusal is the file server's answer, and a
    /// failed open is the database. Collapsing them into one message would leave
    /// somebody retyping a path when the real answer was "ask for access to the
    /// share".
    pub fn connect_share(&mut self, create: bool) {
        use crate::store::smb::{self, Access, Credential};

        self.share_form.error = None;
        let typed = self.share_form.location.clone();
        let password = self.share_form.take_password();

        let parsed = match smb::parse(&typed) {
            Ok(parsed) => parsed,
            Err(e) => return self.report_share_failure(e.to_string()),
        };

        // A share with no file named inside it is a share, not a register. Refused
        // before anything is sent to a server and before anything open is closed.
        if parsed.target.inner.is_empty() {
            return self.report_share_failure(format!(
                "{} names a share but no database inside it — say which file, for example \
                 {}/yubikeys/{}",
                parsed.target.describe(),
                parsed.target.describe(),
                crate::paths::DEFAULT_DATABASE_NAME
            ));
        }

        // The chosen identity decides; a user written into the location does not
        // quietly change it. `smb://felipe@server/share` while the mode says "the
        // signed-in user" is a contradiction the operator has to resolve, because
        // guessing wrong opens the register as an identity nobody reviewed — and on
        // a share that is read-only for everyone else, that looks like lost writes.
        let access = self.share_form.access;
        if access != Access::Named
            && let Some(named) = parsed.user.as_deref()
        {
            return self.report_share_failure(format!(
                "the location names the account `{named}`, but the chosen identity is {} — \
                 either choose a named account, or take `{named}@` out of the location",
                access.label(),
            ));
        }
        let user = match access {
            Access::Named => {
                let typed = self.share_form.user.trim();
                if typed.is_empty() {
                    parsed.user.clone().unwrap_or_default()
                } else {
                    typed.to_owned()
                }
            }
            _ => String::new(),
        };
        let credential = match access {
            Access::LoggedOnUser => Credential::logged_on_user(),
            Access::Anonymous => Credential::anonymous(),
            Access::Named if user.is_empty() => {
                return self.report_share_failure(
                    "a named account needs a user name — or choose the signed-in user, or guest \
                     if the share allows it"
                        .to_owned(),
                );
            }
            Access::Named => Credential::named(&user, &password),
        };
        // The credential has its own copy now; this one dies here.
        drop(password);

        // Whatever is open goes first, and completely: the audit entries, the
        // close, the cloud-sync lock, and the share that database was on.
        // Connecting first would risk taking down, a moment later, the very mount
        // the new connection had just adopted — when both turn out to be the same
        // share.
        self.release_current_database();

        let connection =
            match smb::ShareConnection::open(&parsed.target, &credential, (self.share_connector)())
            {
                Ok(connection) => connection,
                Err(e) => return self.report_share_failure(e.to_string()),
            };
        let database = connection.database_path();
        let describe = connection.describe();

        self.settings.remember_share(crate::settings::ShareEntry {
            location: parsed.target.location(),
            access,
            user,
        });
        self.settings.save_quietly();

        // The database's password is a different secret, from the other field.
        let db_password = self.take_password();
        let config = connection
            .store_config()
            .with_password(db_password)
            .with_operator(&self.operator);
        let opened = if create {
            Store::create_new(&config)
        } else {
            Store::open_existing(&config)
        };

        match opened {
            Ok(store) => {
                self.adopt(store, config);
                self.share = Some(connection);
                self.record("db.share.connected", "database", &describe);
                self.record(
                    if create { "db.created" } else { "app.opened" },
                    "database",
                    &self.config.path.display().to_string(),
                );
                // Creating a register is not unlocking one: the password was set
                // here rather than proved against a file that already had one.
                if !create {
                    self.note_unlock_success();
                }
                self.refresh();
                self.status = format!("{} — {describe}", self.status);
            }
            Err(e) => {
                // A share connected for a database that would not open is a
                // connection nobody asked for. Dropping it disconnects it.
                drop(connection);
                self.report_open_failure(&database, e);
            }
        }
    }

    /// Show a share refusal on the chooser as well as in the log.
    ///
    /// It cannot be audited: there is no open database to write it to. The same
    /// rule as a refused open — see `features/database-selection.md`.
    fn report_share_failure(&mut self, message: String) {
        tracing::error!(event = "db.share.failed", reason = %message);
        self.share_form.error = Some(message.clone());
        self.open_error = Some(message);
    }

    /// Disconnect the share this session connected, if it connected one.
    ///
    /// Called *after* the database on it has been closed, never before: the order
    /// is audit, close the database, then disconnect — the entry that records the
    /// disconnection needs a database to be written to.
    fn release_current_share(&mut self) {
        let Some(connection) = self.share.take() else {
            return;
        };
        let share = connection.target().describe();
        match connection.close() {
            Ok(()) => tracing::info!(event = "db.share.released", share = %share),
            Err(e) => {
                tracing::error!(event = "db.share.detach.failed", share = %share, reason = %e);
                // Loud, not swallowed: the operator has to know the share is still
                // attached, because what they do next may depend on it.
                self.status = format!("WARNING: {share} is still connected — {e}");
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
                self.note_unlock_success();
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
                    .map(|previous| {
                        // Whether the holder had gone quiet is the whole difference
                        // between clearing a crashed session's leftovers and cutting
                        // in on a live one, and it is exactly what a later reader of
                        // the trail needs — the holder's own line does not say it,
                        // because staleness is a judgement about *now*.
                        if previous.is_stale() {
                            previous.to_string()
                        } else {
                            format!(
                                "{previous} — that lock was still being refreshed; it was taken \
                                 over deliberately"
                            )
                        }
                    })
                    .unwrap_or_else(|| "no lock was there to take".into());
                self.adopt(store, config);
                self.record("db.lock.taken_over", "database", &broken);
                self.record(
                    "app.opened",
                    "database",
                    &self.config.path.display().to_string(),
                );
                self.note_unlock_success();
                self.refresh();
                // The register is open and the clock has moved since it was last
                // looked at: a term that went overdue while nobody had this file
                // open is recorded now, once (`crate::receipt`).
                self.check_overdue_signatures();
            }
            Err(e) => self.report_open_failure(path, e),
        }
    }

    /// A password was accepted: clear the throttle, and audit the unlock.
    ///
    /// `db.unlocked` is only written for a register that *is* encrypted — on a
    /// plain file there was nothing to unlock, and an entry saying otherwise would
    /// make the trail read as though the file were protected.
    fn note_unlock_success(&mut self) {
        self.throttle.succeeded();
        if self
            .store
            .as_ref()
            .is_some_and(|store| store.is_encrypted())
        {
            self.record("db.unlocked", "database", "");
        }
    }

    /// A password was refused: count it, and say so in the log.
    ///
    /// The entry cannot go where it belongs. `db.unlock.failed` is about the
    /// database that would not open, and the audit table is *inside* that
    /// database — so it goes to the log, and to the segregated mirror in a
    /// deployment that configures one (`features/audit-trail.md`). Either way it
    /// carries [`crate::password::Throttle::audit_detail`], which is a count and
    /// nothing else: no password, and not even its length.
    fn note_unlock_failure(&mut self, path: &Path) {
        let delay = self.throttle.failed();
        tracing::warn!(
            event = "db.unlock.failed",
            path = %path.display(),
            detail = %self.throttle.audit_detail(),
            delay_s = delay.as_secs(),
        );
    }

    /// The open configuration this session uses: the operator's name goes into
    /// the lock file, so a refusal on another workstation names a person.
    fn store_config(&self, path: &Path) -> StoreConfig {
        StoreConfig::new(path).with_operator(&self.operator)
    }

    /// Show a failed open, keeping a lock refusal actionable.
    fn report_open_failure(&mut self, path: &Path, error: crate::store::StoreError) {
        tracing::error!(event = "db.open.failed", path = %path.display(), reason = %error);
        let mut message = error.to_string();

        // A wrong password is the one failure that is worth slowing down, and the
        // message has to carry the wait — the operator is looking at this line,
        // not at the button they are about to find disabled.
        if error.is_wrong_password() {
            self.note_unlock_failure(path);
            if let Some(wait) = self.throttle.message() {
                message = format!("{message}. {wait}");
            }
        }

        self.show_open_failure(path, &error, message);
    }

    /// Put a failed open on the chooser, keeping a lock refusal actionable.
    ///
    /// Separate from [`Self::report_open_failure`] because the throttle rule is
    /// not the same on every path — the startup probe opens with no password on
    /// purpose and must not be counted — while *this* half is the same everywhere:
    /// whatever refused the open has to reach the screen, and a lock refusal has
    /// to reach it as a card rather than as a sentence.
    fn show_open_failure(
        &mut self,
        path: &Path,
        error: &crate::store::StoreError,
        message: String,
    ) {
        // A lock refusal is not the operator's mistake and is not fixed by
        // retyping the path: it names the workstation that has the register, and
        // offers the one action that can help when that workstation is gone.
        self.db_form.locked = match error {
            crate::store::StoreError::Lease(crate::store::LeaseError::Held { holder, stale }) => {
                Some(LockedDatabase {
                    path: path.to_path_buf(),
                    holder: holder.to_string(),
                    stale: *stale,
                    same_host: holder.is_same_host(),
                    break_confirmed: false,
                })
            }
            _ => None,
        };

        self.db_form.error = Some(message.clone());
        self.open_error = Some(message);
    }

    /// Set, change or remove the database password, then reopen the register.
    ///
    /// The operation itself is [`Store::change_password`] — export under the new
    /// key, verify the copy, swap — and this is the part around it: check what can
    /// be checked before anything moves, and put the application back into a
    /// sensible state afterwards.
    ///
    /// ## Why the guards are here and not left to the store
    ///
    /// [`Store::change_password`] consumes the `Store`, because after a swap the
    /// handle points at a file that is no longer the register. That is right for
    /// the operation and wrong for a refusal: a build without SQLCipher, or a
    /// read-only session, would have the register closed to be told "this build
    /// cannot do that". So both are answered here, while the store is still ours.
    ///
    /// ## What happens when the swap itself fails
    ///
    /// The register is untouched — that is the whole point of the export-and-swap
    /// order — but this session no longer has a handle on it, and the old password
    /// was never kept. So the chooser comes back with the reason, and the operator
    /// reopens with the password they already had. Deliberately not softened:
    /// pretending the database is still open would be the lie, not the extra
    /// unlock.
    pub fn change_database_password(&mut self, remove: bool) {
        let (new, confirm) = self.password_form.take();

        if !cfg!(feature = "encrypted-db") {
            self.password_form.error =
                Some(crate::store::StoreError::EncryptionUnavailable.to_string());
            return;
        }
        let Some(store) = self.store.as_ref() else {
            self.password_form.error = Some("no database is open".into());
            return;
        };
        if store.is_read_only() {
            self.password_form.error = Some(crate::store::StoreError::ReadOnly.to_string());
            return;
        }
        let was_encrypted = store.is_encrypted();
        if remove && !was_encrypted {
            self.password_form.error = Some("this register has no password to remove".into());
            return;
        }

        // Typed twice, and compared before anything is exported: the operator
        // would otherwise discover the typo at the next unlock, with no way back.
        let new_password = if remove {
            None
        } else {
            if new != confirm {
                self.password_form.error =
                    Some("the two passwords are not the same — type it again".into());
                return;
            }
            let assessment = crate::password::assess(&new);
            if !assessment.is_acceptable() {
                self.password_form.error = Some(assessment.summary());
                return;
            }
            Some(new)
        };

        // From here the store is consumed either way.
        let store = self.store.take().expect("checked just above");
        let operator = self.operator.clone();
        let now_encrypted = new_password.is_some();
        self.password_form.dismiss();

        match store.change_password(&operator, new_password.as_deref()) {
            Ok(path) => {
                // Reopening is what proves the new password works, and it is the
                // only way back to a usable session — the handle that did the swap
                // is gone.
                //
                // Deliberately **not** [`Self::open_database`]: that one begins by
                // releasing whatever is open, and releasing includes disconnecting
                // an SMB share this session connected. For a register *on* that
                // share it would pull the file out from under the reopen, and the
                // password change would read as a failure. Nothing needs releasing
                // here anyway — the store was consumed and its lock came off with
                // the swap.
                self.reopen_current(&path, new_password);
                if self.store.is_some() {
                    self.status = match (was_encrypted, now_encrypted) {
                        (true, true) => "the database password was changed".into(),
                        (false, true) => "the database is now password-protected".into(),
                        (_, false) => "the database password was removed — the file is no \
                                       longer encrypted"
                            .to_owned(),
                    };
                }
            }
            Err(e) => {
                tracing::error!(event = "db.rekey.failed", reason = %e);
                let message = format!(
                    "{e} — the register itself was not changed. Reopen it with the password it \
                     had."
                );
                self.open_error = Some(message.clone());
                self.db_form.error = Some(message);
                self.db_form.path = self.config.path.display().to_string();
            }
        }
    }

    /// Reopen the register this session is already pointed at, with a password.
    ///
    /// Used after a password change, and only there. Two things make it different
    /// from [`Self::open_database`]:
    ///
    /// * **It releases nothing.** There is nothing to release — the store was
    ///   consumed by the swap and its lock came off with it — and releasing would
    ///   disconnect an SMB share this session connected, which for a register on
    ///   that share is the file itself going away.
    /// * **It reuses `self.config`** rather than building a fresh one from the
    ///   path. That config carries the location as it was *stated* when the
    ///   register was opened — a share connector knows it is a share, where
    ///   [`crate::store::Location::detect`] can only guess from a mount point — so
    ///   rebuilding it risks reopening the same file under different pragmas.
    fn reopen_current(&mut self, path: &Path, password: Option<String>) {
        let config = self.config.clone().with_password(password);
        match Store::open_existing(&config) {
            Ok(store) => {
                self.adopt(store, config);
                self.record(
                    "app.opened",
                    "database",
                    &self.config.path.display().to_string(),
                );
                self.note_unlock_success();
                self.refresh();
            }
            Err(e) => self.report_open_failure(path, e),
        }
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
    ///
    /// An SMB share this session connected goes with it, in that order: both audit
    /// entries are written while there is still a database to write them to, then
    /// the connection closes, then the share is disconnected. Disconnecting first
    /// would pull the file out from under the close.
    fn release_current_database(&mut self) -> Option<crate::store::Settled> {
        // The audit entries have to be written while the connection is still open,
        // and they are the entries that record the lock and the share being given
        // up: both releases happen with no database left to write to.
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
        let share = self
            .share
            .as_ref()
            .filter(|share| share.is_ours())
            .map(|share| share.describe());
        if self.store.is_some() {
            if let Some(share) = share {
                self.record("db.share.disconnected", "database", &share);
            }
            self.record("db.closed", "database", held.as_deref().unwrap_or(""));
        }
        let settled = self.store.take().and_then(|store| store.close());
        self.release_current_share();
        settled
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
        // This register's own operator, when it has one, so the actor on every
        // audit entry from here on is whoever works in *this* register rather
        // than whoever last typed a name on this workstation
        // (`features/database-selection.md` phase 8).
        self.operator = self.settings.operator_for(&config.path);
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
    /// Notice a share that has gone away, and offer the way back
    /// (`features/smb-share-hosting.md` phase 9).
    ///
    /// Until now a dropped share surfaced as whatever SQLite error the next
    /// operation happened to hit — mid-hand-over, in the middle of recording a
    /// distribution — and the operator had to work out that the file server had gone
    /// and reconnect by hand. The register was never lost, but nothing said so.
    ///
    /// The check is **one `is_file`**, every [`SHARE_CHECK_EVERY`], and only for a
    /// register that is actually on a share. That is deliberately the cheapest thing
    /// that answers the question: a dropped mount makes the path stop resolving, and
    /// anything cleverer (a read, a pragma) would be an I/O operation on a
    /// filesystem that may be hanging.
    pub fn tick_share_health(&mut self) {
        // Only a register this session reached over a share can be dropped by a
        // share going away. A local file that vanished is a different problem with a
        // different answer, and one this function must not claim to fix.
        if self.store.is_none() || self.share.is_none() {
            return;
        }
        let now = std::time::Instant::now();
        if let Some(last) = self.share_checked
            && now.duration_since(last) < SHARE_CHECK_EVERY
        {
            return;
        }
        self.share_checked = Some(now);

        if self.config.path.is_file() {
            return;
        }
        self.handle_dropped_share();
    }

    /// The share is gone: let go of the register, then try to get it back.
    fn handle_dropped_share(&mut self) {
        let Some(share) = self.share.as_ref() else {
            return;
        };
        let target = share.target().clone();
        let identity = share.describe();
        let location = target.location();
        let access = self
            .settings
            .recent_shares
            .iter()
            .find(|entry| entry.location == location)
            .map(|entry| entry.access)
            .unwrap_or_default();
        let user = self
            .settings
            .recent_shares
            .iter()
            .find(|entry| entry.location == location)
            .map(|entry| entry.user.clone())
            .unwrap_or_default();

        tracing::error!(
            event = "db.share.dropped",
            share = %target.describe(),
            path = %self.config.path.display(),
        );

        // **Abandoned, not closed.** The polite close writes `db.closed` into the
        // register first, and the register is exactly what is no longer reachable —
        // the write would fail and report an audit failure for a fault that is not
        // one. There is nothing to record *in* the file about the file being gone;
        // the log line above is where this belongs.
        self.abandon_current_database();

        self.share_lost = Some(LostShare {
            location: location.clone(),
            identity: identity.clone(),
            access,
            user,
        });

        // A share reached as the signed-in user or as a guest needs no password, so
        // one attempt to get straight back on is free and is what an operator would
        // do anyway. A named account cannot be retried: the password was used once
        // and dropped, which is the whole point of never storing it.
        if access == crate::store::smb::Access::Named {
            self.open_error = Some(format!(
                "{location} is no longer reachable — the share went away. The register itself is \
                 on the file server and is intact. Reconnect below: the share and the user name \
                 are remembered, so only the password has to be typed again."
            ));
            self.fill_share_form(&location);
            return;
        }

        self.status = format!("{location} went away — reconnecting…");
        self.reconnect_dropped_share();
    }

    /// Try to reconnect the share this session lost, and reopen the register.
    ///
    /// Separate from [`Self::connect_share`] because it is not the operator filling
    /// in a card: the target and the identity come from what was connected, and
    /// failing leaves the card filled so they can take over.
    pub fn reconnect_dropped_share(&mut self) {
        use crate::store::smb::{Access, Credential, ShareConnection};

        let Some(lost) = self.share_lost.clone() else {
            return;
        };
        // Re-parsed from the canonical location the connection reported, which is
        // the same string the settings file remembers — so a reconnection reaches
        // the share by name rather than by the mount point the last connection
        // happened to land on.
        let target = match crate::store::smb::parse(&lost.location) {
            Ok(parsed) => parsed.target,
            Err(e) => {
                self.open_error = Some(format!("{} cannot be parsed again: {e}", lost.location));
                return;
            }
        };
        let credential = match lost.access {
            Access::LoggedOnUser => Credential::logged_on_user(),
            Access::Anonymous => Credential::anonymous(),
            // Reached only from the button, where the operator has just typed it.
            Access::Named => Credential::named(&lost.user, &self.share_form.take_password()),
        };

        match ShareConnection::open(&target, &credential, (self.share_connector)()) {
            Ok(connection) => {
                let database = connection.database_path();
                let config = connection
                    .store_config()
                    .with_operator(&self.operator)
                    .with_password(self.take_password());
                match Store::open_existing(&config) {
                    Ok(store) => {
                        self.adopt(store, config);
                        self.share = Some(connection);
                        self.share_lost = None;
                        self.share_checked = None;
                        // Audited on both sides of the gap: the connection, and that
                        // it was a *re*connection. A register that went away and came
                        // back is a fact about the session worth reading later.
                        self.record("db.share.connected", "database", &lost.identity);
                        self.record(
                            "db.share.reconnected",
                            "database",
                            &format!("share={} identity={}", lost.location, lost.identity),
                        );
                        self.record("app.opened", "database", &database.display().to_string());
                        self.note_unlock_success();
                        self.refresh();
                        self.status =
                            format!("{} is back — the register is open again", lost.location);
                    }
                    Err(crate::store::StoreError::Missing(path)) => {
                        // The connector said yes and the register is still not
                        // there. That is the share not being *fully* back — a mount
                        // that has reappeared before the server is serving it, which
                        // is what a flapping link looks like — so the message stays
                        // the one about the share rather than becoming "no database
                        // file at …", which would send the operator looking for a
                        // register they never moved.
                        drop(connection);
                        self.open_error = Some(format!(
                            "{} answered, but {} is not reachable on it yet. The register is on                              the file server and is intact — try again in a moment.",
                            lost.location,
                            path.display()
                        ));
                        self.fill_share_form(&lost.location);
                    }
                    Err(e) => {
                        // Any other refusal *is* about the register — a password, a
                        // schema from a newer build, another workstation's lock — and
                        // the operator gets the real reason rather than
                        // "reconnect failed".
                        drop(connection);
                        self.report_open_failure(&database, e);
                        self.fill_share_form(&lost.location);
                    }
                }
            }
            Err(e) => {
                tracing::warn!(event = "db.share.reconnect.failed", reason = %e);
                self.open_error = Some(format!(
                    "{} is still not reachable: {e}. The register is on the file server and is \
                     intact — reconnect below when the share is back.",
                    lost.location
                ));
                self.fill_share_form(&lost.location);
            }
        }
    }

    /// Let go of a register whose file is no longer reachable.
    ///
    /// The counterpart to [`Self::release_current_database`], for the one case that
    /// one cannot handle: there is no file to write the closing audit entry into. It
    /// records nothing, releases nothing on the share (the connection is as dead as
    /// the file), and clears the cached views so no screen shows rows from a register
    /// this session can no longer read.
    fn abandon_current_database(&mut self) {
        if let Some(store) = self.store.take() {
            // Dropped without `close()`: that path waits for a sync client and
            // removes a lock file, both of which need the filesystem that just went
            // away. On a share there is no lock file anyway.
            drop(store);
        }
        // The connection object goes too, without `close()` — disconnecting a mount
        // that is already gone is at best a no-op and at worst a hang.
        let _ = self.share.take();

        self.keys.clear();
        self.holders.clear();
        self.distributions.clear();
        self.runs.clear();
        self.audit_view.clear();
        self.templates.clear();
        self.template_catalogue.clear();
        self.document_counts.clear();
        self.filed_documents.clear();
        self.db_form.error = None;
        self.db_form.locked = None;
        self.db_form.path = self.config.path.display().to_string();
    }

    pub fn tick_lease(&mut self) {
        let operator = self.operator.clone();
        let Some(store) = self.store.as_mut() else {
            return;
        };
        // Same tick, different obligation: the lease is what *stops* a second
        // workstation, and this is only what lets one be named. A failure to say
        // "I am here" is logged and otherwise ignored — it costs a banner on
        // somebody else's screen, not the session.
        if let Err(e) = store.renew_presence(&operator) {
            tracing::warn!(event = "db.presence.renew_failed", reason = %e);
        }
        // Read back on the same tick: a session that goes quiet has to *leave*
        // the banner without anybody pressing Refresh, or the warning outlives
        // the thing it warns about.
        match store.unfinished_batches() {
            Ok(batches) => self.batch.resumable = batches,
            // Not fatal: a batch that cannot be listed is one nobody is offered
            // to resume, which is a lesser failure than refusing to refresh.
            Err(e) => tracing::warn!(event = "batch.read.failed", reason = %e),
        }
        match store.presence() {
            Ok(presence) => self.presence = presence,
            Err(e) => tracing::warn!(event = "db.presence.read_failed", reason = %e),
        }
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
        // With a register open, the name belongs to *that* register
        // (`features/database-selection.md` phase 8): the workstation's default is
        // what a register nobody has named an operator for uses, and overwriting
        // it here is how one name leaked onto every other register on the machine.
        if self.store.is_some() {
            let path = self.config.path.clone();
            let operator = self.operator.clone();
            self.settings.remember_operator(&path, &operator);
        } else {
            self.settings.operator = self.operator.clone();
        }
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
        // The record as it was painted, which is what the optimistic check is
        // against: `seen` has to be the copy the operator typed over, not a fresh
        // read, or the check would always pass and never protect anything.
        let Some(seen) = self.keys.iter().find(|key| key.serial == serial).cloned() else {
            self.inventory.error = Some(format!("serial {serial} is no longer on the register"));
            return;
        };
        let before = seen.notes.clone();

        let Some(store) = &self.store else { return };
        if let Err(e) = store.set_key_notes(serial, &note, seen.updated_at) {
            let conflict = matches!(e, crate::store::StoreError::Conflict { .. });
            self.inventory.error = Some(e.to_string());
            tracing::warn!(event = "key.note.save.failed", serial, reason = %e);
            if conflict {
                // Audited, because a refused write is a thing that happened to the
                // register: it is how anybody later finds out that two operators
                // were working on the same key at the same time.
                self.record(
                    "db.conflict",
                    &format!("serial:{serial}"),
                    "field=notes outcome=refused",
                );
                // And the screen is brought up to date, so the operator can see
                // what they would have overwritten before typing it again.
                self.refresh();
            }
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
        // A reset asked for and not answered is closed rather than left behind
        // this one: two destructive confirmations on one screen is one too many.
        self.reset.serial = None;
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

    /// Ask to return a plugged key to factory default. **Nothing is written
    /// here** (`features/key-lifecycle-and-revocation.md` phase 5).
    ///
    /// Opening the panel reads the applets, which is a read and only a read —
    /// AGENTS.md forbids a hardware write as a side effect of opening a screen,
    /// and this is the screen that would be worst to break that on. The read is
    /// what lets the confirmation name *this* key's loss ("slot 9c holds a
    /// certificate") instead of a generic one, and it is recorded for the same
    /// reason the pre-flight records its own: a destructive act justified by what
    /// the tool saw should leave behind what the tool saw.
    pub fn request_key_reset(&mut self, serial: u32) {
        // The two confirmations are mutually exclusive by construction: whichever
        // is asked for last closes the other, so no frame can show both.
        self.inventory.pending_removal = None;
        self.inventory.note_serial = None;
        // And a power cycle asked for on another key is over: the applets it
        // carries were confirmed for that serial, not this one.
        self.abandon_power_cycle("another key's reset was opened");

        self.reset.serial = Some(serial);
        self.reset.applets = crate::device::reset::Applet::ALL.to_vec();
        self.reset.typed.clear();
        self.reset.outcomes.clear();
        self.reset.error = None;
        self.reset.observed = self.read_applets(serial);
        self.status = format!("serial {serial}: nothing has been written — read the preview");
    }

    /// Read one key's applets, record what was read, and remember it for the
    /// screens.
    ///
    /// **A read and only a read** — `get_info`, the PIV slot list, the retry
    /// counters, the management applet's device info, `ykman otp info`. That is what
    /// makes it safe to call from a button the operator pressed, and `AGENTS.md`
    /// forbids anything stronger happening as a side effect of a screen.
    ///
    /// One entry point rather than three, because the audit entry belongs to the
    /// read: the reset preview and the wizard's pre-flight both used to write their
    /// own, and a third caller would have written a fourth or forgotten.
    pub fn read_applets(&mut self, serial: u32) -> crate::device::applets::Snapshot {
        let snapshot = crate::device::applets::read(serial, &self.transport);
        // Recorded because refusals downstream rest on it, and "what did the tool
        // see" is the first question when one is disputed. States, slots and counts
        // only — never a secret.
        if !snapshot.is_empty() {
            self.record(
                "device.applets.read",
                &serial.to_string(),
                &snapshot.describe().join(" | "),
            );
        }
        self.applet_reads.insert(serial, snapshot.clone());
        snapshot
    }

    /// The factory defaults a key is known to still carry, or `None` when its
    /// applets have not been read this session
    /// (`features/step-piv-pin-puk-management-key.md` phase 6).
    ///
    /// `None` and "no defaults" are different answers and the screen renders them
    /// differently: a key nobody has looked at gets an invitation to look, not a
    /// clean bill of health.
    pub fn factory_default_badge(&self, serial: u32) -> Option<Option<String>> {
        self.applet_reads
            .get(&serial)
            .map(|snapshot| snapshot.factory_default_badge())
    }

    /// Abandon a reset the operator asked about and then declined.
    pub fn cancel_key_reset(&mut self) {
        self.abandon_power_cycle("the operator cancelled");
        self.reset.serial = None;
        self.reset.typed.clear();
        self.reset.error = None;
        self.status = "nothing was written".into();
    }

    /// Tick or untick one applet.
    ///
    /// The list is kept in [`crate::device::reset::Applet::ALL`] order rather
    /// than click order, because that order is the order the hardware calls go
    /// out in and the preview should read as the run will happen.
    pub fn toggle_reset_applet(&mut self, applet: crate::device::reset::Applet, wanted: bool) {
        self.reset.applets.retain(|a| *a != applet);
        if wanted {
            self.reset.applets.push(applet);
        }
        self.reset.applets = crate::device::reset::Applet::ALL
            .iter()
            .copied()
            .filter(|a| self.reset.applets.contains(a))
            .collect();
    }

    /// The preview: what each applet's reset would destroy, and how.
    ///
    /// **All three, whatever is ticked.** The preview is how an operator decides
    /// what to tick, and one that showed only the current selection would hide
    /// what unticking an applet spares — which is the comparison they are making.
    pub fn reset_plan(&self) -> Vec<crate::device::reset::PlanItem> {
        crate::device::reset::plan(
            &crate::device::reset::Applet::ALL,
            &self.reset.observed,
            &self.transport,
        )
    }

    /// May the reset button be enabled?
    ///
    /// Two conditions, both the operator's: something is selected, and the serial
    /// has been typed back. The second is not ceremony — the panel is opened from
    /// a row in a table of attached keys, and re-typing the number is what
    /// distinguishes "reset this key" from "click landed on the wrong row".
    pub fn reset_is_confirmable(&self) -> bool {
        let Some(serial) = self.reset.serial else {
            return false;
        };
        !self.reset.applets.is_empty() && self.reset.typed.trim() == serial.to_string()
    }

    /// Take the operator's confirmation, and either run the reset or ask for the
    /// power cycle a FIDO2 reset cannot do without.
    ///
    /// **Still nothing written here when FIDO2 is ticked.** CTAP accepts
    /// `authenticatorReset` only in the first seconds after the authenticator
    /// powers up, so a key that has been in the port since the preview opened is
    /// always out of time — the tool used to ask the operator to win that race by
    /// hand and then report `ykman`'s refusal when they lost it. Instead the
    /// confirmation is taken now, frozen with its applets, and
    /// [`crate::device::reinsert`] walks the operator through pulling the key out
    /// and putting it back, firing the run the moment it returns.
    pub fn confirm_key_reset(&mut self) {
        use crate::device::reinsert;

        self.reset.error = None;
        let Some(serial) = self.reset.serial else {
            return;
        };
        if !self.reset_is_confirmable() {
            self.reset.error = Some(
                "type the serial back and choose at least one applet — nothing was written".into(),
            );
            return;
        }

        let applets = self.reset.applets.clone();
        if reinsert::needed(&applets) {
            self.begin_power_cycle(serial, &applets);
            return;
        }
        self.run_confirmed_reset(serial, &applets);
    }

    /// Ask for the key to be re-inserted, and start watching the port for it.
    ///
    /// The device watch is stopped first and stays stopped for the handshake:
    /// [`Self::sync_device_watch`] will not bring it back while one is live. Two
    /// enumerations of the same reader, one of them a `ykman` subprocess, is not
    /// what to spend a five-second window on.
    fn begin_power_cycle(&mut self, serial: u32, applets: &[crate::device::reset::Applet]) {
        use crate::device::reinsert::{Handshake, PresenceWatch, poll_for};

        self.stop_device_watch("a factory reset is arming");

        let native = self.transport.transport == crate::device::Transport::Native;
        let poll = poll_for(native);
        let handshake = Handshake::start(serial, applets, poll, std::time::Instant::now());
        let (event, detail) = handshake.requested();
        self.record(event, &format!("serial:{serial}"), &detail);

        self.reset.presence_seen = crate::device::reinsert::Presence::default();
        self.reset.presence = Some(PresenceWatch::start(
            crate::device::select::backend_for(&self.transport),
            serial,
            poll,
        ));
        self.reset.handshake = Some(handshake);
        self.status = format!(
            "serial {serial}: pull the key out and plug it back in — nothing has been written"
        );
    }

    /// Drive a live handshake, once per frame.
    ///
    /// Thin on purpose: every decision belongs to
    /// [`crate::device::reinsert::Handshake`], where it is unit tested against
    /// synthetic instants rather than against a key and a stopwatch.
    pub fn tick_power_cycle(&mut self) {
        if self.tab != Tab::Inventory {
            // The panel is the only place this is visible or cancellable, so a
            // handshake left running behind another screen would poll a port for a
            // step nobody can see. Nothing was written; the confirmation goes with it.
            self.abandon_power_cycle("the screen showing the reset was left");
            return;
        }

        let Some(handshake) = &mut self.reset.handshake else {
            return;
        };
        if let Some(watch) = &self.reset.presence {
            self.reset.presence_seen = watch.snapshot();
        }

        let present = self.reset.presence_seen.present;
        let reaction = handshake.observe(present, std::time::Instant::now());
        self.settle_power_cycle(reaction);
    }

    /// Act on one reaction: record it, and fire or end the handshake.
    fn settle_power_cycle(&mut self, reaction: crate::device::reinsert::Reaction) {
        use crate::device::reinsert::Reaction;

        let Some(handshake) = &self.reset.handshake else {
            return;
        };
        let serial = handshake.serial();
        let applets = handshake.applets().to_vec();
        let entry = handshake.audit_for(reaction);
        let target = format!("serial:{serial}");
        if let Some((event, detail)) = entry {
            self.record(event, &target, &detail);
        }

        match reaction {
            Reaction::Wait => {}
            Reaction::Fire => {
                // The port is released before the reset goes out, and the
                // handshake with it: `ykman fido reset` must not race an
                // enumeration of the same reader inside the window it is racing.
                self.reset.presence = None;
                self.reset.handshake = None;
                self.run_confirmed_reset(serial, &applets);
            }
            Reaction::Expired => {
                self.reset.error = Some(
                    "the key was back in the port for longer than the applet's window, so the \
                     reset was not sent — nothing was written. Pull the key out and plug it \
                     back in to try again."
                        .into(),
                );
                self.status =
                    format!("serial {serial}: the power-up window closed — nothing written");
            }
            Reaction::GaveUp => {
                self.reset.error = Some(
                    "the key was neither pulled out nor plugged back in, so the reset was \
                     abandoned — nothing was written."
                        .into(),
                );
                self.status = format!("serial {serial}: the power cycle was abandoned");
            }
        }
    }

    /// Ask again, after a window that closed or an operator who stepped away.
    ///
    /// The selection is the one already confirmed and carried by the handshake —
    /// a retry is another chance at the same agreement, never a new one.
    pub fn restart_power_cycle(&mut self) {
        let Some(handshake) = &mut self.reset.handshake else {
            return;
        };
        handshake.restart(std::time::Instant::now());
        let (event, detail) = handshake.requested();
        let target = format!("serial:{}", handshake.serial());
        self.reset.error = None;
        self.record(event, &target, &detail);
        self.reset.presence_seen = crate::device::reinsert::Presence::default();
        self.status = "pull the key out and plug it back in — nothing has been written".into();
    }

    /// Send the reset on the operator's word rather than on a poll.
    ///
    /// For a workstation whose enumeration is slower than the window: the operator
    /// has the key in their hand and can see it is back before this can.
    pub fn arm_power_cycle_now(&mut self) {
        let Some(handshake) = &mut self.reset.handshake else {
            return;
        };
        let reaction = handshake.arm_now(std::time::Instant::now());
        self.settle_power_cycle(reaction);
    }

    /// Drop a handshake without writing anything, recording that it was dropped.
    fn abandon_power_cycle(&mut self, why: &str) {
        let Some(handshake) = self.reset.handshake.take() else {
            return;
        };
        self.reset.presence = None;
        self.reset.presence_seen = crate::device::reinsert::Presence::default();
        if !handshake.is_finished() {
            let (event, detail) = handshake.cancelled();
            let target = format!("serial:{}", handshake.serial());
            self.record(event, &target, &detail);
        }
        tracing::debug!(event = "key.reset.power_cycle.dropped", reason = why);
    }

    /// Power-cycle the key and try the FIDO2 reset again, after one that refused.
    ///
    /// The button under an outcome table where FIDO2 says *refused* — which is
    /// where the timing failure lands when the window closes anyway. Only FIDO2 is
    /// retried: the other applets either succeeded or have their own row, and
    /// resetting a slot twice because the operator wanted one more attempt at
    /// something else is not a thing this panel may do.
    pub fn retry_fido2_reset(&mut self) {
        use crate::device::reset::Applet;

        let Some(serial) = self.reset.serial else {
            return;
        };
        self.reset.error = None;
        self.reset.applets = vec![Applet::Fido2];
        self.reset.outcomes.clear();
        self.begin_power_cycle(serial, &[Applet::Fido2]);
    }

    /// Run the confirmed reset against the attached key.
    ///
    /// The confirmation is built here, from the applets the operator agreed to —
    /// either in the same click that called this, or frozen into the handshake
    /// when they did — and re-checked inside the engine against the request.
    fn run_confirmed_reset(&mut self, serial: u32, applets: &[crate::device::reset::Applet]) {
        use crate::device::reset::{self, Confirmation, HardwareResetter, Request};

        // Nothing else touches the key while this runs, for the reason the
        // executor stops it too: enumerating readers while another handle holds
        // the card is not a thing to discover half way through a reset. The next
        // frame restarts it.
        self.stop_device_watch("a factory reset is about to write to a key");

        let applets = applets.to_vec();
        let request = Request::new(serial, &applets, &self.operator);
        let confirmation = Confirmation::given(serial, &applets);
        let mut resetter = HardwareResetter::for_key(serial, &self.transport);

        let outcome = {
            let Some(store) = self.store.take() else {
                self.reset.error = Some("no database is open, so nothing was written".into());
                return;
            };
            let mut recorder = StoreRecorder {
                store,
                operator: self.operator.clone(),
                failure: None,
            };
            let result = reset::perform(&request, &confirmation, &mut resetter, &mut recorder);
            let failure = recorder.failure.clone();
            self.store = Some(recorder.store);
            (result, failure)
        };

        match outcome {
            (Ok(outcomes), failure) => {
                let done = outcomes
                    .iter()
                    .filter(|o| o.status == reset::Status::Done)
                    .count();
                let failed = outcomes
                    .iter()
                    .filter(|o| o.status == reset::Status::Failed)
                    .count();
                self.status = if failed == 0 {
                    format!("serial {serial}: {done} applet(s) returned to factory default")
                } else {
                    format!(
                        "serial {serial}: {done} applet(s) reset, {failed} refused — read the \
                         detail below"
                    )
                };
                tracing::warn!(event = "key.reset", serial, done, failed);
                self.reset.outcomes = outcomes;
                // What the reset achieved is also what clears the reissue gate
                // (`features/key-lifecycle-and-revocation.md` phase 6): the
                // applets it returned to factory default carry nothing of the
                // previous holder's, and until that is on record the store will
                // refuse to put this key back into stock.
                self.record_reset_sanitisation(serial);
                // Re-read, so the panel shows the key as it is now rather than as
                // it was when the operator opened the panel. A reset that claims
                // to have worked and a key that still holds a certificate is
                // exactly the disagreement this makes visible.
                self.reset.observed = crate::device::applets::read(serial, &self.transport);
                if let Some(failure) = failure {
                    self.reset.error = Some(format!("AUDIT FAILURE: {failure}"));
                }
            }
            (Err(e), _) => {
                tracing::error!(event = "key.reset.refused", serial, reason = %e);
                self.reset.error = Some(e.to_string());
                self.status = format!("refused: {e}");
            }
        }
        self.refresh();
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

    // ------------------------------------------------------------- lifecycle
    //
    // What happens to a key after the hand-over
    // (`features/key-lifecycle-and-revocation.md`). Every method here follows the
    // same three rules the rest of this file does: the store refuses before
    // anything is written, the refusal is shown rather than swallowed, and a write
    // that succeeded is audited before the panel says so.

    /// Open the lifecycle panel for one key, reading what the register holds.
    pub fn open_key_lifecycle(&mut self, serial: u32) {
        // One panel at a time on this screen: the other two are a destructive
        // reset and a row deletion, and none of the three should be answerable
        // while another is open.
        self.inventory.note_serial = None;
        self.inventory.pending_removal = None;
        self.reset.serial = None;

        let holder = self.holder_display_for(serial);
        self.lifecycle = LifecyclePanel {
            serial: Some(serial),
            reported_by: holder,
            ..LifecyclePanel::default()
        };
        self.reload_lifecycle();
    }

    pub fn close_key_lifecycle(&mut self) {
        self.lifecycle = LifecyclePanel::default();
    }

    /// Re-read the five things the panel shows. Called on open and after a write.
    fn reload_lifecycle(&mut self) {
        let Some(serial) = self.lifecycle.serial else {
            return;
        };
        let Some(store) = &self.store else {
            self.lifecycle.error = Some("no database is open".into());
            return;
        };
        let read = (|| {
            Ok::<_, crate::store::StoreError>((
                store.incidents_for(serial)?,
                store.remediations_for(serial)?,
                store.dependencies_for(serial)?,
                store.rma_cases_for(serial)?,
                store.sanitisation_for(serial)?,
            ))
        })();
        match read {
            Ok((incidents, remediations, dependencies, rma, sanitisation)) => {
                self.lifecycle.incidents = incidents;
                self.lifecycle.remediations = remediations;
                self.lifecycle.dependencies = dependencies;
                self.lifecycle.rma = rma;
                self.lifecycle.sanitisation = sanitisation;
            }
            Err(e) => {
                tracing::error!(event = "key.lifecycle.read.failed", serial, reason = %e);
                self.lifecycle.error = Some(e.to_string());
            }
        }
    }

    /// The holder this key was last handed to, as the register named them.
    ///
    /// The hand-over is the source rather than the holder table, because the
    /// question is *who had this key*, and a key handed over twice has had two
    /// holders. Empty when it has never been handed over — a key lost from the
    /// drawer is a real case, and inventing a holder for it would be worse than
    /// saying nothing.
    pub fn holder_display_for(&self, serial: u32) -> String {
        self.distributions
            .iter()
            .filter(|record| record.key_serial == serial)
            .max_by_key(|record| record.distributed_at)
            .map(|record| record.holder_display.clone())
            .unwrap_or_default()
    }

    /// The open incident for this key, if there is one.
    pub fn open_incident(&self) -> Option<&crate::domain::KeyIncident> {
        self.lifecycle.incidents.iter().find(|i| i.is_open())
    }

    /// Record a loss or theft, and move the key to `Lost`
    /// (`features/key-lifecycle-and-revocation.md` phase 2).
    ///
    /// The report and the status change are one store operation, so a key can
    /// never be `Lost` with nothing saying why, or carry a report while the
    /// register still says it is in somebody's hands.
    pub fn report_key_incident(&mut self) {
        use crate::domain::KeyIncident;
        use crate::domain::lifecycle::parse_report_date;

        self.lifecycle.error = None;
        self.lifecycle.notice = None;
        let Some(serial) = self.lifecycle.serial else {
            return;
        };

        let reported_at = match parse_report_date(&self.lifecycle.report_date, chrono::Utc::now()) {
            Ok(at) => at,
            Err(e) => {
                self.lifecycle.error = Some(e);
                return;
            }
        };
        let holder = self.holder_display_for(serial);
        let incident = match KeyIncident::new(
            serial,
            self.lifecycle.report_kind,
            reported_at,
            &self.lifecycle.reported_by,
            &holder,
            &self.lifecycle.circumstances,
            &self.operator,
        ) {
            Ok(incident) => incident,
            Err(e) => {
                self.lifecycle.error = Some(e.to_string());
                return;
            }
        };

        let Some(store) = &self.store else {
            self.lifecycle.error = Some("no database is open".into());
            return;
        };
        if let Err(e) = store.report_incident(&incident) {
            tracing::warn!(event = "key.incident.refused", serial, reason = %e);
            self.lifecycle.error = Some(e.to_string());
            return;
        }

        // One event for both kinds, with the kind in the detail: the trail is
        // filtered by event name, and "show me the keys that went missing" is one
        // question rather than two.
        self.record(
            "key.reported_lost",
            &format!("serial:{serial}"),
            &incident.audit_detail(),
        );
        // The certificate this key carried is the reason the reason field exists:
        // a key nobody can produce is a compromised key until somebody says
        // otherwise.
        self.lifecycle.revocation_reason =
            crate::domain::RevocationReason::for_incident(incident.kind);
        self.status = format!(
            "serial {serial} recorded as {} — {} still to deal with",
            incident.kind.label().to_lowercase(),
            crate::incident::summarise(&self.lifecycle.dependencies, &self.lifecycle.remediations)
        );
        self.lifecycle.report_open = false;
        self.lifecycle.circumstances.clear();
        self.lifecycle.report_date.clear();
        self.reload_lifecycle();
        self.refresh();
    }

    /// Start recording that one dependency has been dealt with.
    pub fn settle_dependency(&mut self, dependency: &Dependency) {
        self.lifecycle.error = None;
        self.lifecycle.notice = None;
        self.lifecycle.reference.clear();
        self.lifecycle.detail.clear();
        self.lifecycle.settling = Some(dependency.clone());
    }

    pub fn cancel_settling(&mut self) {
        self.lifecycle.settling = None;
        self.lifecycle.error = None;
    }

    /// Record the revocation or the removal the operator has just performed
    /// elsewhere (phases 3 and 4).
    ///
    /// "Elsewhere" is the whole shape of this: the CA that issued the certificate
    /// and the relying party that holds the credential are somebody else's
    /// systems, so what this tool can do is know *what* has to be dealt with, and
    /// hold the reference that proves it was. See
    /// [`crate::domain::lifecycle`] for why that is the honest design rather than
    /// a missing feature.
    pub fn record_remediation(&mut self) {
        use crate::domain::lifecycle::{DependencyKind, Remediation};

        self.lifecycle.error = None;
        let Some(serial) = self.lifecycle.serial else {
            return;
        };
        let Some(dependency) = self.lifecycle.settling.clone() else {
            return;
        };
        let incident = self.open_incident().map(|incident| incident.id);

        let built = match dependency.kind {
            DependencyKind::Certificate => Remediation::certificate_revoked(
                serial,
                incident,
                &dependency.subject,
                self.lifecycle.revocation_reason,
                &self.lifecycle.reference,
                &self.operator,
                &self.lifecycle.detail,
            ),
            DependencyKind::Credential => Remediation::credential_removed(
                serial,
                incident,
                &dependency.subject,
                dependency
                    .detail
                    .trim_start_matches("relying party ")
                    .trim(),
                &self.lifecycle.reference,
                &self.operator,
            ),
            // Neither is anybody's ticket, and the panel offers no button for
            // them; reached only if one is ever added without this arm.
            DependencyKind::OtpAccessCode | DependencyKind::Custody => {
                self.lifecycle.error = Some(
                    "this entry is recorded for information — there is nothing to close".into(),
                );
                return;
            }
        };
        let remediation = match built {
            Ok(remediation) => remediation,
            Err(e) => {
                self.lifecycle.error = Some(e.to_string());
                return;
            }
        };

        let Some(store) = &self.store else {
            self.lifecycle.error = Some("no database is open".into());
            return;
        };
        if let Err(e) = store.insert_remediation(&remediation) {
            tracing::warn!(event = "key.remediation.refused", serial, reason = %e);
            self.lifecycle.error = Some(e.to_string());
            return;
        }
        self.record(
            remediation.kind.audit_event(),
            &format!("serial:{serial}"),
            &remediation.audit_detail(),
        );
        self.status = format!("serial {serial}: {} recorded", remediation.kind.label());
        self.lifecycle.settling = None;
        self.lifecycle.reference.clear();
        self.lifecycle.detail.clear();
        self.reload_lifecycle();
    }

    /// Record that applets were returned to factory default **outside** this tool
    /// (phase 6).
    ///
    /// The counterpart of *mark bootstrapped* on the same screen, and it exists
    /// for the same reason: the register has to be able to say what is true about
    /// a key somebody handled with `ykman` on a bench. The reference field is
    /// where they say how they know — because this one claim, unlike a reset this
    /// tool performed, rests entirely on the operator's word.
    pub fn record_manual_sanitisation(&mut self) {
        use crate::domain::lifecycle::Remediation;

        self.lifecycle.error = None;
        let Some(serial) = self.lifecycle.serial else {
            return;
        };
        if self.lifecycle.sanitised_applets.is_empty() {
            self.lifecycle.error =
                Some("choose the applets that were reset — nothing was recorded".into());
            return;
        }

        let applets = self.lifecycle.sanitised_applets.clone();
        let remediation = match Remediation::sanitised(
            serial,
            &applets,
            &self.lifecycle.reference,
            &self.operator,
            "recorded by the operator; this tool did not perform the reset",
        ) {
            Ok(remediation) => remediation,
            Err(e) => {
                self.lifecycle.error = Some(e.to_string());
                return;
            }
        };

        let Some(store) = &self.store else {
            self.lifecycle.error = Some("no database is open".into());
            return;
        };
        if let Err(e) = store.insert_remediation(&remediation) {
            self.lifecycle.error = Some(e.to_string());
            return;
        }
        self.record(
            remediation.kind.audit_event(),
            &format!("serial:{serial}"),
            &format!("{} source=operator", remediation.audit_detail()),
        );
        self.status = format!(
            "serial {serial}: {} recorded as sanitised",
            crate::device::reset::describe(&applets)
        );
        self.lifecycle.sanitised_open = false;
        self.lifecycle.sanitised_applets.clear();
        self.lifecycle.reference.clear();
        self.reload_lifecycle();
    }

    /// Record the sanitisation a reset this tool performed has just achieved.
    ///
    /// Called from [`Self::run_confirmed_reset`] rather than by the operator,
    /// because the reset is the evidence: an applet the transport reported as
    /// reset — or as already at factory default — is an applet nothing of the
    /// previous holder's is on. A failed applet is not recorded, which is why this
    /// reads the outcomes rather than the request.
    ///
    /// Public so a behaviour test can drive it from a set of outcomes: the reset
    /// that produces them needs a plugged-in key, and the rule this enforces — a
    /// reset is what clears the gate — is one no test should have to take on trust.
    pub fn record_reset_sanitisation(&mut self, serial: u32) {
        use crate::domain::lifecycle::{Remediation, cleared_by};

        let cleared = cleared_by(&self.reset.outcomes);
        if cleared.is_empty() {
            return;
        }
        let Ok(remediation) = Remediation::sanitised(
            serial,
            &cleared,
            "factory reset by this tool",
            &self.operator,
            "recorded from the reset's own outcomes",
        ) else {
            return;
        };

        let Some(store) = &self.store else { return };
        match store.insert_remediation(&remediation) {
            Ok(()) => {
                let detail = remediation.audit_detail();
                self.record(
                    remediation.kind.audit_event(),
                    &format!("serial:{serial}"),
                    &format!("{detail} source=reset"),
                );
            }
            Err(e) => {
                // Loud, not silent: the key is clean and the register cannot say
                // so, which is the state the reissue gate will refuse in.
                tracing::error!(event = "key.sanitised.record.failed", serial, reason = %e);
                self.reset.error = Some(format!(
                    "the reset ran, and the register could not record the sanitisation: {e}. \
                     Record it by hand from *Lifecycle…* before this key is reissued"
                ));
            }
        }
        if self.lifecycle.serial == Some(serial) {
            self.reload_lifecycle();
        }
    }

    /// Close an incident once nothing is outstanding — or say why it is being
    /// closed anyway.
    pub fn close_key_incident(&mut self, id: uuid::Uuid) {
        self.lifecycle.error = None;
        let Some(serial) = self.lifecycle.serial else {
            return;
        };
        let settled =
            crate::incident::is_settled(&self.lifecycle.dependencies, &self.lifecycle.remediations);
        let note = self.lifecycle.detail.trim().to_owned();
        if !settled && note.is_empty() {
            self.lifecycle.error = Some(
                "something on this key has not been dealt with. Record what was done — or write \
                 in the note why it is being closed without it, so the gap is visible rather \
                 than quiet"
                    .into(),
            );
            return;
        }

        let Some(store) = &self.store else {
            self.lifecycle.error = Some("no database is open".into());
            return;
        };
        match store.close_incident(id, &note) {
            Ok(incident) => {
                self.record(
                    "key.incident_closed",
                    &format!("serial:{serial}"),
                    &format!(
                        "kind={} outstanding={} note_chars={}",
                        incident.kind.audit_name(),
                        crate::domain::lifecycle::outstanding(
                            &self.lifecycle.dependencies,
                            &self.lifecycle.remediations
                        )
                        .len(),
                        note.chars().count()
                    ),
                );
                self.status = format!("serial {serial}: incident closed");
                self.lifecycle.detail.clear();
                self.reload_lifecycle();
            }
            Err(e) => self.lifecycle.error = Some(e.to_string()),
        }
    }

    /// Produce the incident note for the ESI (phase 7).
    ///
    /// Held in the panel so the operator can read it before it goes anywhere, and
    /// audited as an export the moment it is produced: the note carries the
    /// holder's name and what was on their key, so a copy leaving the tool is an
    /// event the register should hold.
    pub fn generate_incident_note(&mut self, id: uuid::Uuid) {
        let Some(serial) = self.lifecycle.serial else {
            return;
        };
        let Some(incident) = self
            .lifecycle
            .incidents
            .iter()
            .find(|incident| incident.id == id)
            .cloned()
        else {
            return;
        };

        let key = self.keys.iter().find(|key| key.serial == serial).cloned();
        let holder = self
            .distributions
            .iter()
            .filter(|record| record.key_serial == serial)
            .max_by_key(|record| record.distributed_at)
            .and_then(|record| {
                self.holders
                    .iter()
                    .find(|holder| holder.id == record.holder_id)
            })
            .cloned();

        let request = crate::incident::NoteRequest {
            incident: &incident,
            key: key.as_ref(),
            holder: holder.as_ref(),
            dependencies: &self.lifecycle.dependencies,
            remediations: &self.lifecycle.remediations,
            organisation: &self.org,
            prepared_by: &self.operator,
            report_to: &self.settings.report_incidents_to,
            prepared_at: chrono::Utc::now(),
        };
        let text = crate::incident::text(&request);
        self.record(
            "key.incident_note",
            &format!("serial:{serial}"),
            &format!(
                "kind={} outstanding={} format=text",
                incident.kind.audit_name(),
                crate::domain::lifecycle::outstanding(
                    &self.lifecycle.dependencies,
                    &self.lifecycle.remediations
                )
                .len()
            ),
        );
        self.lifecycle.note = Some((id, text));
        self.status = format!("serial {serial}: incident note prepared");
    }

    /// Write the note the panel is showing to a file, as text or as a PDF.
    pub fn save_incident_note(&mut self, as_pdf: bool) {
        let Some(serial) = self.lifecycle.serial else {
            return;
        };
        let Some((id, text)) = self.lifecycle.note.clone() else {
            self.lifecycle.error = Some("prepare the note first".into());
            return;
        };
        let Some(incident) = self
            .lifecycle
            .incidents
            .iter()
            .find(|incident| incident.id == id)
            .cloned()
        else {
            return;
        };

        let (bytes, extension) = if as_pdf {
            let key = self.keys.iter().find(|key| key.serial == serial).cloned();
            let request = crate::incident::NoteRequest {
                incident: &incident,
                key: key.as_ref(),
                holder: None,
                dependencies: &self.lifecycle.dependencies,
                remediations: &self.lifecycle.remediations,
                organisation: &self.org,
                prepared_by: &self.operator,
                report_to: &self.settings.report_incidents_to,
                prepared_at: chrono::Utc::now(),
            };
            (
                crate::pdf::render(&crate::incident::document(&request)),
                "pdf",
            )
        } else {
            (text.into_bytes(), "txt")
        };

        let suggested = format!("{}.{extension}", crate::incident::filename(&incident));
        if let Some(path) = self.save_bytes(&suggested, &bytes) {
            self.record(
                "key.incident_note",
                &format!("serial:{serial}"),
                &format!("format={extension} bytes={}", bytes.len()),
            );
            self.status = format!("incident note written to {}", path.display());
            self.lifecycle.notice = Some(format!(
                "written to {} — it names the holder and what was on their key, so treat it as \
                 the record it is",
                path.display()
            ));
        }
    }

    /// Send a key to the supplier, opening an RMA case (phase 8).
    pub fn send_key_for_rma(&mut self) {
        use crate::domain::RmaCase;

        self.lifecycle.error = None;
        let Some(serial) = self.lifecycle.serial else {
            return;
        };
        let case = match RmaCase::open(
            serial,
            &self.lifecycle.rma_reference,
            &self.lifecycle.rma_fault,
            chrono::Utc::now(),
            &self.operator,
        ) {
            Ok(case) => case,
            Err(e) => {
                self.lifecycle.error = Some(e.to_string());
                return;
            }
        };

        let Some(store) = &self.store else {
            self.lifecycle.error = Some("no database is open".into());
            return;
        };
        if let Err(e) = store.insert_rma(&case) {
            self.lifecycle.error = Some(e.to_string());
            return;
        }
        self.record(
            "key.rma.sent",
            &format!("serial:{serial}"),
            &case.audit_detail(),
        );
        self.status = format!("serial {serial}: RMA {} opened", case.reference);
        self.lifecycle.rma_open = false;
        self.lifecycle.rma_reference.clear();
        self.lifecycle.rma_fault.clear();
        self.reload_lifecycle();
    }

    /// Link the replacement the supplier sent back.
    pub fn record_rma_replacement(&mut self, id: uuid::Uuid) {
        self.lifecycle.error = None;
        let Some(serial) = self.lifecycle.serial else {
            return;
        };
        let typed = self.lifecycle.rma_replacement.trim().to_owned();
        let Ok(replacement) = typed.parse::<u32>() else {
            self.lifecycle.error = Some(format!(
                "`{typed}` is not a serial — type the replacement's number"
            ));
            return;
        };

        let Some(store) = &self.store else {
            self.lifecycle.error = Some("no database is open".into());
            return;
        };
        match store.link_rma_replacement(id, replacement) {
            Ok(case) => {
                self.record(
                    "key.rma.replaced",
                    &format!("serial:{serial}"),
                    &case.audit_detail(),
                );
                self.status = format!(
                    "serial {serial}: replaced by serial {replacement} under RMA {}",
                    case.reference
                );
                self.lifecycle.rma_replacement.clear();
                self.reload_lifecycle();
            }
            Err(e) => self.lifecycle.error = Some(e.to_string()),
        }
    }

    /// Close an RMA that is not producing a replacement.
    pub fn close_rma_case(&mut self, id: uuid::Uuid) {
        self.lifecycle.error = None;
        let Some(serial) = self.lifecycle.serial else {
            return;
        };
        let note = self.lifecycle.rma_fault.trim().to_owned();
        let Some(store) = &self.store else {
            self.lifecycle.error = Some("no database is open".into());
            return;
        };
        match store.close_rma(id, &note) {
            Ok(case) => {
                self.record(
                    "key.rma.closed",
                    &format!("serial:{serial}"),
                    &format!(
                        "{} note_chars={}",
                        case.audit_detail(),
                        note.chars().count()
                    ),
                );
                self.status = format!("serial {serial}: RMA {} closed", case.reference);
                self.lifecycle.rma_fault.clear();
                self.reload_lifecycle();
            }
            Err(e) => self.lifecycle.error = Some(e.to_string()),
        }
    }

    /// Tick or untick one applet on the manual sanitisation form.
    pub fn toggle_sanitised_applet(&mut self, applet: crate::device::reset::Applet, wanted: bool) {
        self.lifecycle.sanitised_applets.retain(|a| *a != applet);
        if wanted {
            self.lifecycle.sanitised_applets.push(applet);
        }
        self.lifecycle.sanitised_applets = crate::device::reset::Applet::ALL
            .into_iter()
            .filter(|a| self.lifecycle.sanitised_applets.contains(a))
            .collect();
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
        let attempted = password.is_some();
        let config = self.config.clone().with_password(password);
        let path = config.path.clone();
        match Store::open(&config) {
            Ok(store) => {
                self.adopt(store, config);
                self.record("app.opened", "database", "");
                self.note_unlock_success();
                self.refresh();
                self.check_overdue_signatures();
            }
            Err(e) => {
                tracing::error!(event = "db.open.failed", reason = %e);
                // Only when a password was actually submitted. Startup calls this
                // with `None` to see whether the file is encrypted at all, and
                // that probe is the application's question rather than a guess at
                // anybody's password — counting it would have every session with
                // an encrypted register start one failure down.
                if attempted && e.is_wrong_password() {
                    self.note_unlock_failure(&path);
                }
                let mut message = e.to_string();
                if attempted
                    && e.is_wrong_password()
                    && let Some(wait) = self.throttle.message()
                {
                    message = format!("{message}. {wait}");
                }
                // Through the same display as every other refused open. It has to
                // be: this is the path an ordinary launch takes, so a register a
                // second window is holding is refused *here* — and before this it
                // arrived as a sentence with no card and no way to take the lock
                // over, which left the operator reading who had it and unable to
                // act on it.
                self.show_open_failure(&path, &e, message);
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
        match store.unfinished_batches() {
            Ok(batches) => self.batch.resumable = batches,
            // Not fatal: a batch that cannot be listed is one nobody is offered
            // to resume, which is a lesser failure than refusing to refresh.
            Err(e) => tracing::warn!(event = "batch.read.failed", reason = %e),
        }
        match store.presence() {
            Ok(presence) => self.presence = presence,
            // Not fatal and not shown: the banner is a coordination aid, and a
            // register that cannot say who else is in it is still a register.
            Err(e) => tracing::warn!(event = "db.presence.read_failed", reason = %e),
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
        match store.filed_documents() {
            Ok(filed) => self.filed_documents = filed,
            Err(e) => tracing::error!(event = "document.read.failed", reason = %e),
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

    // ------------------------------- templates as files, signatures, and diffs

    /// The signature verdict for a template, under this deployment's trusted keys.
    ///
    /// One place, so the badge in the catalogue, the pre-flight check before a run
    /// and the import preview cannot disagree about whether a template is signed.
    pub fn template_trust(&self, template: &crate::template::BootstrapTemplate) -> Trust {
        crate::template::signing::verify(template, &self.settings.template_keys)
    }

    /// May a run use this template?
    ///
    /// `Ok(trust)` when it may, carrying the verdict so the caller can audit it —
    /// an unsigned template that ran under pilot mode is a fact the trail has to
    /// keep. `Err(message)` when signatures are required and this one does not
    /// verify.
    ///
    /// Deliberately **not** a bare bool: "may I run this" and "what was the
    /// signature state" are both needed at the same moment, and computing them
    /// separately is how the audit entry ends up disagreeing with the decision.
    pub fn template_run_permission(
        &self,
        template: &crate::template::BootstrapTemplate,
    ) -> std::result::Result<Trust, String> {
        let trust = self.template_trust(template);
        if trust.is_verified() || !self.settings.templates_must_be_signed {
            return Ok(trust);
        }
        Err(format!(
            "this deployment requires a signed template, and `{}` version {} is {}. {}",
            template.id,
            template.version,
            trust.label(),
            trust.describe()
        ))
    }

    /// Is this session allowed to run unsigned templates, and should it say so?
    pub fn pilot_mode(&self) -> bool {
        !self.settings.templates_must_be_signed
    }

    /// Write a template version to a file (phase 4).
    ///
    /// The path comes from the field the operator typed, or from the save dialog
    /// in a build that has one. Nothing is exported that is not already on record:
    /// this writes what the database holds, so a file cannot carry a procedure
    /// nobody stored.
    pub fn export_template(&mut self, id: &str, version: &str, path: &Path) {
        self.template_editor.error = None;
        self.template_editor.notice = None;

        let Some(stored) = self
            .template_catalogue
            .iter()
            .find(|s| s.template.id == id && s.template.version == version)
            .map(|s| s.template.clone())
        else {
            self.template_editor.error = Some(format!("no template `{id}` version {version}"));
            return;
        };

        let file = crate::template::TemplateFile::of(&stored, chrono::Utc::now());
        match std::fs::write(path, file.to_json()) {
            Ok(()) => {
                // The **canonical bytes**, beside the export, because without them
                // the signing half of phase 5 cannot be performed at all: a
                // signature is over those exact bytes, and this application
                // deliberately cannot make one. Writing them here is what lets
                // `openssl pkeyutl -sign -rawin` — or an HSM, or a smartcard — do
                // it, with no tool in between that could disagree about the
                // encoding. Derivable, so it costs nothing to regenerate, and
                // useless to anybody without the key.
                let canonical = path.with_extension("canonical");
                let bytes = crate::template::signing::canonical_bytes(&stored);
                let also = match std::fs::write(&canonical, &bytes) {
                    Ok(()) => format!(
                        " The bytes to sign are beside it in {} ({} bytes) — see \
                         docs/operations.md for the signing step.",
                        canonical.display(),
                        bytes.len()
                    ),
                    Err(e) => {
                        // Not fatal: the procedure itself is exported, which is what
                        // was asked for. Said out loud rather than swallowed,
                        // because somebody about to have it signed needs to know.
                        tracing::warn!(
                            event = "template.export.canonical_failed",
                            path = %canonical.display(),
                            reason = %e
                        );
                        format!(
                            " The bytes to sign could not be written to {}: {e}",
                            canonical.display()
                        )
                    }
                };

                // Audited: a procedure leaving this register is a fact worth
                // keeping, for the same reason an export of personal data is. It
                // carries no secret — a template holds none — and it names the
                // file so "where did this come from" is answerable.
                self.record(
                    "template.exported",
                    &format!("template:{id}"),
                    &format!(
                        "version={version} steps={} fingerprint={} file={}",
                        stored.steps.len(),
                        crate::template::signing::fingerprint(&stored),
                        path.display()
                    ),
                );
                self.status = format!("{id} version {version} written to {}", path.display());
                self.template_editor.notice = Some(format!(
                    "exported to {}. Its fingerprint is {} — whoever receives it can check they \
                     have the same procedure without reading the whole file.{also}",
                    path.display(),
                    crate::template::signing::fingerprint(&stored)
                ));
            }
            Err(e) => {
                tracing::error!(event = "template.export.failed", template = id, reason = %e);
                self.template_editor.error =
                    Some(format!("could not write {}: {e}", path.display()));
            }
        }
    }

    /// Read a template file and hold it as a preview (phase 4).
    ///
    /// Reading is not importing. The operator sees what the file contains, whether
    /// its signature verifies *here*, and what it would change against the version
    /// this register already holds — and then decides. The same shape as the CSV
    /// import, and for the same reason: a procedure decides what is written to
    /// security hardware, so "I did not realise it would change that" is not an
    /// acceptable outcome of a click.
    pub fn read_template_file(&mut self, path: &Path) {
        self.template_editor.error = None;
        self.template_editor.notice = None;
        self.template_editor.pending_import = None;

        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) => {
                self.template_editor.error =
                    Some(format!("could not read {}: {e}", path.display()));
                return;
            }
        };
        let file = match crate::template::TemplateFile::from_json(&raw) {
            Ok(file) => file,
            Err(e) => {
                tracing::warn!(event = "template.import.refused", path = %path.display(), reason = %e);
                self.template_editor.error = Some(e.to_string());
                return;
            }
        };

        let trust = self.template_trust(&file.template);
        let against = self
            .latest_stored_template(&file.template.id)
            .map(|current| {
                (
                    current.version.clone(),
                    crate::template::diff::diff(current, &file.template),
                )
            });

        self.template_editor.pending_import = Some(PendingImport {
            source: path.display().to_string(),
            file,
            trust,
            against,
        });
    }

    /// Store the previewed import as a new version (phase 4).
    pub fn apply_template_import(&mut self) {
        self.template_editor.error = None;
        self.template_editor.notice = None;

        let Some(pending) = self.template_editor.pending_import.take() else {
            return;
        };
        let incoming = pending.file.template.clone();
        let Some(store) = &self.store else {
            self.template_editor.error = Some("no database open".into());
            return;
        };

        match store.import_template(&incoming) {
            Ok(outcome) => {
                if let crate::store::TemplateImport::Stored { template, previous } = &outcome {
                    // One event for an import, distinct from `template.created`
                    // and `template.changed`: where a procedure came from is
                    // exactly what a reviewer will want years later, and it is not
                    // recoverable from the body.
                    self.record(
                        "template.imported",
                        &format!("template:{}", template.id),
                        &format!(
                            "version={} previous={} steps={} fingerprint={} {} source={}",
                            template.version,
                            previous.as_deref().unwrap_or("none"),
                            template.steps.len(),
                            crate::template::signing::fingerprint(template),
                            pending.trust.audit_detail(),
                            pending.source
                        ),
                    );
                }
                self.status = outcome.describe();
                self.template_editor.notice = Some(outcome.describe());
                self.refresh();
            }
            Err(e) => {
                tracing::warn!(event = "template.import.failed", reason = %e);
                self.template_editor.error = Some(e.to_string());
            }
        }
    }

    /// Drop a previewed import the operator declined.
    pub fn cancel_template_import(&mut self) {
        self.template_editor.pending_import = None;
        self.template_editor.error = None;
    }

    /// Compare two versions of a template (phase 6).
    ///
    /// Computed from the cached catalogue, so it is pure data and needs no database
    /// read inside a paint pass.
    pub fn template_diff(&self) -> Option<crate::template::TemplateDiff> {
        let (id, from, to) = self.template_editor.compare.as_ref()?;
        let find = |version: &str| {
            self.template_catalogue
                .iter()
                .find(|s| s.template.id == *id && s.template.version == version)
                .map(|s| &s.template)
        };
        Some(crate::template::diff::diff(find(from)?, find(to)?))
    }

    /// Add a public key to the template trust store.
    ///
    /// Checked before it is stored: a malformed key would make every template
    /// signed by that id read as *altered* rather than as *misconfigured*, sending
    /// the operator after the wrong problem entirely. Replacing an id that is
    /// already listed is allowed and is the normal way to rotate a key — with the
    /// consequence stated, because every template signed by the old one starts
    /// failing verification at that moment.
    pub fn add_template_key(&mut self) {
        self.key_form.error = None;
        let key = crate::template::TemplateKey {
            id: self.key_form.id.trim().to_owned(),
            public_key: self.key_form.public_key.trim().to_lowercase(),
            comment: self.key_form.comment.trim().to_owned(),
        };
        if let Err(refusal) = key.check() {
            self.key_form.error = Some(refusal);
            return;
        }

        let replacing = self
            .settings
            .template_keys
            .iter()
            .position(|existing| existing.id == key.id);
        match replacing {
            Some(index) => {
                let old = std::mem::replace(&mut self.settings.template_keys[index], key.clone());
                self.status = if old.public_key == key.public_key {
                    format!(
                        "`{}` was already trusted — its description was updated",
                        key.id
                    )
                } else {
                    format!(
                        "`{}` now has different key material: every template signed by the old \
                         one will read as altered until it is signed again",
                        key.id
                    )
                };
            }
            None => {
                self.status = format!("`{}` is now trusted to sign templates", key.id);
                self.settings.template_keys.push(key.clone());
            }
        }
        // The key id, never the key material, in the log: it is public, but a log
        // line is read by people and 64 hex characters in it are noise.
        tracing::info!(event = "template.key.trusted", key = key.id.as_str());
        self.key_form = TemplateKeyForm::default();
    }

    /// Show the difference between a version and the newest one of its id.
    ///
    /// The question the spec asks — "what changed since the batch we shipped in
    /// June?" — starts from a version a run recorded, so this is the entry point
    /// from a catalogue row.
    pub fn compare_with_latest(&mut self, id: &str, version: &str) {
        let latest = self
            .latest_stored_template(id)
            .map(|t| t.version.clone())
            .unwrap_or_else(|| version.to_owned());
        self.template_editor.compare = Some((id.to_owned(), version.to_owned(), latest));
        self.template_editor.error = None;
    }

    // ------------------------------------------------------ consignment terms

    /// Render the consignment term for a hand-over, in the requested language.
    pub fn generate_term(&mut self, distribution_id: uuid::Uuid) {
        use crate::term::{TermContext, choose_template, render_term};

        self.term_panel.open = true;
        self.term_panel.distribution = Some(distribution_id);
        self.term_panel.rendered = None;
        self.term_panel.pdf = None;
        self.term_panel.pdf_note = None;
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
        // Owned, so that producing the PDF and writing the audit entry below do
        // not fight over the borrow of `self.term_templates`.
        let Some(template) =
            choose_template(&self.term_templates, "consignment", &language).cloned()
        else {
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

        match render_term(&template, &ctx) {
            Ok(text) => {
                let details = format!(
                    "holder={} language={} template={}@{}",
                    holder.email, template.language, template.id, template.version
                );
                self.term_panel.pdf_note = Self::pdf_note(&text);
                self.term_panel.pdf = self.term_pdf(&template, &ctx);
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

    // ------------------------------------- signature tracking (receipts, ph 4)

    /// What is on file against one hand-over.
    pub fn filed_for(&self, distribution_id: uuid::Uuid) -> crate::receipt::Filed {
        self.filed_documents
            .get(&distribution_id)
            .copied()
            .unwrap_or_default()
    }

    /// Where the term for one hand-over stands.
    ///
    /// Derived every time it is asked for, from the record, what is filed and the
    /// clock. Nothing is stored: a `signature_state` column would need updating when
    /// a document is filed, when a reference is typed, and — impossibly — when a day
    /// passes.
    pub fn signature_state(&self, record: &DistributionRecord) -> crate::receipt::SignatureState {
        crate::receipt::state_of(
            record,
            self.filed_for(record.id),
            &self.settings.signatures,
            chrono::Utc::now(),
        )
    }

    /// Has the return been documented? Only meaningful once a key is back.
    pub fn return_state(&self, record: &DistributionRecord) -> crate::receipt::ReturnState {
        crate::receipt::return_state_of(record, self.filed_for(record.id))
    }

    /// How the register's paperwork stands, for the line at the top of the screen.
    pub fn outstanding_paperwork(&self) -> crate::receipt::Outstanding {
        crate::receipt::outstanding(
            &self.distributions,
            |record| self.filed_for(record.id),
            &self.settings.signatures,
            chrono::Utc::now(),
        )
    }

    /// Record `receipt.pending_overdue` for terms that have passed the threshold,
    /// **once each, ever**.
    ///
    /// Called when the register is opened and after anything that could change the
    /// answer. Two problems to avoid, and one of them is the reason this looks the
    /// way it does:
    ///
    /// * A time-based transition has no click behind it. Nobody *does* anything to
    ///   make a term overdue, so there is no natural moment to audit — and auditing
    ///   from the paint pass would write an entry per frame.
    /// * "Once each" has to survive restarts, or every session re-records the same
    ///   hand-overs and the trail fills with duplicates of a fact it already holds.
    ///
    /// So the trail itself is the record of what has been recorded: the existing
    /// `receipt.pending_overdue` entries are read, and only a hand-over not among
    /// them is written. No new column, and idempotent for the life of the register —
    /// the audit table cannot be rewritten, which is exactly the property that makes
    /// it usable as this marker.
    pub fn check_overdue_signatures(&mut self) {
        if !self.settings.signatures.required {
            return;
        }
        let Some(store) = &self.store else { return };

        let already: std::collections::BTreeSet<String> = match store.audit_entries_matching(
            &crate::audit::AuditFilter {
                event: "receipt.pending_overdue".into(),
                ..Default::default()
            },
            usize::MAX,
        ) {
            Ok(entries) => entries.into_iter().map(|entry| entry.target).collect(),
            Err(e) => {
                // Without the check the entries would duplicate, so this stops
                // rather than writing possibly-repeated history.
                tracing::error!(event = "receipt.overdue.check_failed", reason = %e);
                return;
            }
        };

        let now = chrono::Utc::now();
        let newly: Vec<(String, String)> = self
            .distributions
            .iter()
            .filter_map(|record| {
                let state = crate::receipt::state_of(
                    record,
                    self.filed_for(record.id),
                    &self.settings.signatures,
                    now,
                );
                if !state.is_overdue() {
                    return None;
                }
                let target = format!("distribution:{}", record.id);
                if already.contains(&target) {
                    return None;
                }
                Some((target, state.audit_detail()))
            })
            .collect();

        for (target, detail) in newly {
            self.record("receipt.pending_overdue", &target, &detail);
        }
    }

    /// Record the unit's own reference for a signed term, after the hand-over.
    ///
    /// The other way a term gets settled, for a unit that files paper elsewhere:
    /// `receipt.signed` is the event the spec defines for it, and this is what
    /// writes it. Audited with the serial and the reference — the reference is a
    /// document number, not personal data, and it is the thing somebody will search
    /// the trail for.
    pub fn record_receipt_reference(&mut self, distribution_id: uuid::Uuid, reference: &str) {
        self.term_panel.error = None;
        let reference = reference.trim().to_owned();
        if reference.is_empty() {
            self.term_panel.error = Some(
                "type the reference your unit filed the signed term under, or upload the scan \
                 instead"
                    .into(),
            );
            return;
        }
        if reference.chars().count() > crate::domain::MAX_TEXT {
            self.term_panel.error = Some(format!(
                "a reference is at most {} characters",
                crate::domain::MAX_TEXT
            ));
            return;
        }

        let serial = self
            .distributions
            .iter()
            .find(|d| d.id == distribution_id)
            .map(|d| d.key_serial);
        let Some(serial) = serial else {
            self.term_panel.error = Some("hand-over not found".into());
            return;
        };

        let result = {
            let Some(store) = &self.store else {
                self.term_panel.error = Some("no database open".into());
                return;
            };
            store.set_receipt_ref(distribution_id, &reference)
        };
        match result {
            Ok(()) => {
                self.record(
                    "receipt.signed",
                    &format!("serial:{serial}"),
                    &format!("reference={reference}"),
                );
                self.status = format!("signed term recorded for serial {serial}: {reference}");
                self.term_panel.reference.clear();
                self.refresh();
            }
            Err(e) => self.term_panel.error = Some(e.to_string()),
        }
    }

    // --------------------------------- the return receipt (receipts, phase 6)

    /// Render the receipt that closes the custody loop for a returned key.
    ///
    /// The mirror of [`Self::generate_term`], and deliberately the same panel: the
    /// operator reviews it, exports it as text or PDF, and files the signed copy
    /// against the hand-over as a `ReturnReceipt`. The wording is a term template
    /// like any other (`term::RETURN_ID`), so a unit edits it in the Terms screen
    /// and it is versioned the same way.
    ///
    /// Refused for a key that has not come back. A receipt saying a key was returned
    /// on a date nobody recorded would be a document contradicting the register.
    pub fn generate_return_receipt(&mut self, distribution_id: uuid::Uuid) {
        use crate::term::{RETURN_ID, TermContext, choose_template, render_term};

        self.term_panel.open = true;
        self.term_panel.distribution = Some(distribution_id);
        self.term_panel.document = crate::term::RETURN_ID.to_owned();
        self.term_panel.rendered = None;
        self.term_panel.pdf = None;
        self.term_panel.pdf_note = None;
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
        if record.returned_at.is_none() {
            self.term_panel.error = Some(
                "this key has not been returned yet — record the return first, and the receipt \
                 will carry its date"
                    .into(),
            );
            return;
        }

        let Some(holder) = self
            .holders
            .iter()
            .find(|h| h.id == record.holder_id)
            .cloned()
        else {
            self.term_panel.error = Some("the holder of this hand-over is not on record".into());
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

        // A return receipt says nothing about what was applied — that belongs to the
        // consignment term — so `applied` and `custody` are left out rather than
        // repeated. The template does not reference them.
        let ctx = TermContext::from_records(
            &holder,
            &key,
            Some(&record),
            "",
            "",
            &self.operator,
            &self.org,
        );

        let language = self.term_panel.language.clone();
        let Some(template) = choose_template(&self.term_templates, RETURN_ID, &language).cloned()
        else {
            self.term_panel.error = Some(format!(
                "no return receipt wording for `{language}` — add it in the Terms screen"
            ));
            return;
        };
        if !template.language.eq_ignore_ascii_case(&language) {
            self.term_panel.language_used = Some(template.language.clone());
        }
        self.term_panel.template_used = Some(format!(
            "{}@{} ({})",
            template.id, template.version, template.language
        ));

        match render_term(&template, &ctx) {
            Ok(text) => {
                self.term_panel.pdf_note = Self::pdf_note(&text);
                self.term_panel.pdf = self.term_pdf(&template, &ctx);
                self.term_panel.rendered = Some(text);
                self.record(
                    "receipt.generated",
                    &format!("serial:{}", record.key_serial),
                    &format!(
                        "kind=return holder={} language={} template={}@{}",
                        holder.email, template.language, template.id, template.version
                    ),
                );
            }
            Err(e) => self.term_panel.error = Some(e.to_string()),
        }
    }

    /// The term as a PDF, or `None` with the reason in the status bar.
    ///
    /// A failure here is not fatal: the text output is still on screen and still
    /// saveable, so the operator is told and the hand-over continues.
    fn term_pdf(
        &mut self,
        template: &crate::term::TermTemplate,
        ctx: &crate::term::TermContext,
    ) -> Option<Vec<u8>> {
        let created = crate::pdf::pdf_date(&chrono::Local::now());
        match crate::term::render_term_pdf(template, ctx, &created) {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                tracing::warn!(event = "term.pdf.refused", reason = %e);
                self.status = format!("the PDF could not be produced: {e} — save as text instead");
                None
            }
        }
    }

    /// Warn about characters the PDF font cannot set, before anybody prints a
    /// document full of question marks.
    fn pdf_note(text: &str) -> Option<String> {
        let missing = crate::pdf::unrepresentable(text);
        if missing.is_empty() {
            return None;
        }
        let shown: String = missing.iter().take(8).collect();
        Some(format!(
            "the PDF font cannot set {} character(s) of this term ({shown}) — they print as `?`. \
             The text output carries them correctly.",
            missing.len()
        ))
    }

    // ------------------------------------------------- editing a term template

    /// Fill the editor buffers from the newest stored version of a language.
    ///
    /// Reads the cached templates, never the database, so it is safe to call from
    /// a click. A language with nothing stored opens on the built-in wording when
    /// the build ships one, and blank otherwise.
    /// Switch the Terms editor to another document type, keeping the language.
    ///
    /// Refuses to discard an unsaved edit, the same way switching language does: a
    /// legal text somebody has just typed is not something to lose to a click on a
    /// dropdown.
    pub fn load_term_document(&mut self, id: &str) {
        self.term_editor.error = None;
        self.term_editor.notice = None;
        if self.term_editor.is_dirty(&self.term_templates) {
            self.term_editor.error = Some(format!(
                "there are unsaved changes to the {} wording in {} — save them, or discard them \
                 with “Reload stored version”, before switching document",
                self.term_editor.id, self.term_editor.language
            ));
            return;
        }
        self.term_editor.id = id.trim().to_owned();
        let language = self.term_editor.language.clone();
        self.load_term_template(&language);
    }

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

    /// Export the draft as a PDF against the sample data.
    ///
    /// This is how the wording reaches the people who own it: the term's text is
    /// institutional, and it needs its owner's review and the DPO's sign-off on
    /// the data-protection paragraph (`features/consignment-terms.md`). Sending
    /// them the document as it will be printed is more use than sending them a
    /// template with `{{variables}}` in it.
    ///
    /// Nothing is stored and no hand-over is involved: the values are
    /// [`crate::term::TermContext::sample`]'s fictitious ones, and the footer
    /// says `@draft`.
    pub fn save_term_preview_pdf(&mut self) {
        use crate::term::{TermContext, render_term, render_term_pdf};

        self.term_editor.error = None;
        self.term_editor.notice = None;
        let draft = self.term_editor.draft();
        if let Err(e) = draft.check() {
            self.term_editor.error = Some(e.to_string());
            return;
        }

        let sample = TermContext::sample();
        let created = crate::pdf::pdf_date(&chrono::Local::now());
        let bytes = match render_term_pdf(&draft, &sample, &created) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.term_editor.error = Some(e.to_string());
                return;
            }
        };
        if let Ok(text) = render_term(&draft, &sample) {
            self.term_editor.error = Self::pdf_note(&text);
        }

        let suggested = format!(
            "{}-{}-sample.pdf",
            draft.id.trim(),
            draft.language.trim().replace('/', "-")
        );
        if let Some(path) = self.save_bytes(&suggested, &bytes) {
            self.status = format!("sample term written to {}", path.display());
            self.term_editor.notice = Some(format!(
                "written to {} — sample data, nothing recorded and nothing stored",
                path.display()
            ));
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

    /// Write the rendered term to a file the operator chooses, as plain text.
    pub fn save_term(&mut self) {
        let Some(text) = self.term_panel.rendered.clone() else {
            return;
        };
        self.write_term("text", "txt", text.as_bytes());
    }

    /// Write the term as the PDF that gets printed, signed and filed.
    pub fn save_term_pdf(&mut self) {
        let Some(bytes) = self.term_panel.pdf.clone() else {
            self.status = "generate the term before exporting it as a PDF".into();
            return;
        };
        self.write_term("pdf", "pdf", &bytes);
    }

    /// Save one rendering of the term, and audit which format left the tool.
    ///
    /// The format is in the audit detail because the two outputs are filed
    /// differently — a signed PDF comes back as a scan, a text copy goes into a
    /// ticket — so "a term was written" is not enough to reconstruct what
    /// happened.
    fn write_term(&mut self, format: &str, extension: &str, bytes: &[u8]) {
        let serial = self
            .term_panel
            .distribution
            .and_then(|id| self.distributions.iter().find(|d| d.id == id))
            .map(|d| d.key_serial)
            .unwrap_or_default();
        let suggested = format!("termo-{serial}.{extension}");

        if let Some(path) = self.save_bytes(&suggested, bytes) {
            let display = path.display().to_string();
            self.record(
                "term.saved",
                &format!("serial:{serial}"),
                &format!("format={format} path={display}"),
            );
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

    // ------------------------------------------------- the certificate exchange
    //
    // The issuer is the operator (`features/ca-integration.md` phase 1, decided
    // 2026-08-13): the run produces a request, somebody signs it at whichever CA
    // the deployment uses, and the certificate comes back through this pair of
    // methods. Two steps, both explicit, because the round trip leaves this
    // workstation and may take days.

    /// Save the certification request this run produced.
    ///
    /// Read out of the persisted run rather than held in memory, so it survives
    /// the register being closed and reopened while the CA does its work. A
    /// request nobody can retrieve means generating a second key and abandoning
    /// the first, which is why this is not a "copy to clipboard".
    pub fn save_certificate_request(&mut self) {
        let Some(csr) = self
            .wizard
            .run
            .as_ref()
            .and_then(crate::bootstrap::certificate_request)
            .map(str::to_owned)
        else {
            self.wizard.error =
                Some("this run produced no certification request to save".to_owned());
            return;
        };

        let serial = self.wizard.serial.trim().to_owned();
        let filename = format!("csr-{serial}.pem");
        if let Some(path) = self.save_bytes(&filename, csr.as_bytes()) {
            let display = path.display().to_string();
            // Audited: this is the point at which a request for a certificate in a
            // holder's name leaves the tool, and "who asked for that certificate"
            // is a question an audit does ask.
            self.record(
                // The name this event has in `features/ca-integration.md`.
                "ca.csr.exported",
                &format!("serial:{serial}"),
                &display,
            );
            self.status = format!("certification request written to {display}");
        }
    }

    /// Load an issued certificate from a file, ready for the import step.
    pub fn load_certificate(&mut self) {
        let Some((name, bytes)) = self.read_certificate_file() else {
            return;
        };
        // Lossy on purpose: a DER file is not UTF-8, and `device::certificate`
        // accepts raw DER as well as PEM. A file that is neither fails at the
        // preview, with a sentence, rather than here.
        self.wizard.certificate_pem = String::from_utf8_lossy(&bytes).into_owned();
        self.preview_certificate();
        self.status = match &self.wizard.certificate_preview {
            Some(Ok(summary)) => format!("{name}: {}", summary.one_line()),
            _ => format!("{name} was loaded, but it is not a certificate this tool can read"),
        };
    }

    /// Parse what is in the certificate box, so the operator sees whose it is
    /// before it reaches the key.
    pub fn preview_certificate(&mut self) {
        self.wizard.certificate_preview = Some(crate::device::certificate::preview(
            &self.wizard.certificate_pem,
        ));
    }

    /// Does the loaded certificate carry the address this run is building for?
    ///
    /// The wizard shows the answer before the run. The import step checks it again
    /// and refuses — this is the warning, not the gate.
    pub fn certificate_matches_holder(&self) -> Option<bool> {
        let summary = match self.wizard.certificate_preview.as_ref()? {
            Ok(summary) => summary,
            Err(_) => return None,
        };
        let holder = self.holders.get(self.wizard.holder_index)?;
        let ctx = RenderContext::for_holder(
            holder,
            self.wizard.serial.trim().parse().unwrap_or_default(),
            &self.operator,
            &self.org,
        );
        let expected = self.settings.san.render(&ctx).ok()?;
        Some(summary.covers_email(&expected))
    }

    #[cfg(feature = "file-dialog")]
    fn read_certificate_file(&mut self) -> Option<(String, Vec<u8>)> {
        let path = rfd::FileDialog::new()
            .set_title("Choose the issued certificate")
            .add_filter("Certificate", &["pem", "crt", "cer", "der"])
            .pick_file()?;
        match std::fs::read(&path) {
            Ok(content) => Some((
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "certificate".into()),
                content,
            )),
            Err(e) => {
                self.status = format!("could not read {}: {e}", path.display());
                None
            }
        }
    }

    #[cfg(not(feature = "file-dialog"))]
    fn read_certificate_file(&mut self) -> Option<(String, Vec<u8>)> {
        // Not a dead end: the certificate can still be pasted into the box, which
        // is what a PEM document is for.
        self.status = "choosing a file needs a build with `--features file-dialog` — paste the \
                       certificate instead"
            .into();
        None
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
        // The chosen key, when the operator has chosen one — `None` still means "the
        // only attached key", and still refuses when several are attached. That
        // refusal is the whole reason the picker exists, so this must not become
        // "whichever one is first".
        match self.backend.info(self.selected_serial) {
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
                // Ambiguity is not a fault to log and forget: it is the situation
                // the picker answers, and the trail records that nothing was chosen.
                if let crate::device::DeviceError::Ambiguous(n) = e {
                    self.record(
                        "device.ambiguous",
                        "device",
                        &format!("attached={n} — nothing was chosen"),
                    );
                }
            }
        }
    }

    /// Change the transport the operator reads through, and say what happened.
    ///
    /// Re-decided rather than assumed: choosing *native* on a machine whose PC/SC is
    /// dead has to report itself as chosen-and-failing rather than as working, and
    /// the only way to know is to probe again.
    ///
    /// Audited as `device.transport.selected`, because which transport wrote to a key
    /// is part of the story of that key — and because a session that silently changed
    /// transport mid-way is the first thing to check when two runs of the same
    /// template behave differently.
    pub fn set_transport(&mut self, requested: crate::device::Transport) {
        if self.settings.transport == requested {
            return;
        }
        self.settings.transport = requested;
        self.settings.save_quietly();

        let choice =
            crate::device::select::decide(requested, crate::device::select::probe(requested));
        self.backend = crate::device::select::backend_for(&choice);
        tracing::info!(
            event = "device.transport.selected",
            transport = choice.transport.slug(),
            disabled = choice.disabled,
            reason = choice.reason.as_str()
        );
        // Recorded when there is a register to record it in; at startup there is
        // not, which is why the log line above is unconditional and this is not.
        self.record(
            "device.transport.selected",
            "device",
            &format!(
                "requested={} using={} disabled={}",
                requested.slug(),
                choice.transport.slug(),
                choice.disabled
            ),
        );
        self.status = format!("transport: {}", choice.describe());
        self.transport = choice;

        // The watch holds a backend of its own, made when it started. Stopping it
        // makes the next frame build one through the new choice — otherwise the
        // screen would show one transport while the watch polled through another.
        self.stop_device_watch("the transport changed");
        self.detected.clear();
    }

    // ------------------------------------------------- watching for hardware

    /// Which screens want to know what is plugged in.
    ///
    /// Not every screen: the watch costs a `ykman` subprocess per tick in the
    /// default build, and paying that while somebody reads the audit trail buys
    /// nothing. Inventory is where keys are taken in, and Bootstrap is where one is
    /// prepared — those two.
    fn tab_watches_hardware(tab: Tab) -> bool {
        matches!(tab, Tab::Inventory | Tab::Bootstrap)
    }

    /// Start or stop the watch to match the screen the operator is on.
    ///
    /// Called once per frame. Idempotent by construction: it compares what should
    /// be running against what is, so the frame after a tab change does the work
    /// and every frame after that does nothing.
    pub fn sync_device_watch(&mut self) {
        // Not while a reset is arming: `device::reinsert` is watching the same
        // reader for the same key, faster and for a deadline, and two enumerations
        // of one port — one of them a subprocess — is not what to spend a
        // five-second window on.
        let wanted = Self::tab_watches_hardware(self.tab)
            && self.store.is_some()
            && self.reset.handshake.is_none();
        match (wanted, self.watch.is_some()) {
            (true, false) => self.start_device_watch(),
            (false, true) => self.stop_device_watch("the screen that needed it was closed"),
            _ => {}
        }
    }

    /// Begin watching, with a backend of its own.
    pub fn start_device_watch(&mut self) {
        // The tick follows the *live* transport, not what the build could do: a
        // native build demoted to the subprocess at startup must poll at the
        // subprocess rate, or it forks a Python process every 1.5s for as long as
        // the screen is open.
        let interval = crate::device::watch::interval_for(
            self.transport.transport == crate::device::Transport::Native,
        );
        // A second backend, not the one the GUI holds: read-on-demand and the watch
        // must not be two callers of one handle. Built through the same choice the
        // GUI reads by — a watch polling a different transport from the one the
        // status bar names would make the status bar a lie.
        let backend = crate::device::select::backend_for(&self.transport);
        tracing::debug!(
            event = "device.watch.started",
            interval_ms = interval.as_millis() as i64
        );
        self.watch = Some(crate::device::DeviceWatch::start(backend, interval));
    }

    /// Stop watching and forget what was attached.
    ///
    /// `why` is logged rather than shown: stopping is normal (leaving a screen,
    /// starting a run) and a status line about it would be noise.
    pub fn stop_device_watch(&mut self, why: &str) {
        if self.watch.take().is_some() {
            tracing::debug!(event = "device.watch.stopped", reason = why);
        }
        // The list is cleared with it. Leaving a stale "2 keys attached" on screen
        // after the watch has gone is worse than showing nothing, because it invites
        // a choice that is no longer based on anything.
        self.attached = crate::device::Attached::default();
    }

    /// Read the watch's latest snapshot, once per frame.
    ///
    /// Returns whether this is a new arrangement of keys — which is the moment the
    /// screen may act on it: fill a serial, or drop a selection that has been
    /// unplugged.
    pub fn poll_device_watch(&mut self) -> bool {
        let Some(watch) = &self.watch else {
            return false;
        };
        self.attached = watch.snapshot();
        if self.attached.generation == self.seen_generation {
            return false;
        }
        self.seen_generation = self.attached.generation;

        // A selection only survives while the key it names is still there. Silently
        // keeping it would leave the wizard aimed at a serial nobody can see.
        if let Some(serial) = self.selected_serial
            && !self.attached.serials().contains(&serial)
        {
            self.selected_serial = None;
            self.status = format!("serial {serial} was unplugged — choose a key again");
        }

        // Exactly one key, unambiguous: adopt it as the selection so the wizard and
        // the inventory screen have something to work with. This fills a *field*,
        // and records nothing — see the module documentation on `device::watch`.
        if let Some(only) = self.attached.only_key() {
            self.selected_serial = Some(only.serial);
            if self.wizard.serial.trim().is_empty() {
                self.wizard.serial = only.serial.to_string();
            }
        }

        // Two or more, and nothing chosen: audited, because the spec lists it and
        // because "the operator was shown two keys and had to choose" is part of the
        // story of whichever key was then written to.
        if self.attached.is_ambiguous() && self.selected_serial.is_none() {
            self.record(
                "device.ambiguous",
                "device",
                &format!(
                    "attached={} unreadable={} — nothing was chosen",
                    self.attached.keys.len(),
                    self.attached.unreadable.len()
                ),
            );
        }

        true
    }

    /// The key an operation should act on: the chosen one, or the only one.
    ///
    /// `None` when there is nothing attached, or when several are and none has been
    /// chosen. Every caller that would touch hardware goes through this rather than
    /// reaching for `attached.keys[0]`.
    pub fn target_serial(&self) -> Option<u32> {
        self.selected_serial
            .or_else(|| self.attached.only_key().map(|key| key.serial))
    }

    /// Choose one of several attached keys (phase 3).
    pub fn select_key(&mut self, serial: u32) {
        self.selected_serial = Some(serial);
        self.wizard.serial = serial.to_string();
        let model = self
            .attached
            .keys
            .iter()
            .find(|k| k.serial == serial)
            .map(|k| k.model.clone())
            .unwrap_or_default();
        self.status = format!("{model} {serial} selected");
        // Audited: with several keys attached, *which one* the operator picked is
        // the fact that explains everything written afterwards.
        self.record(
            "device.selected",
            &format!("serial:{serial}"),
            &format!("model={model} of={} attached", self.attached.keys.len()),
        );
    }

    /// Selected template, if any.
    pub fn selected_template(&self) -> Option<&BootstrapTemplate> {
        // A pinned version wins: it is the one a resumed run actually applied, and
        // it may no longer be in the list the wizard offers.
        self.wizard
            .pinned_template
            .as_ref()
            .or_else(|| self.templates.get(self.wizard.template_index))
    }

    /// Every template version on record, including superseded and retired ones.
    ///
    /// The wizard offers only the newest of each for a *new* run; a **resume** needs
    /// the exact version the run recorded, whatever has happened since.
    fn template_version(&self, id: &str, version: &str) -> Option<BootstrapTemplate> {
        self.template_catalogue
            .iter()
            .map(|stored| &stored.template)
            .find(|template| template.id == id && template.version == version)
            .cloned()
    }

    /// Take up an unfinished run off the register and set the wizard up to finish it
    /// (`features/gui-bootstrap-wizard.md` phase 5).
    ///
    /// The case this is for: the CA took three days, the register has been closed
    /// and reopened, and the run the operator wants to finish exists only as rows in
    /// the database. Everything the wizard needs is rebuilt from those rows — the
    /// serial, the exact template version, and which optional steps the original run
    /// included — so the plan the executor indexes against is the plan the run was
    /// made from.
    ///
    /// It deliberately does **not** start anything. The wizard lands on the run view
    /// with the certificate field waiting, and finishing it is still the operator
    /// pressing the button that resumes.
    pub fn adopt_run(&mut self, run_id: uuid::Uuid) {
        let Some(run) = self.runs.iter().find(|run| run.id == run_id).cloned() else {
            self.wizard.error = Some("that run is no longer on the register".into());
            return;
        };
        let Some(template) = self.template_version(&run.template_id, &run.template_version) else {
            self.wizard.error = Some(format!(
                "this run applied {} version {}, which is not in this register — a run cannot be \
                 resumed without the procedure it recorded",
                run.template_id, run.template_version
            ));
            return;
        };
        if let Some(refusal) = crate::bootstrap::resume_refusal(&template, &run) {
            self.wizard.error = Some(refusal);
            return;
        }

        self.wizard.serial = run.key_serial.to_string();
        self.wizard.holder_index = run
            .holder_id
            .and_then(|id| self.holders.iter().position(|holder| holder.id == id))
            .unwrap_or(self.wizard.holder_index);
        self.wizard.step_enabled = crate::bootstrap::step_selection(&template, &run);
        self.wizard.pinned_template = Some(template);
        self.wizard.certificate_pem.clear();
        self.wizard.certificate_preview = None;
        self.wizard.error = None;

        self.build_plan();
        // The plan has to line up with the run's steps or the executor would index
        // one against the other. `step_selection` is built to make it, and this says
        // so rather than trusting it — the failure would be silent and would write
        // to a key.
        if self.wizard.plan.len() != run.steps.len() {
            self.wizard.error = Some(format!(
                "this run recorded {} step(s) and the procedure plans {} — it cannot be resumed \
                 safely",
                run.steps.len(),
                self.wizard.plan.len()
            ));
            self.wizard.pinned_template = None;
            return;
        }

        self.wizard.run = Some(run);
        self.wizard.stage = WizardStage::Running;
        self.status = format!(
            "picked up an unfinished run on serial {} — load the certificate and finish it",
            self.wizard.serial
        );
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

    /// Execute the confirmed plan against the attached key.
    ///
    /// The confirmation is passed rather than assumed: `Executor::run` re-checks
    /// it against the request, so a plan changed after the operator agreed does
    /// not inherit their agreement.
    ///
    /// Returns without writing when this build has no transport — the button is
    /// disabled in that case, and this is the second guard behind it.
    pub fn execute_run(&mut self, confirmation: crate::bootstrap::Confirmation) {
        self.drive_run(RunMode::Fresh(confirmation));
    }

    /// Finish a run that stopped short, with the certificate the operator has now.
    ///
    /// This is the second half of the manual issuer
    /// (`features/ca-integration.md` phase 1) and it is not optional: the
    /// pre-flight refuses a *fresh* run on a key that is already configured, with
    /// no override (`features/device-detection.md` phase 5), so a run whose import
    /// step skipped could otherwise never be completed at all. Resuming leaves
    /// every `Done` step alone and attempts the rest.
    pub fn resume_run(&mut self) {
        let Some(run) = self.wizard.run.clone() else {
            self.wizard.error = Some("there is no run in progress to finish".into());
            return;
        };

        // The plan is rebuilt from the template on screen, and the executor
        // indexes the run's steps against it — so a plan that no longer lines up
        // is refused rather than resumed against the wrong commands.
        if run.steps.len() != self.wizard.plan.len() {
            self.wizard.error = Some(format!(
                "this run has {} step(s) and the plan on screen has {} — select the procedure and \
                 version this run used ({} {}) and build the plan again before finishing it",
                run.steps.len(),
                self.wizard.plan.len(),
                run.template_id,
                run.template_version
            ));
            return;
        }
        let selected = self
            .selected_template()
            .map(|t| (t.id.clone(), t.version.clone()));
        if selected != Some((run.template_id.clone(), run.template_version.clone())) {
            self.wizard.error = Some(format!(
                "this run applied {} version {}, and that is not what is selected — a resume must \
                 continue the same procedure",
                run.template_id, run.template_version
            ));
            return;
        }

        // The PIV PIN, if the operator typed one. Not required in general — a
        // resume may be finishing an OTP step that needs no PIN — but the
        // certificate import cannot authenticate without it, and it says so.
        let piv_pin = match self.wizard.resume_pin.trim() {
            "" => None,
            typed => match crate::secret::Secret::from_operator_input(
                crate::secret::SecretKind::PivPin,
                typed,
            ) {
                Ok(secret) => Some(secret),
                Err(e) => {
                    self.wizard.error = Some(format!("that PIV PIN is not usable: {e}"));
                    return;
                }
            },
        };
        // Wiped from the text field before the run rather than after: the run may
        // fail, and the field is not where a PIN should wait.
        self.wizard.resume_pin.clear();

        self.drive_run(RunMode::Resume { run, piv_pin });
    }

    /// The shared path: gate, build the request, and hand it to the executor.
    fn drive_run(&mut self, mode: RunMode) {
        use crate::bootstrap::{ExecutionRequest, Executor, Transports};

        let Ok(serial) = self.wizard.serial.trim().parse::<u32>() else {
            self.wizard.error = Some("invalid serial".into());
            return;
        };

        // **Nothing else touches the key while this runs.** The watch is stopped
        // first, and stopping it joins its thread, so no enumeration is in flight
        // when the first write goes out. Enumerating PC/SC readers while another
        // handle holds an exclusive transaction is not a thing to discover halfway
        // through setting a PIN — and on the subprocess transport it would mean a
        // `ykman` process competing with the executor for the same reader.
        //
        // The next frame restarts it, because `sync_device_watch` compares what
        // should be running against what is.
        self.stop_device_watch("a bootstrap run is about to write to a key");
        let Some(template) = self.selected_template().cloned() else {
            self.wizard.error = Some("no template selected".into());
            return;
        };

        // The signature gate, before the first write and before anything is
        // recorded (`features/bootstrap-templates.md` phase 5). A template decides
        // what is written to security hardware, so this is the last moment at which
        // "is this the procedure somebody approved?" can still be answered — after
        // the first step it is a question about a key that has already changed.
        let trust = match self.template_run_permission(&template) {
            Ok(trust) => trust,
            Err(refusal) => {
                tracing::warn!(
                    event = "bootstrap.refused.unsigned_template",
                    template = template.id.as_str(),
                    version = template.version.as_str(),
                );
                self.wizard.error = Some(refusal);
                return;
            }
        };
        if !trust.is_verified() {
            // Pilot mode is *visible*, which is the whole condition the spec put on
            // it: the run is allowed, and the trail says it was allowed without a
            // verified signature. An entry written afterwards would be missing for
            // exactly the run that crashed.
            self.record(
                "template.unsigned_used",
                &format!("template:{}", template.id),
                &format!(
                    "version={} serial={serial} {} pilot_mode=on",
                    template.version,
                    trust.audit_detail()
                ),
            );
        }

        let holder = self.holders.get(self.wizard.holder_index).cloned();

        // The SAN comes from the deployment's policy, not from the template:
        // which form it takes follows from which CA issues the certificate.
        let ctx = holder
            .as_ref()
            .map(|h| RenderContext::for_holder(h, serial, &self.operator, &self.org));
        let san = match ctx.as_ref().map(|c| self.settings.san.render(c)) {
            Some(Ok(value)) => value,
            Some(Err(e)) => {
                self.wizard.error = Some(format!("the certificate SAN is not usable: {e}"));
                return;
            }
            None => String::new(),
        };

        let request = ExecutionRequest {
            template: &template,
            commands: &self.wizard.plan,
            serial,
            holder_id: holder.as_ref().map(|h| h.id),
            operator: self.operator.clone(),
            relying_party: self.org.clone(),
            certificate_subject: holder
                .as_ref()
                .map(|h| h.certificate_subject(&self.org, &h.unit))
                .unwrap_or_default(),
            certificate_email: san,
            holder_display: holder.as_ref().map(|h| h.display()).unwrap_or_default(),
            certificate_pem: Some(self.wizard.certificate_pem.clone())
                .filter(|pem| !pem.trim().is_empty()),
        };

        let mut backend = match Self::write_backend(serial) {
            Some(backend) => backend,
            None => {
                self.wizard.error = Some(
                    "this build has no transport that can write to a key — rebuild with \
                     `--features native-device`"
                        .into(),
                );
                return;
            }
        };

        // The recorder is built here and dropped at the end of the call, so the
        // executor never holds the store across a frame.
        let outcome = {
            let Some(store) = self.store.take() else {
                self.wizard.error = Some("no database is open".into());
                return;
            };
            let mut recorder = StoreRecorder {
                store,
                operator: self.operator.clone(),
                failure: None,
            };
            let result = {
                let mut executor = Executor::new(Transports {
                    backend: backend.as_mut(),
                });
                let run = match mode {
                    RunMode::Fresh(confirmation) => {
                        executor.run(&request, &confirmation, &mut recorder)
                    }
                    // No `Confirmation` here because the engine's `resume` takes
                    // none: it re-attempts steps the operator already agreed to,
                    // and the button that reaches this is itself the consent —
                    // shown next to what the certificate says.
                    RunMode::Resume { run, piv_pin } => {
                        if let Some(pin) = piv_pin {
                            executor.supply(pin);
                        }
                        executor.resume(&request, run, &mut recorder)
                    }
                };
                // Take the secrets out whatever happened: a failed run may still
                // have generated one, and it must not outlive this call.
                self.wizard.secrets = Some(crate::secret::ShowOnce::new(executor.take_secrets()));
                run
            };
            self.store = Some(recorder.store);
            result
        };

        self.wizard.stage = crate::app::WizardStage::Running;
        match outcome {
            Ok(run) => {
                let (done, failed, skipped, _) = run.tally();
                let tally = format!(
                    "run {:?}: {done} done, {failed} failed, {skipped} skipped",
                    run.status
                );
                self.status = match self.settle_key_status(serial, &run) {
                    Ok(true) => {
                        format!("{tally} — serial {serial} is bootstrapped and ready to hand over")
                    }
                    Ok(false) => tally,
                    Err(e) => {
                        // The run happened; the key is filed as something it is no
                        // longer. That has to be met here, not on the Distribution
                        // screen an hour later.
                        self.wizard.error = Some(e.clone());
                        format!("{tally} — but serial {serial} was not moved: {e}")
                    }
                };
                self.wizard.run = Some(run);
            }
            Err(e) => {
                self.wizard.error = Some(e.to_string());
                self.status = format!("the run did not complete: {e}");
            }
        }
        // A batch folds the outcome in here, on the same path a single run takes
        // — including the failure branch above. A batch that only heard about its
        // successes would report a clean sweep of a box where seven keys failed.
        if self.batch.position.is_some() {
            self.record_batch_outcome();
        }
        self.refresh();
    }

    /// Move a key to `Bootstrapped` when the run that just finished says it is.
    ///
    /// The lifecycle has always read `InStock → Bootstrapped → Distributed`, but
    /// nothing performed the first arrow: the only thing that moved a key was a
    /// button on the Inventory screen, so a key could be fully configured and
    /// still be filed as untouched stock. The operator met that hours later, on
    /// another screen, as a refused hand-over.
    ///
    /// Only a `Completed` run counts, which is the same line the engine already
    /// draws: a run with a required step unmet is marked `Failed` and audited
    /// `bootstrap.incomplete` — "the key is not ready to hand over".
    ///
    /// `Ok(true)` when the key moved, `Ok(false)` when there was nothing to move
    /// it to, `Err` when the move was refused — which the caller has to show,
    /// because a key filed as something it is no longer is the whole bug.
    pub fn settle_key_status(
        &mut self,
        serial: u32,
        run: &crate::domain::BootstrapRun,
    ) -> Result<bool, String> {
        if run.status != crate::domain::RunStatus::Completed {
            return Ok(false);
        }
        let Some(store) = &self.store else {
            return Ok(false);
        };
        let current = match store.key_by_serial(serial) {
            Ok(Some(found)) => found.status,
            Ok(None) => return Ok(false),
            Err(e) => {
                tracing::error!(event = "key.status.read.failed", serial, reason = %e);
                return Err(e.to_string());
            }
        };
        // Already there, or somewhere a run has no business overruling — a key
        // marked lost during a resume is not quietly returned to the shelf.
        if current == KeyStatus::Bootstrapped || !current.can_transition_to(KeyStatus::Bootstrapped)
        {
            return Ok(false);
        }
        if let Err(e) = store.set_key_status(serial, KeyStatus::Bootstrapped) {
            tracing::error!(event = "key.status.write.failed", serial, reason = %e);
            return Err(e.to_string());
        }
        self.record(
            "key.status_changed",
            &format!("serial:{serial}"),
            &format!(
                "to={} from={} run={}",
                KeyStatus::Bootstrapped.audit_name(),
                current.audit_name(),
                run.id
            ),
        );
        Ok(true)
    }

    /// The write transport for this build, if it has one.
    ///
    /// [`crate::device::composite::NativeBackend`] routes each applet to
    /// whatever is compiled in and answers `TransportUnavailable` for the rest,
    /// so a partially-implemented build produces a step that skips with a reason
    /// rather than a run that cannot start.
    fn write_backend(serial: u32) -> Option<Box<dyn crate::device::write::WriteBackend>> {
        crate::device::composite::NativeBackend::is_available().then(
            || -> Box<dyn crate::device::write::WriteBackend> {
                Box::new(crate::device::composite::NativeBackend::for_key(serial))
            },
        )
    }

    /// Dismiss the show-once panel, wiping the values.
    pub fn dismiss_secrets(&mut self) {
        if let Some(panel) = &mut self.wizard.secrets {
            let detail = panel.audit_detail();
            panel.dismiss();
            let serial = self.wizard.serial.trim().to_owned();
            self.record("secret.shown", &format!("serial:{serial}"), &detail);
        }
        self.wizard.secrets = None;
    }

    /// Carry a finished run straight to a hand-over
    /// (`features/gui-bootstrap-wizard.md` phase 6).
    ///
    /// The summary already shows the evidence; what was missing was the step after
    /// it. Retyping the serial and re-picking the holder on the Distribution screen
    /// is three chances to pick the wrong row, in the one place in this application
    /// where the wrong row means a security token recorded against somebody who does
    /// not have it.
    ///
    /// It fills the form and switches screens. It deliberately does **not** record
    /// anything: a hand-over is a statement that a person took physical possession
    /// of a key, and nothing the tool can see tells it that has happened.
    pub fn attach_run_to_handover(&mut self) {
        let Some(run) = self.wizard.run.clone() else {
            self.wizard.error = Some("there is no finished run to attach".into());
            return;
        };
        if run.status != crate::domain::RunStatus::Completed {
            self.wizard.error = Some(format!(
                "this run is {} — a key is handed over once its procedure has completed, so \
                 finish it first",
                format!("{:?}", run.status).to_lowercase()
            ));
            return;
        }
        let Some(key_index) = self
            .keys
            .iter()
            .position(|key| key.serial == run.key_serial)
        else {
            self.wizard.error = Some(format!(
                "serial {} is not in the inventory, so there is nothing to hand over",
                run.key_serial
            ));
            return;
        };

        self.dist_form = DistForm {
            key_index,
            // The run knows whose key it was prepared for. Left at whatever the form
            // happened to hold, this is precisely the field that gets mis-picked.
            holder_index: run
                .holder_id
                .and_then(|id| self.holders.iter().position(|holder| holder.id == id))
                .unwrap_or_default(),
            run_id: Some(run.id),
            ..DistForm::default()
        };
        self.tab = Tab::Distribution;
        self.status = format!(
            "hand-over ready for serial {}: the run that prepared it is attached — check the \
             holder and the receipt reference before recording it",
            run.key_serial
        );
    }

    /// Start again, leaving the recorded run on file.
    pub fn reset_wizard(&mut self) {
        self.dismiss_secrets();
        self.wizard.stage = crate::app::WizardStage::Selecting;
        self.wizard.findings.clear();
        self.wizard.run = None;
        self.wizard.plan.clear();
        self.wizard.error = None;
        // A pinned version belongs to the run that was being finished. Left in
        // place it would silently decide the *next* run's procedure, and a
        // superseded version is exactly the one a new run must not use.
        self.wizard.pinned_template = None;
    }

    /// Run the pre-flight against the current plan and move to the confirmation.
    ///
    /// Separate from `build_plan` because the operator may change the key after
    /// planning, and the checks are about *this key* rather than the procedure.
    /// Nothing is written here — that is the whole point of the stage.
    pub fn preflight(&mut self) {
        use crate::bootstrap::{AppletSnapshot, Preflight};

        if self.wizard.plan.is_empty() {
            self.wizard.error = Some("build a plan first".into());
            return;
        }
        let serial = self.wizard.serial.trim().parse::<u32>().ok();

        // Read the applets now (`features/device-detection.md` phase 4). Until this
        // existed the snapshot was always `default()`, so every check that depended on
        // applet state — including "this key is already configured" — was written and
        // never fired.
        //
        // Read-only: `get_info`, the PIV slot list, the retry counter, `ykman otp
        // info`. Nothing here writes, which is what makes it safe to run as the
        // operator moves through the wizard rather than only on a button.
        let applets = match serial {
            Some(serial) => self.read_applets(serial),
            None => AppletSnapshot::default(),
        };
        // Looked up after the recording inside that read, so the audit write does not have to
        // borrow around a reference into `self.keys`.
        let key = serial.and_then(|s| self.keys.iter().find(|k| k.serial == s));
        // A template with no rule is unrestricted, which is also what an
        // unselected one has to read as: the missing-template case is already the
        // wizard's own error, and inventing a rule here would report it twice.
        let applicability = self
            .selected_template()
            .map(|template| template.applicability.clone())
            .unwrap_or_default();
        self.wizard.findings = Preflight {
            commands: &self.wizard.plan,
            key,
            applets: &applets,
            can_write: Self::can_write_to_a_key(),
            applicability: &applicability,
        }
        .run();

        self.wizard.stage = crate::app::WizardStage::Confirming;
        self.status = crate::bootstrap::preflight::summarise(&self.wizard.findings);
    }

    /// Does this build have a transport that can write to a key?
    ///
    /// Compile-time, because it is a property of the build rather than of the
    /// session. A default build answers `false`, which is what makes the
    /// pre-flight block rather than letting an operator confirm a run that
    /// cannot happen.
    pub fn can_write_to_a_key() -> bool {
        crate::device::composite::NativeBackend::is_available()
    }

    /// Go back to selection, discarding the confirmation.
    ///
    /// Named for what it protects: a confirmation is for one plan on one key, so
    /// changing either has to invalidate it rather than carry it forward.
    pub fn cancel_confirmation(&mut self) {
        self.wizard.stage = crate::app::WizardStage::Selecting;
        self.wizard.findings.clear();
        self.status = "nothing was written".into();
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

        // An explicitly attached run wins over "the newest one on this serial": the
        // operator came here from that run's summary, and a key bootstrapped twice
        // has two.
        let run_id = self
            .dist_form
            .run_id
            .filter(|id| {
                self.runs
                    .iter()
                    .any(|r| r.id == *id && r.key_serial == key.serial)
            })
            .or_else(|| {
                self.dist_form.link_last_run.then(|| {
                    self.runs
                        .iter()
                        .find(|r| r.key_serial == key.serial)
                        .map(|r| r.id)
                })?
            });

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

        // The lifecycle is asked *before* the record is written. Asking after it
        // produced the worst of both answers: a hand-over on the register, the key
        // still sitting in stock, and a refusal the operator could do nothing
        // about. The key's own row decides it, not the cached list, because a
        // stale copy would decide it wrong.
        let current = match store.key_by_serial(key.serial) {
            Ok(Some(found)) => found.status,
            Ok(None) => {
                self.dist_form.error =
                    Some(format!("serial {} is not in the inventory", key.serial));
                return;
            }
            Err(e) => {
                self.dist_form.error = Some(e.to_string());
                return;
            }
        };
        if current != KeyStatus::Distributed && !current.can_transition_to(KeyStatus::Distributed) {
            self.dist_form.error = Some(format!(
                "serial {} is {} — a key is handed over once a bootstrap run has completed on \
                 it. Nothing was recorded: run the bootstrap, or mark the key bootstrapped on \
                 the Inventory screen, and record the hand-over then.",
                key.serial,
                current.label()
            ));
            tracing::warn!(
                event = "distribution.refused.lifecycle",
                serial = key.serial,
                status = current.audit_name()
            );
            return;
        }

        if let Err(e) = store.insert_distribution(&record) {
            self.dist_form.error = Some(e.to_string());
            return;
        }
        if let Err(e) = store.set_key_status(key.serial, KeyStatus::Distributed) {
            // The pre-flight above should have caught this; if the status moved
            // under us between the two reads, the refusal is still surfaced.
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
        // The return closes the chase for this hand-over's term and opens a
        // different question — whether the return itself is documented — so the
        // outstanding tally is worth recomputing now rather than at the next open.
        self.check_overdue_signatures();
        if self.settings.signatures.required
            && let Some(record) = self.distributions.iter().find(|d| d.id == id).cloned()
            && !self.signature_state(&record).is_settled()
        {
            // Said once, where the operator is looking: the key is back and the
            // record of who was responsible for it while it was out never arrived.
            // Not an error — nothing failed — but not silence either.
            self.status = format!(
                "serial {serial} returned. Note: no signed term was ever filed for this                  hand-over, and that gap is now permanent"
            );
        }
    }

    // ---------------------------------------------------------------- batches

    /// Parse the pairing list the operator pasted or loaded.
    ///
    /// All of it or none of it: [`crate::batch::pairing`] says why, and the
    /// refusal carries every bad line so one pass at the spreadsheet fixes them.
    pub fn load_pairing_list(&mut self) {
        self.batch.error = None;
        self.batch.pairs.clear();

        let known: std::collections::BTreeMap<String, uuid::Uuid> = self
            .holders
            .iter()
            .map(|holder| (holder.email.to_ascii_lowercase(), holder.id))
            .collect();

        match crate::batch::pairing::parse(&self.batch.pairing_text, &known) {
            Ok(pairs) => {
                self.batch.notice = Some(format!(
                    "list accepted — {}",
                    crate::batch::pairing::summarise(&pairs)
                ));
                self.batch.pairs = pairs;
            }
            Err(e) => self.batch.error = Some(e.to_string()),
        }
    }

    /// Start a batch against the selected template.
    pub fn start_batch(&mut self) {
        use crate::batch::{Batch, Shape};

        self.batch.error = None;
        self.batch.notice = None;

        let Some(template) = self.selected_template().cloned() else {
            self.batch.error = Some("choose a procedure before starting a batch".into());
            return;
        };
        if self.batch.shape.needs_pairing_list() && self.batch.pairs.is_empty() {
            self.batch.error = Some(
                "an assigned batch needs its pairing list first: every address is checked before \
                 the first key is touched, not as each one comes up"
                    .into(),
            );
            return;
        }

        let batch = match self.batch.shape {
            Shape::StockPreparation => Batch::stock(
                &template.id,
                &template.version,
                &self.operator,
                self.batch.planned.max(1),
            ),
            Shape::AssignedEnrolment => Batch::assigned(
                &template.id,
                &template.version,
                &self.operator,
                &self.batch.pairs,
            ),
        };

        let Some(store) = &self.store else {
            self.batch.error = Some("no database is open".into());
            return;
        };
        if let Err(e) = store.insert_batch(&batch) {
            // Refused rather than started in memory: a batch that cannot be
            // written cannot be resumed, and an unresumable batch of fifty keys
            // is the failure this whole feature exists to prevent.
            self.batch.error = Some(format!("the batch could not be started: {e}"));
            return;
        }

        self.record(
            "batch.started",
            &format!("batch:{}", batch.id),
            &batch.audit_detail(),
        );
        self.batch.notice = Some(format!(
            "batch started — {}. Insert the first key.",
            batch.tally().describe()
        ));
        self.batch.current = Some(batch);
        self.batch.position = None;
        self.batch.open = true;
    }

    /// Offer a key to the batch, and set the wizard up for it.
    ///
    /// Returns true when the run may proceed. A duplicate is refused *and*
    /// audited: the same key inserted twice is a real mistake, and one nobody
    /// would otherwise be able to reconstruct from the trail.
    pub fn present_batch_key(&mut self, serial: u32) -> bool {
        use crate::batch::Presented;

        self.batch.error = None;
        let Some(batch) = self.batch.current.as_mut() else {
            self.batch.error = Some("no batch is in progress".into());
            return false;
        };

        let presented = batch.present(serial);
        let id = batch.id;
        match presented {
            Presented::Ready { position } => {
                let entry = batch.entries[position].clone();
                // Written before the run, so a crash mid-run leaves the register
                // saying which key was in the reader.
                if let Some(store) = &self.store
                    && let Err(e) = store.record_batch_entry(id, &entry)
                {
                    tracing::error!(event = "batch.entry.save.failed", reason = %e);
                    self.batch.error = Some(format!("the batch could not be updated: {e}"));
                    return false;
                }
                self.batch.position = Some(position);
                self.batch.notice = Some(presented.describe(serial));

                // The wizard does the rest, exactly as it would for one key.
                self.wizard.serial = serial.to_string();
                if let Some(holder_id) = entry.holder_id
                    && let Some(index) = self.holders.iter().position(|h| h.id == holder_id)
                {
                    self.wizard.holder_index = index;
                }
                true
            }
            Presented::Duplicate { .. } | Presented::Full => {
                let detail = format!("batch={id} serial={serial}");
                let refusal = presented.describe(serial);
                if matches!(presented, Presented::Duplicate { .. }) {
                    self.record("batch.key.duplicate", &format!("serial:{serial}"), &detail);
                }
                self.batch.error = Some(refusal);
                false
            }
        }
    }

    /// Fold the run that just finished back into the batch.
    ///
    /// Called after the wizard settles, whichever way it went: a batch that only
    /// recorded its successes would report a clean sweep of a box where seven
    /// keys failed.
    pub fn record_batch_outcome(&mut self) {
        use crate::batch::Outcome;
        use crate::domain::RunStatus;

        let Some(position) = self.batch.position else {
            return;
        };
        let run = self.wizard.run.clone();
        let error = self.wizard.error.clone();

        let outcome = match (&run, &error) {
            (Some(run), _) if run.status == RunStatus::Completed => Outcome::Done { run: run.id },
            (Some(run), _) => Outcome::Failed {
                run: Some(run.id),
                reason: format!(
                    "run {:?}: {}",
                    run.status,
                    error.clone().unwrap_or_else(|| run.summary())
                ),
            },
            (None, Some(reason)) => Outcome::Failed {
                run: None,
                reason: reason.clone(),
            },
            // Nothing to fold in: the wizard has not run yet.
            (None, None) => return,
        };

        let (id, entry, complete, tally) = {
            let Some(batch) = self.batch.current.as_mut() else {
                return;
            };
            batch.record(position, outcome);
            (
                batch.id,
                batch.entries[position].clone(),
                batch.is_complete(),
                batch.tally(),
            )
        };

        let event = match entry.state {
            crate::batch::EntryState::Done => "batch.key.done",
            _ => "batch.key.failed",
        };
        let detail = self
            .batch
            .current
            .as_ref()
            .map(|batch| batch.key_audit_detail(position))
            .unwrap_or_default();

        if let Some(store) = &self.store
            && let Err(e) = store.record_batch_entry(id, &entry)
        {
            // Loud: the key was written and the batch cannot say so, which is
            // exactly the bookkeeping this feature took over from the operator.
            tracing::error!(event = "batch.entry.save.failed", reason = %e);
            self.batch.error = Some(format!("the batch could not be updated: {e}"));
        }
        let target = entry
            .serial
            .map(|serial| format!("serial:{serial}"))
            .unwrap_or_else(|| format!("batch:{id}"));
        self.record(event, &target, &detail);

        self.batch.position = None;
        self.batch.notice = Some(tally.describe());

        if complete {
            if let Some(store) = &self.store
                && let Err(e) = store.finish_batch(id, chrono::Utc::now())
            {
                tracing::error!(event = "batch.finish.save.failed", reason = %e);
            }
            self.record(
                "batch.finished",
                &format!("batch:{id}"),
                &tally.audit_detail(),
            );
            self.batch.notice = Some(format!("batch finished — {}", tally.describe()));
        }
    }

    /// Pass over the position in hand — the key is not in the box, or the
    /// operator is coming back to it.
    pub fn skip_batch_key(&mut self, reason: &str) {
        use crate::batch::Outcome;

        let Some(position) = self.batch.position else {
            return;
        };
        let reason = if reason.trim().is_empty() {
            "skipped by the operator".to_owned()
        } else {
            reason.trim().to_owned()
        };

        let (id, entry, complete, tally) = {
            let Some(batch) = self.batch.current.as_mut() else {
                return;
            };
            batch.record(position, Outcome::Skipped { reason });
            (
                batch.id,
                batch.entries[position].clone(),
                batch.is_complete(),
                batch.tally(),
            )
        };
        let detail = self
            .batch
            .current
            .as_ref()
            .map(|batch| batch.key_audit_detail(position))
            .unwrap_or_default();

        if let Some(store) = &self.store {
            let _ = store.record_batch_entry(id, &entry);
            if complete {
                let _ = store.finish_batch(id, chrono::Utc::now());
            }
        }
        self.record("batch.key.skipped", &format!("batch:{id}"), &detail);
        if complete {
            self.record(
                "batch.finished",
                &format!("batch:{id}"),
                &tally.audit_detail(),
            );
        }
        self.batch.position = None;
        self.batch.notice = Some(tally.describe());
    }

    /// Write the consignment terms the open batch owes, in one action
    /// (`features/bulk-enrollment.md` phase 7, `features/receipts-and-terms.md`
    /// phase 7).
    ///
    /// Each term is rendered by the same `term::render_term_pdf` the single
    /// hand-over uses, from the same context: a batch is not a second way of
    /// producing a term. What it removes is the fifty trips through the
    /// Distribution screen, which is where the fiftieth term stops getting
    /// generated at all.
    ///
    /// One **language** for the set, the one chosen on the term panel. Holders do
    /// not carry a language on the register, so per-holder wording is not
    /// something this can derive — and guessing it from a name or an address is
    /// exactly the guess a consignment document should not make. A unit that
    /// needs two languages runs the action twice.
    pub fn generate_batch_terms(&mut self, into: &Path) -> Option<PathBuf> {
        use crate::batch::documents;
        use crate::term::{TermContext, choose_template};

        self.batch.error = None;
        let Some(batch) = self.batch.current.clone() else {
            self.batch.error = Some("no batch is open".into());
            return None;
        };

        let plan = match documents::plan(&batch) {
            Ok(plan) => plan,
            Err(refusal) => {
                self.batch.error = Some(refusal.message().to_owned());
                return None;
            }
        };
        if plan.is_empty() {
            self.batch.error = Some(format!(
                "no key in this batch has finished a run yet, so there is nothing to \
                 generate — {}",
                plan.describe()
            ));
            return None;
        }

        let language = self.term_panel.language.clone();
        // Cloned before anything borrows `self` mutably, and once for the whole
        // set: fifty terms from one template version is what makes them a set.
        let Some(template) =
            choose_template(&self.term_templates, crate::term::CONSIGNMENT_ID, &language).cloned()
        else {
            self.batch.error = Some(format!("no consignment term template for `{language}`"));
            return None;
        };

        let now = chrono::Utc::now();
        let directory = into.join(documents::directory_name(&batch, now));
        if let Err(e) = std::fs::create_dir_all(&directory) {
            let message = format!("could not create {}: {e}", directory.display());
            tracing::error!(event = "batch.terms.failed", reason = %e);
            self.batch.error = Some(message.clone());
            self.status = message;
            return None;
        }

        let mut written = 0usize;
        // Problems are collected rather than returned on: a term that cannot be
        // produced for one holder is not a reason to leave the other forty-nine
        // unwritten, and the operator needs the whole list in one pass.
        let mut problems: Vec<String> = Vec::new();

        for planned in &plan.planned {
            let Some(holder) = self
                .holders
                .iter()
                .find(|h| h.id == planned.holder_id)
                .cloned()
            else {
                problems.push(format!(
                    "serial {}: the holder is no longer in the register",
                    planned.serial
                ));
                continue;
            };
            let key = self
                .keys
                .iter()
                .find(|k| k.serial == planned.serial)
                .cloned()
                .unwrap_or_else(|| {
                    YubiKeyRecord::from_serial(planned.serial, SerialSource::ManualEntry)
                });
            // The hand-over record when the key has already been given out, and
            // `None` while it has not — which is the ordinary case here and is
            // what leaves the hand-over lines out of a term that is about to be
            // signed. The same `Option` the single path passes.
            let record = self
                .distributions
                .iter()
                .find(|d| d.key_serial == planned.serial && d.returned_at.is_none())
                .cloned();
            let run = batch
                .entries
                .get(planned.position)
                .and_then(|entry| entry.run_id)
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
                record.as_ref(),
                &applied,
                &custody,
                &self.operator,
                &self.org,
            );

            let Some(bytes) = self.term_pdf(&template, &ctx) else {
                problems.push(format!(
                    "serial {}: the PDF could not be produced",
                    planned.serial
                ));
                continue;
            };
            let path = directory.join(planned.file_name("pdf"));
            if let Err(e) = std::fs::write(&path, &bytes) {
                tracing::error!(event = "batch.terms.failed", reason = %e);
                problems.push(format!(
                    "serial {}: could not write the file ({e})",
                    planned.serial
                ));
                continue;
            }

            written += 1;
            // Audited per term, with the same event the single path writes. A
            // batch is not an excuse for a coarser trail: "fifty terms were
            // generated" cannot answer which holder's term was produced from
            // which template version.
            self.record(
                "term.generated",
                &format!("serial:{}", planned.serial),
                &format!(
                    "holder={} language={} template={}@{} batch={}",
                    holder.email, template.language, template.id, template.version, batch.id
                ),
            );
        }

        self.record(
            "batch.terms",
            &format!("batch:{}", batch.id),
            &format!(
                "documents={written} skipped={} refused={} path={}",
                plan.skipped.len(),
                problems.len(),
                directory.display()
            ),
        );

        let mut notice = format!("{written} term(s) written to {}", directory.display());
        if !plan.skipped.is_empty() {
            notice.push_str(&format!(
                " — {} position(s) produced nothing, the first being {}",
                plan.skipped.len(),
                plan.skipped[0].describe
            ));
        }
        self.batch.notice = Some(notice.clone());
        self.status = notice;
        if !problems.is_empty() {
            self.batch.error = Some(problems.join("; "));
        }
        Some(directory)
    }

    /// Ask where the terms go, then write them.
    pub fn generate_batch_terms_interactive(&mut self) {
        if let Some(directory) = self.choose_export_directory() {
            self.generate_batch_terms(&directory);
        }
    }

    /// Read the batches somebody could pick up.
    pub fn reload_batches(&mut self) {
        let Some(store) = &self.store else { return };
        match store.unfinished_batches() {
            Ok(batches) => self.batch.resumable = batches,
            Err(e) => tracing::warn!(event = "batch.read.failed", reason = %e),
        }
    }

    /// Pick a batch up where it was left.
    pub fn resume_batch(&mut self, id: uuid::Uuid) {
        self.batch.error = None;
        let Some(batch) = self.batch.resumable.iter().find(|b| b.id == id).cloned() else {
            self.batch.error = Some("that batch is no longer on the register".into());
            return;
        };

        // The procedure comes from the batch, not from whatever is selected: a
        // resumed batch must finish the box with the version it started.
        if let Some(index) = self
            .templates
            .iter()
            .position(|t| t.id == batch.template_id && t.version == batch.template_version)
        {
            self.wizard.template_index = index;
        } else {
            self.batch.error = Some(format!(
                "this batch applied {} version {}, which is not among the procedures this build \
                 offers — the rest of the box would get a different one",
                batch.template_id, batch.template_version
            ));
            return;
        }

        self.record(
            "batch.resumed",
            &format!("batch:{id}"),
            &batch.resume_audit_detail(),
        );
        self.batch.notice = Some(format!(
            "resumed — {}. Insert the next key.",
            batch.tally().describe()
        ));
        self.batch.shape = batch.shape;
        self.batch.current = Some(batch);
        self.batch.position = None;
        self.batch.open = true;
    }

    /// Put the batch down without finishing it. It stays resumable.
    pub fn close_batch(&mut self) {
        self.batch.current = None;
        self.batch.position = None;
        self.batch.open = false;
        self.batch.notice = None;
        self.reload_batches();
    }

    // ---------------------------------------------------------------- reports

    /// The newest version of each template id, retired ones included.
    ///
    /// Read from the **catalogue** rather than from the wizard's list, because a
    /// retired template is still on the register: a run against it is explicable,
    /// and calling it "not on the register" in the compliance report would send
    /// somebody looking for a version that is right there.
    fn newest_template_versions(&self) -> std::collections::BTreeMap<String, String> {
        let mut newest: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for stored in &self.template_catalogue {
            let id = &stored.template.id;
            let version = &stored.template.version;
            let replace = match newest.get(id) {
                Some(current) => {
                    crate::versioning::version_order(version)
                        > crate::versioning::version_order(current)
                }
                None => true,
            };
            if replace {
                newest.insert(id.clone(), version.clone());
            }
        }
        newest
    }

    /// Build one report from the register as it stands.
    ///
    /// Everything except the audit extract comes from the cached views the
    /// screens already read; the remediations are fetched here because no screen
    /// holds them for the whole register, and the reconciliation report needs
    /// them for every returned key.
    ///
    /// Takes no panel state and changes none, so the bundle can build nine of
    /// these without the screen flickering through all nine.
    pub fn build_report(
        &self,
        kind: crate::report::ReportKind,
        now: chrono::DateTime<chrono::Utc>,
    ) -> std::result::Result<crate::report::Report, String> {
        use crate::report::{Dataset, ReportKind, Verification, audit_extract, build};

        let Some(store) = &self.store else {
            return Err("no database is open".into());
        };

        if kind == ReportKind::AuditExtract {
            let (entries, verification) = match (store.audit_trail(), store.verify_audit()) {
                (Ok(entries), Ok(count)) => (entries, Verification::Verified { entries: count }),
                // The extract is still produced when the chain does not verify,
                // and says so at the top: refusing would leave the one person who
                // needs to investigate with nothing to look at.
                (Ok(entries), Err(e)) => (
                    entries,
                    Verification::Broken {
                        reason: e.to_string(),
                    },
                ),
                (Err(e), _) => return Err(format!("could not read the trail: {e}")),
            };
            return Ok(audit_extract(
                &entries,
                &self.reports.audit_filter,
                &verification,
                &self.operator,
                now,
            ));
        }

        let remediations = store
            .remediations()
            .map_err(|e| format!("could not read the register: {e}"))?;

        let data = Dataset {
            keys: &self.keys,
            holders: &self.holders,
            distributions: &self.distributions,
            runs: &self.runs,
            remediations: &remediations,
            newest_template_version: self.newest_template_versions(),
            filed: self.filed_documents.clone(),
            policy: self.settings.signatures.clone(),
            now,
        };

        Ok(match kind {
            ReportKind::CertificateExpiry => {
                crate::report::certificate_expiry(&data, &self.operator, self.reports.expiry_days)
            }
            kind => build(kind, &data, &self.operator),
        })
    }

    /// Build the selected report and put it on the screen.
    pub fn generate_report(&mut self) {
        self.reports.error = None;
        self.reports.current = None;

        match self.build_report(self.reports.kind, chrono::Utc::now()) {
            Ok(report) => {
                self.status = format!("{} generated", report.provenance());
                // The format may not survive the report: PDF is offered for two
                // of them only, and a stale choice would export the wrong thing.
                if !crate::report::export::Format::available_for(report.kind)
                    .contains(&self.reports.format)
                {
                    self.reports.format = crate::report::export::Format::Csv;
                }
                self.reports.current = Some(report);
            }
            Err(reason) => self.reports.error = Some(reason),
        }
    }

    /// Generate every report at once into a dated folder, with a manifest
    /// (`features/reports-and-export.md` phase 8).
    ///
    /// One moment in time for all of them, which is the point: nine files
    /// generated over nine clicks are nine different answers, and a folder of
    /// those cannot be reconciled against anything. Returns the folder written.
    pub fn export_bundle(&mut self, into: &Path) -> Option<PathBuf> {
        use crate::report::bundle;

        self.reports.error = None;
        let now = chrono::Utc::now();
        let directory = into.join(bundle::directory_name(now));
        if let Err(e) = std::fs::create_dir_all(&directory) {
            let message = format!("could not create {}: {e}", directory.display());
            tracing::error!(event = "export.bundle.failed", reason = %e);
            self.reports.error = Some(message.clone());
            self.status = message;
            return None;
        }

        // Each report is built once even when it leaves in two formats: the
        // compliance report and the extract go out as both a spreadsheet and a
        // document, and building each twice would let the two disagree.
        let mut built: std::collections::BTreeMap<
            crate::report::ReportKind,
            crate::report::Report,
        > = std::collections::BTreeMap::new();
        let mut written: Vec<(String, crate::report::Report)> = Vec::new();

        for (kind, format) in bundle::CONTENTS {
            let report = match built.get(&kind) {
                Some(report) => report.clone(),
                None => match self.build_report(kind, now) {
                    Ok(report) => {
                        built.insert(kind, report.clone());
                        report
                    }
                    Err(reason) => {
                        self.reports.error = Some(reason);
                        return None;
                    }
                },
            };

            let name = report.file_name(format);
            let path = directory.join(&name);
            let content = crate::report::export::render(&report, format);
            if let Err(e) = std::fs::write(&path, &content) {
                let message = format!("could not write {}: {e}", path.display());
                tracing::error!(event = "export.bundle.failed", reason = %e);
                self.reports.error = Some(message.clone());
                self.status = message;
                return None;
            }
            // Audited per file, not once for the folder: `export.taken` says what
            // left and where it went, and a single entry for nine files would
            // answer neither question.
            self.record(
                "export.taken",
                &format!("report:{}", report.kind.slug()),
                &report.audit_detail(format, &path),
            );
            written.push((name, report));
        }

        let manifest_entries: Vec<(String, &crate::report::Report)> = written
            .iter()
            .map(|(name, report)| (name.clone(), report))
            .collect();
        let manifest = bundle::manifest(&manifest_entries, &self.operator, now);
        let manifest_path = directory.join(bundle::MANIFEST);
        if let Err(e) = std::fs::write(&manifest_path, manifest) {
            // The reports are already written and audited; a missing manifest is
            // a folder that is harder to read, not a failed export.
            tracing::error!(event = "export.manifest.failed", reason = %e);
            self.reports.error = Some(format!(
                "the reports were written, but {} could not be: {e}",
                manifest_path.display()
            ));
        }

        self.record(
            "export.bundle",
            "report:bundle",
            &format!("files={} path={}", written.len(), directory.display()),
        );
        self.status = format!(
            "{} report file(s) written to {}",
            written.len(),
            directory.display()
        );
        Some(directory)
    }

    /// Ask for the folder the bundle goes into, then write it.
    pub fn export_bundle_interactive(&mut self) {
        if let Some(directory) = self.choose_export_directory() {
            self.export_bundle(&directory);
        }
    }

    #[cfg(feature = "file-dialog")]
    fn choose_export_directory(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("Where should the report bundle go?")
            .pick_folder()
    }

    #[cfg(not(feature = "file-dialog"))]
    fn choose_export_directory(&mut self) -> Option<PathBuf> {
        Some(
            self.config
                .path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
        )
    }

    /// Write the generated report to a file the operator chooses, and audit it.
    ///
    /// The audit entry is the point of the operation as much as the file is: NRM
    /// §5.3.1 treats the export of critical data as an action somebody must be
    /// able to review afterwards, and three of these reports are a list of people
    /// and the credentials they hold.
    pub fn export_report(&mut self) {
        self.reports.error = None;
        let Some(report) = self.reports.current.as_ref() else {
            self.reports.error = Some("generate a report before exporting it".into());
            return;
        };
        let suggested = report.file_name(self.reports.format);
        // A cancelled dialog is not an error and says nothing.
        let Some(path) = self.choose_export_path(&suggested) else {
            return;
        };
        self.write_report(&path);
    }

    /// Write the generated report to `path` and audit it.
    ///
    /// Split from [`Self::export_report`] because the dialog is the untestable
    /// half and this is the operation: the behaviour suite drives the write and
    /// the audit entry without a file chooser anywhere near it.
    pub fn write_report(&mut self, path: &Path) -> bool {
        let Some(report) = self.reports.current.clone() else {
            self.reports.error = Some("generate a report before exporting it".into());
            return false;
        };
        let format = self.reports.format;
        let content = crate::report::export::render(&report, format);

        if let Err(e) = std::fs::write(path, &content) {
            let message = format!("could not write {}: {e}", path.display());
            tracing::error!(event = "export.write.failed", reason = %e);
            self.reports.error = Some(message.clone());
            self.status = message;
            return false;
        }

        self.record(
            "export.taken",
            &format!("report:{}", report.kind.slug()),
            &report.audit_detail(format, path),
        );
        self.status = format!(
            "exported {} row(s) to {}",
            report.rows.len(),
            path.display()
        );
        true
    }

    #[cfg(feature = "file-dialog")]
    fn choose_export_path(&mut self, suggested: &str) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("Export report")
            .set_file_name(suggested)
            .save_file()
    }

    #[cfg(not(feature = "file-dialog"))]
    fn choose_export_path(&mut self, suggested: &str) -> Option<PathBuf> {
        // Without a dialog the file goes next to the database — a location the
        // operator already knows, and one they chose.
        Some(
            self.config
                .path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(suggested),
        )
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
        // Notices a file server that has gone away under a share-hosted register,
        // rather than letting the next write be the thing that finds out.
        self.tick_share_health();
        // A confirmed FIDO2 reset waiting for its key to come back. First, because
        // it may fire the run this frame, and because it decides whether the
        // background watch may run at all.
        if self.reset.handshake.is_some() {
            self.tick_power_cycle();
            // The window is five seconds wide and egui sleeps when idle: without
            // this the port would be polled into a frame nobody paints.
            ui.ctx().request_repaint_after(
                self.reset
                    .presence
                    .as_ref()
                    .map(|p| p.interval())
                    .unwrap_or(std::time::Duration::from_millis(200)),
            );
        }
        // Start or stop the hardware watch for the screen we are on, then take one
        // snapshot for this frame. Both are cheap and neither touches the hardware:
        // the looking happens on the watch's own thread.
        self.sync_device_watch();
        if self.poll_device_watch() {
            // Something was plugged in or pulled out. Repaint now rather than
            // waiting for the next mouse move, or a key inserted while nobody
            // touches the machine appears only when they do.
            ui.ctx().request_repaint();
        }
        if self.watch.is_some() {
            // egui sleeps when idle, so without this the watch would publish into a
            // window nobody repaints. One repaint per interval, not per frame.
            ui.ctx().request_repaint_after(
                self.watch
                    .as_ref()
                    .map(|w| w.interval())
                    .unwrap_or(std::time::Duration::from_secs(1)),
            );
        }
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
        self.about_box(ui);

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
                            Tab::Reports => crate::ui::reports::show(self, ui),
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
    /// The About box (`features/application-icon.md` phase 7).
    ///
    /// The mark, what this build is, and **the diagnostic report** — the same text
    /// `--diagnose` prints, which `docs/operations.md` already calls the first thing
    /// to attach to a support request. Reused rather than reimplemented: an About box
    /// that listed the version and the features by hand would be a second answer to
    /// the same question, and the two would drift.
    ///
    /// Copyable, because the point of showing it is that somebody sends it on. An
    /// operator who cannot select it retypes it, and a retyped diagnostic is worse
    /// than none.
    ///
    /// The report is gathered **when the box is opened**, not while it is painted:
    /// gathering reads the filesystem and enumerates the cameras, and doing that once
    /// per frame would be sixty camera enumerations a second for a panel nobody is
    /// touching. A support report is a snapshot of the moment somebody asked for it
    /// anyway — reopening the box takes a fresh one.
    fn about_box(&mut self, ui: &mut egui::Ui) {
        let Some(report) = self.about.clone() else {
            return;
        };
        let mut open = true;
        let mut copy: Option<String> = None;

        elegance::Modal::new("about", &mut open)
            .heading("YubiKey Distribution Manager")
            .subtitle(format!("version {}", crate::build_id()))
            .max_width(620.0)
            .show(ui.ctx(), |ui| {
                ui.vertical_centered(|ui| {
                    crate::ui::app_icon(ui, 88.0);
                });
                ui.add_space(12.0);

                crate::ui::hint(
                    ui,
                    "What this build is and what it can reach — the same report as \
                     `yk-dist-manager --diagnose`, and the first thing to attach to a support \
                     request.",
                );
                ui.add_space(10.0);

                egui::ScrollArea::vertical()
                    .max_height(280.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&report)
                                    .monospace()
                                    .size(elegance::Theme::current(ui.ctx()).typography.monospace),
                            )
                            // Selectable, so it can be copied by hand as well as by
                            // the button — the operator may want one line of it.
                            .selectable(true),
                        );
                    });

                ui.add_space(12.0);
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add(elegance::Button::new("Copy the report").outline())
                        .on_hover_text("paste it into the ticket")
                        .clicked()
                    {
                        copy = Some(report.clone());
                    }
                    crate::ui::faint(
                        ui,
                        "The mark is deliberately institution-neutral; replacing it is one SVG \
                         and `make icons`.",
                    );
                });
            });

        if let Some(report) = copy {
            ui.ctx().copy_text(report);
            self.status = "diagnostic report copied to the clipboard".into();
        }
        if !open {
            self.about = None;
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let theme = elegance::Theme::current(ui.ctx());

        egui::Panel::top("tabs")
            .frame(gutter_frame(egui::Frame::side_top_panel(ui.style())))
            .show(ui, |ui| {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    // Small, beside the name: this is the row an operator glances at
                    // to tell this window from the register, the term and a terminal
                    // during a hand-over, which is the reason the icon exists at all
                    // (`features/application-icon.md`).
                    crate::ui::app_icon(ui, theme.typography.heading + 10.0);
                    ui.add_space(8.0);
                    ui.add(egui::Label::new(
                        egui::RichText::new("YubiKey Distribution Manager")
                            .size(theme.typography.heading + 2.0)
                            .color(theme.palette.text)
                            .strong(),
                    ));
                    ui.add_space(8.0);
                    if ui
                        .add(
                            elegance::Badge::new(crate::VERSION, elegance::BadgeTone::Neutral)
                                .preserve_case(),
                        )
                        .on_hover_text(format!(
                            "build {} — what this build is, and what it can reach. Click",
                            crate::build_id()
                        ))
                        .interact(egui::Sense::click())
                        .clicked()
                    {
                        // The version badge *is* the About affordance: it is already
                        // the thing somebody points at when asked "which version are
                        // you running?", and a second button saying About would be a
                        // second place to look for the same answer.
                        //
                        // Gathered here, on the click, for the reason on the field.
                        self.about = Some(crate::diagnostics::Report::gather().render());
                    }
                });
                ui.add_space(6.0);

                let mut index = self.tab.index();
                ui.add(elegance::TabBar::new(
                    &mut index,
                    Tab::ALL.map(|tab| tab.label()),
                ));
                self.tab = Tab::from_index(index);

                // Who else is in the register, under the tabs rather than on one
                // screen: the operator who needs to know is the one about to
                // start a hand-over, and they may be on any of them.
                if let Some(line) = self.presence.describe(chrono::Utc::now()) {
                    ui.add_space(6.0);
                    crate::ui::notice(ui, elegance::CalloutTone::Warning, &line);
                }

                // A shortcut nobody knows about is not a shortcut. The log
                // toggle carries an error count so a failure is visible without
                // opening the panel — text, not a coloured dot.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (_, _, warnings, errors) = self.log.counts();
                    let label = match (errors, warnings) {
                        (0, 0) => "Log".to_owned(),
                        (0, w) => format!("Log ({w} warning)"),
                        (e, _) => format!("Log ({e} ERROR)"),
                    };
                    if ui
                        .selectable_label(self.log_panel_open, label)
                        .on_hover_text("⌘L / Ctrl+L")
                        .clicked()
                    {
                        self.log_panel_open = !self.log_panel_open;
                    }
                    ui.label(
                        egui::RichText::new("⌘R refresh · ⌘D detect · ⌘± size")
                            .small()
                            .weak(),
                    )
                    .on_hover_text("Ctrl on Windows and Linux. ⌘0 resets the size.");
                });
            });
    }

    /// Who is operating, where the database lives, and the last outcome.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        use crate::status::Severity;
        use elegance::IndicatorState;

        let theme = elegance::Theme::current(ui.ctx());
        let severity = crate::status::classify(&self.status);

        // Keyboard flow (`features/gui-shell.md` phase 6). These matter for a
        // repeated hand-over: an operator with a key in one hand should not have
        // to find a button with the mouse for the two things they do fifty times
        // a day.
        //
        // Read before anything paints, so a shortcut is not swallowed by whatever
        // widget happens to have focus.
        {
            let pressed = |ui: &egui::Ui, key: egui::Key| {
                ui.ctx()
                    .input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, key))
            };
            if pressed(ui, egui::Key::R) {
                self.refresh();
                self.status = "refreshed".into();
            }
            if pressed(ui, egui::Key::D) {
                self.detect_keys();
            }
            if pressed(ui, egui::Key::L) {
                self.log_panel_open = !self.log_panel_open;
            }
            // Font scaling, for the same reason the contrast pass exists: the
            // register is read at a desk by whoever is on shift.
            if pressed(ui, egui::Key::Plus) || pressed(ui, egui::Key::Equals) {
                let zoom = (ui.ctx().zoom_factor() + 0.1).min(2.0);
                ui.ctx().set_zoom_factor(zoom);
            }
            if pressed(ui, egui::Key::Minus) {
                let zoom = (ui.ctx().zoom_factor() - 0.1).max(0.8);
                ui.ctx().set_zoom_factor(zoom);
            }
            if pressed(ui, egui::Key::Num0) {
                ui.ctx().set_zoom_factor(1.0);
            }
        }

        // Remember where the window is and which screen is open. Compared before
        // writing, because this runs every frame and the settings file is on
        // disk — a per-frame write would be a per-frame fsync, on a share.
        {
            let size = ui.max_rect().size();
            let maximised = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
            let tab = self.tab.label().to_owned();
            let current = crate::settings::WindowState {
                width: size.x,
                height: size.y,
                tab,
                maximised,
            };
            if current != self.settings.window {
                self.settings.window = current;
                // Cosmetic: a settings write that fails costs the operator a
                // window position, so it is logged rather than surfaced.
                if let Err(e) = self.settings.save() {
                    tracing::warn!(event = "settings.window.save_failed", reason = %e);
                }
            }
        }

        // The log panel, above the status bar so an error is one click from the
        // line that mentions it (`features/gui-shell.md` phase 8).
        if self.log_panel_open {
            egui::Panel::bottom("log-panel")
                .resizable(true)
                .frame(gutter_frame(egui::Frame::side_top_panel(ui.style())))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Log");
                        ui.label(self.log.describe());
                        ui.add_space(12.0);
                        egui::ComboBox::from_id_salt("log-level")
                            .selected_text(self.log_min_level.label())
                            .show_ui(ui, |ui| {
                                for level in [
                                    crate::logbuf::Level::Debug,
                                    crate::logbuf::Level::Info,
                                    crate::logbuf::Level::Warn,
                                    crate::logbuf::Level::Error,
                                ] {
                                    ui.selectable_value(
                                        &mut self.log_min_level,
                                        level,
                                        level.label(),
                                    );
                                }
                            });
                        // The whole point of the panel: turn "it didn't work"
                        // into a paste into a ticket.
                        if ui.button("Copy all").clicked() {
                            let block = self.log.to_clipboard(self.log_min_level);
                            ui.ctx().copy_text(block);
                            self.status = "log copied to the clipboard".into();
                        }
                        if ui.button("Clear").clicked() {
                            self.log.clear();
                        }
                        if ui.button("Close").clicked() {
                            self.log_panel_open = false;
                        }
                    });
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .stick_to_bottom(true)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for line in self.log.lines(self.log_min_level) {
                                // Level as text as well as colour, so severity
                                // survives a monochrome screen (phase 10).
                                ui.horizontal_wrapped(|ui| {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(line.level.label())
                                                .monospace()
                                                .strong(),
                                        )
                                        .selectable(true),
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(&line.text).monospace(),
                                        )
                                        .selectable(true),
                                    );
                                });
                            }
                        });
                });
        }

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
                    // Which transport is reading the hardware. In the status bar because
                    // it is the first thing to establish when a key behaves differently
                    // from the last one — and because an operator who overrode it in
                    // Settings needs to see, without going back there, whether the
                    // override took (`features/native-device-transport.md` phase 6).
                    pill = pill.item(
                        if self.transport.disabled {
                            "via: none".to_owned()
                        } else {
                            format!("via: {}", self.transport.transport.label())
                        },
                        if self.transport.disabled {
                            // Amber, not red: nothing is broken. The register works,
                            // and keys can still be recorded by serial — what is
                            // unavailable is reading from hardware.
                            IndicatorState::Connecting
                        } else {
                            IndicatorState::On
                        },
                    );
                    // What is plugged in, while something is watching for it. In the
                    // status bar rather than only on the screen that owns the list,
                    // because "which key is this application about to act on" is the
                    // question a wrong answer to is most expensive — and the wizard
                    // is two clicks from writing to it.
                    if self.watch.is_some() {
                        pill = pill.item(
                            self.attached.describe(),
                            if self.attached.is_ambiguous() && self.selected_serial.is_none() {
                                // Amber, not green: something is attached and the
                                // application is waiting to be told which one. The
                                // words say it too — `Attached::describe` ends in
                                // "choose one" — because a coloured dot is not a
                                // message (`gui-shell` phase 10).
                                IndicatorState::Connecting
                            } else {
                                IndicatorState::On
                            },
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
