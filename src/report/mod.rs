//! The questions the register exists to answer, answered as tables
//! (`features/reports-and-export.md`).
//!
//! Six reports over the records, plus the audit extract. Each one is **derived**
//! on demand from what is already stored — there is no report table, no cached
//! aggregate and nothing to refresh. That is the same choice
//! [`crate::receipt`] made for the signature state and
//! [`crate::domain::lifecycle::dependencies`] made for what a run put on a key,
//! and it is made here for the same reason: a stored summary is a second truth
//! about the register, and the moment it disagrees with the records nobody can
//! tell which one is wrong.
//!
//! # One shape for every report
//!
//! Every report is a [`Report`]: a scope line, a column header and rows of
//! strings. That is deliberately flat, and it is what makes
//! [`export`] one writer per format instead of one writer per report — a CSV
//! exporter that knew about custody rows and inventory rows separately would
//! eventually quote a comma in one and not in the other.
//!
//! It also fixes the thing an export is for. A report on screen is read by the
//! operator who generated it; a report in a file is read by somebody else, six
//! months later, who has to know what they are holding. So the scope, the
//! generation time and the operator travel *inside* the file, not in its name.
//!
//! # These files leave the application's protection
//!
//! Three of the seven carry personal data — a custody report is a list of people
//! and the credentials they hold. Once written to disk the file is protected by
//! the file system and nothing else: no database password, no audit trail of who
//! read it. [`ReportKind::carries_personal_data`] is what the export dialog uses
//! to say so before the file is written, and every export is audited
//! (`export.taken`) because the norm treats export of critical data as an
//! operation somebody must be able to review afterwards.

pub mod bundle;
pub mod export;

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};

use crate::audit::{AuditEntry, AuditFilter};
use crate::domain::lifecycle::{Remediation, sanitisation};
use crate::domain::{
    BootstrapRun, CustodyModel, DistributionRecord, Holder, KeyStatus, RunStatus, StepKind,
    StepStatus, YubiKeyRecord,
};
use crate::receipt::{Filed, SignaturePolicy, state_of};

/// Which question a report answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReportKind {
    /// What do we own?
    InventorySummary,
    /// Who holds a key right now, and for how long?
    Custody,
    /// What is neither in stock nor accounted for in somebody's hands?
    Unaccounted,
    /// Which distributed keys have no run, a failed run, or a superseded
    /// template version?
    BootstrapCompliance,
    /// Which signing certificates expire soon?
    CertificateExpiry,
    /// Where did the secrets each run set go?
    CustodyModel,
    /// The trail for a range, with a verification statement.
    AuditExtract,
}

impl ReportKind {
    /// Every report, in the order the screen offers them: what we own, who has
    /// it, what is missing, then the three that answer a specific question.
    pub const ALL: [ReportKind; 7] = [
        ReportKind::InventorySummary,
        ReportKind::Custody,
        ReportKind::Unaccounted,
        ReportKind::BootstrapCompliance,
        ReportKind::CertificateExpiry,
        ReportKind::CustodyModel,
        ReportKind::AuditExtract,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            ReportKind::InventorySummary => "Inventory summary",
            ReportKind::Custody => "Custody",
            ReportKind::Unaccounted => "Unaccounted",
            ReportKind::BootstrapCompliance => "Bootstrap compliance",
            ReportKind::CertificateExpiry => "Certificate expiry",
            ReportKind::CustodyModel => "Custody model",
            ReportKind::AuditExtract => "Audit extract",
        }
    }

    /// Stable name for the audit detail and the suggested file name. Snake-case
    /// like every other identifier the trail carries.
    pub fn slug(&self) -> &'static str {
        match self {
            ReportKind::InventorySummary => "inventory-summary",
            ReportKind::Custody => "custody",
            ReportKind::Unaccounted => "unaccounted",
            ReportKind::BootstrapCompliance => "bootstrap-compliance",
            ReportKind::CertificateExpiry => "certificate-expiry",
            ReportKind::CustodyModel => "custody-model",
            ReportKind::AuditExtract => "audit-extract",
        }
    }

    /// The question this report answers, in one line, for the screen.
    pub fn question(&self) -> &'static str {
        match self {
            ReportKind::InventorySummary => {
                "How many keys, of which models, in which states, from which batches?"
            }
            ReportKind::Custody => "Who holds a key right now, and for how long?",
            ReportKind::Unaccounted => {
                "Which keys are neither in stock nor accounted for in somebody's hands?"
            }
            ReportKind::BootstrapCompliance => {
                "Which distributed keys have no bootstrap run, a failed one, or a template \
                 version we have since corrected?"
            }
            ReportKind::CertificateExpiry => {
                "Which signing certificates expire soon, and who holds them?"
            }
            ReportKind::CustodyModel => {
                "Where did the secrets each run set go — to the holder, or to an escrow?"
            }
            ReportKind::AuditExtract => {
                "What does the trail say for this range, and does it still verify?"
            }
        }
    }

    /// Does the exported file contain personal data?
    ///
    /// Three of them do, and the operator is told before the file is written.
    /// The inventory summary counts keys rather than naming people, the custody
    /// **model** report is about where secrets went, and the audit extract's
    /// actors are operators rather than holders — but a custody report is a list
    /// of named people and the credentials they hold, which is exactly the
    /// artefact the DPO cares about.
    pub fn carries_personal_data(&self) -> bool {
        matches!(
            self,
            ReportKind::Custody | ReportKind::BootstrapCompliance | ReportKind::CertificateExpiry
        )
    }
}

/// What the operator is told before a file with personal data is written.
///
/// Short and specific on purpose: the useful sentence is not "this is
/// confidential" but "nothing in this application protects it once it is
/// written".
pub const PERSONAL_DATA_WARNING: &str = "This report names people and the credentials they hold. Once written, the file is outside \
     this application's protection — no database password, and no record of who opens it. Choose \
     where it goes deliberately.";

/// A generated report: what it is, when it was made, and the rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub kind: ReportKind,
    pub generated_at: DateTime<Utc>,
    pub generated_by: String,
    /// What was counted — "every key in the register", "8 open hand-overs".
    pub scope: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// Statements that belong to the report rather than to a row: the audit
    /// extract's verification result, or the reason a report is empty.
    pub notes: Vec<String>,
}

impl Report {
    fn new(
        kind: ReportKind,
        by: &str,
        now: DateTime<Utc>,
        columns: &[&str],
        rows: Vec<Vec<String>>,
    ) -> Self {
        Self {
            kind,
            generated_at: now,
            generated_by: by.trim().to_owned(),
            scope: String::new(),
            columns: columns.iter().map(|c| (*c).to_owned()).collect(),
            rows,
            notes: Vec::new(),
        }
    }

    fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = scope.into();
        self
    }

    fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The one line that identifies this file wherever it is found.
    pub fn provenance(&self) -> String {
        format!(
            "{} — generated {} by {} — {}",
            self.kind.label(),
            self.generated_at.format("%Y-%m-%d %H:%M:%SZ"),
            if self.generated_by.is_empty() {
                "(operator not recorded)"
            } else {
                &self.generated_by
            },
            self.scope,
        )
    }

    /// Suggested file name: `custody-2026-08-14.csv`.
    pub fn file_name(&self, format: export::Format) -> String {
        format!(
            "{}-{}.{}",
            self.kind.slug(),
            self.generated_at.format("%Y-%m-%d"),
            format.extension()
        )
    }

    /// The `export.taken` detail. Names what left and where it went, and cannot
    /// carry a secret because a report is built out of records that hold none.
    pub fn audit_detail(&self, format: export::Format, path: &std::path::Path) -> String {
        format!(
            "report={} format={} rows={} path={}",
            self.kind.slug(),
            format.slug(),
            self.rows.len(),
            path.display(),
        )
    }
}

/// Everything the reports are derived from.
///
/// Borrowed rather than owned: the caller already has these lists in memory for
/// the screens, and a report that copied the register to summarise it would be
/// the second truth this module exists to avoid.
pub struct Dataset<'a> {
    pub keys: &'a [YubiKeyRecord],
    pub holders: &'a [Holder],
    pub distributions: &'a [DistributionRecord],
    pub runs: &'a [BootstrapRun],
    /// Every recorded remediation, for the sanitisation state of a returned key.
    pub remediations: &'a [Remediation],
    /// The newest version of each template id, so a run against an older one can
    /// be reported as superseded.
    pub newest_template_version: BTreeMap<String, String>,
    /// What is filed against each hand-over, for the receipt state.
    pub filed: BTreeMap<uuid::Uuid, Filed>,
    pub policy: SignaturePolicy,
    pub now: DateTime<Utc>,
}

impl Dataset<'_> {
    fn holder(&self, id: uuid::Uuid) -> Option<&Holder> {
        self.holders.iter().find(|h| h.id == id)
    }

    fn filed_for(&self, record: &DistributionRecord) -> Filed {
        self.filed.get(&record.id).copied().unwrap_or_default()
    }

    /// Runs against one key, oldest first.
    fn runs_for(&self, serial: u32) -> Vec<&BootstrapRun> {
        self.runs
            .iter()
            .filter(|r| r.key_serial == serial)
            .collect()
    }

    /// The hand-over that is still open for this key, if any.
    fn open_distribution(&self, serial: u32) -> Option<&DistributionRecord> {
        self.distributions
            .iter()
            .filter(|d| d.key_serial == serial && d.is_open())
            // Newest wins: a key returned and handed out again has two records
            // for the same serial, and only the later one is live.
            .max_by_key(|d| d.distributed_at)
    }
}

/// Build one report.
pub fn build(kind: ReportKind, data: &Dataset<'_>, by: &str) -> Report {
    match kind {
        ReportKind::InventorySummary => inventory_summary(data, by),
        ReportKind::Custody => custody(data, by),
        ReportKind::Unaccounted => unaccounted(data, by),
        ReportKind::BootstrapCompliance => bootstrap_compliance(data, by),
        ReportKind::CertificateExpiry => certificate_expiry(data, by, DEFAULT_EXPIRY_WINDOW_DAYS),
        ReportKind::CustodyModel => custody_model(data, by),
        // The extract needs the trail, which the dataset does not carry: it is
        // built by `audit_extract` from what the store hands over.
        ReportKind::AuditExtract => Report::new(kind, by, data.now, AUDIT_COLUMNS, Vec::new())
            .with_note(
                "the audit extract is generated from the trail, not from the records — use \
                 `report::audit_extract`",
            ),
    }
}

// ---------------------------------------------------------------------------
// Inventory summary
// ---------------------------------------------------------------------------

/// Counts by status, model, firmware, batch, FIPS and provenance.
///
/// Every status is listed even at zero, and that is the useful behaviour rather
/// than the tidy one: "0 lost" is an answer, and a row that disappears when the
/// count reaches zero makes a reader work out whether the category is empty or
/// whether this build knows about it at all. The *other* groupings only list
/// what is present — there is no closed set of models to enumerate.
pub fn inventory_summary(data: &Dataset<'_>, by: &str) -> Report {
    let mut rows: Vec<Vec<String>> = Vec::new();

    rows.push(vec![
        "Total".into(),
        "keys on the register".into(),
        data.keys.len().to_string(),
    ]);

    for status in KeyStatus::ALL {
        let count = data.keys.iter().filter(|k| k.status == status).count();
        rows.push(vec![
            "Status".into(),
            status.label().to_owned(),
            count.to_string(),
        ]);
    }

    for (label, values) in [
        (
            "Model",
            tally(data.keys.iter().map(|k| label_or_unknown(&k.model))),
        ),
        (
            "Firmware",
            tally(data.keys.iter().map(|k| label_or_unknown(&k.firmware))),
        ),
        (
            "Batch",
            tally(data.keys.iter().map(|k| {
                let batch = k.batch.trim();
                if batch.is_empty() {
                    "(no batch recorded)".to_owned()
                } else {
                    batch.to_owned()
                }
            })),
        ),
    ] {
        for (value, count) in values {
            rows.push(vec![label.into(), value, count.to_string()]);
        }
    }

    let fips = data.keys.iter().filter(|k| k.fips).count();
    rows.push(vec!["FIPS".into(), "FIPS series".into(), fips.to_string()]);
    rows.push(vec![
        "FIPS".into(),
        "not FIPS".into(),
        (data.keys.len() - fips).to_string(),
    ]);

    for source in crate::domain::SerialSource::ALL {
        let count = data
            .keys
            .iter()
            .filter(|k| k.serial_source == source)
            .count();
        rows.push(vec![
            "Serial provenance".into(),
            source.label().to_owned(),
            count.to_string(),
        ]);
    }

    Report::new(
        ReportKind::InventorySummary,
        by,
        data.now,
        &["Grouping", "Value", "Keys"],
        rows,
    )
    .with_scope(format!("every key on the register ({})", data.keys.len()))
}

fn label_or_unknown(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        // A key recorded from a label or typed in has no model until somebody
        // plugs it in, and calling that "" in a report would read as a bug.
        "(not read from the key)".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Count occurrences, ordered by count descending then by value, so the biggest
/// group is at the top and ties are stable.
fn tally(values: impl Iterator<Item = String>) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for value in values {
        *counts.entry(value).or_default() += 1;
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

// ---------------------------------------------------------------------------
// Custody
// ---------------------------------------------------------------------------

/// Who holds a key right now.
///
/// **Open hand-overs only, and one row per open hand-over.** A key handed out,
/// returned, and handed out again has two records and is held once — counting
/// both would inflate the number this report exists to produce, and the
/// arithmetic is the whole point of it.
pub fn custody(data: &Dataset<'_>, by: &str) -> Report {
    let mut open: Vec<&DistributionRecord> =
        data.distributions.iter().filter(|d| d.is_open()).collect();
    open.sort_by_key(|d| d.distributed_at);

    let rows = open
        .iter()
        .map(|record| {
            let holder = data.holder(record.holder_id);
            let state = state_of(record, data.filed_for(record), &data.policy, data.now);
            vec![
                record.key_serial.to_string(),
                holder
                    .map(|h| h.full_name.clone())
                    .unwrap_or_else(|| record.holder_display.clone()),
                holder.map(|h| h.email.clone()).unwrap_or_default(),
                holder.map(|h| h.unit.clone()).unwrap_or_default(),
                record.distributed_at.format("%Y-%m-%d").to_string(),
                record.days_held(data.now).max(0).to_string(),
                record.method.label().to_owned(),
                state.label().to_owned(),
            ]
        })
        .collect();

    Report::new(
        ReportKind::Custody,
        by,
        data.now,
        &[
            "Serial",
            "Holder",
            "E-mail",
            "Unit",
            "Handed over",
            "Days held",
            "Method",
            "Term",
        ],
        rows,
    )
    .with_scope(format!("{} open hand-over(s)", open.len()))
}

// ---------------------------------------------------------------------------
// Unaccounted
// ---------------------------------------------------------------------------

/// The reconciliation report: everything the register cannot fully account for.
///
/// Four kinds of gap, and they are deliberately in one table rather than four:
/// the question an operator is asking is "what do I have to chase", and a
/// screen that answered it in four places would be four screens nobody opens.
///
/// What this report does **not** do is compare against what was purchased.
/// "Expected inventory" comes from procurement data this tool has never been
/// given, and inventing a source for it would produce a column that looks
/// authoritative and is not — see the gate in `features/reports-and-export.md`.
pub fn unaccounted(data: &Dataset<'_>, by: &str) -> Report {
    let mut rows: Vec<Vec<String>> = Vec::new();

    for key in data.keys {
        let open = data.open_distribution(key.serial);

        match key.status {
            // Filed as being with somebody, with nothing saying who.
            KeyStatus::Distributed if open.is_none() => rows.push(vec![
                key.serial.to_string(),
                key.status.label().to_owned(),
                "no open hand-over".into(),
                "the register says this key is with a holder and no hand-over record says who — \
                 record the hand-over, or correct the status"
                    .into(),
            ]),
            KeyStatus::Lost => {
                let outstanding = crate::domain::lifecycle::outstanding(
                    &crate::domain::lifecycle::dependencies(
                        &data
                            .runs_for(key.serial)
                            .into_iter()
                            .cloned()
                            .collect::<Vec<_>>(),
                    ),
                    &remediations_for(data, key.serial),
                )
                .len();
                rows.push(vec![
                    key.serial.to_string(),
                    key.status.label().to_owned(),
                    if outstanding == 0 {
                        "lost, nothing outstanding".into()
                    } else {
                        format!("lost, {outstanding} item(s) outstanding")
                    },
                    "reported lost or stolen — the certificate, the credentials and the access \
                     code are dealt with in somebody else's system, and the lifecycle panel \
                     lists what is still open"
                        .into(),
                ]);
            }
            // Back in the drawer still carrying the previous holder's
            // credentials: the reissue gate refuses it, and until somebody
            // resets it the key is stock that cannot be used.
            KeyStatus::Returned => {
                let state = sanitisation(
                    &data
                        .runs_for(key.serial)
                        .into_iter()
                        .cloned()
                        .collect::<Vec<_>>(),
                    &remediations_for(data, key.serial),
                );
                if !state.is_clear() {
                    rows.push(vec![
                        key.serial.to_string(),
                        key.status.label().to_owned(),
                        "returned, not sanitised".into(),
                        state.describe(),
                    ]);
                }
            }
            _ => {}
        }

        // The mirror of the first case: a hand-over that is open against a key
        // the register does not think is out. Caught here rather than at the
        // hand-over, because it is produced by a *later* status change.
        if let Some(record) = open
            && key.status != KeyStatus::Distributed
        {
            rows.push(vec![
                key.serial.to_string(),
                key.status.label().to_owned(),
                "open hand-over against a key that is not distributed".into(),
                format!(
                    "handed to {} on {} and never returned, while the register says {}",
                    record.holder_display,
                    record.distributed_at.format("%Y-%m-%d"),
                    key.status.label(),
                ),
            ]);
        }
    }

    // A hand-over pointing at a serial the inventory has never heard of. Only
    // reachable in a register whose key rows were removed or imported apart from
    // their history, which is precisely when a reconciliation report is read.
    let known: BTreeSet<u32> = data.keys.iter().map(|k| k.serial).collect();
    for record in data.distributions.iter().filter(|d| d.is_open()) {
        if !known.contains(&record.key_serial) {
            rows.push(vec![
                record.key_serial.to_string(),
                "(not in the inventory)".into(),
                "hand-over against an unknown serial".into(),
                format!(
                    "handed to {} on {} — no key with this serial is on the register",
                    record.holder_display,
                    record.distributed_at.format("%Y-%m-%d"),
                ),
            ]);
        }
    }

    rows.sort_by(|a, b| a[0].cmp(&b[0]).then_with(|| a[2].cmp(&b[2])));

    let report = Report::new(
        ReportKind::Unaccounted,
        by,
        data.now,
        &["Serial", "Status", "Gap", "What it means"],
        rows,
    )
    .with_scope(format!(
        "{} key(s) and {} hand-over(s) checked",
        data.keys.len(),
        data.distributions.len()
    ));

    if report.is_empty() {
        report.with_note("every key is accounted for: nothing to reconcile")
    } else {
        report.with_note(
            "expected-versus-present reconciliation is not included: this tool has no \
             procurement data to compare against",
        )
    }
}

fn remediations_for(data: &Dataset<'_>, serial: u32) -> Vec<Remediation> {
    data.remediations
        .iter()
        .filter(|r| r.key_serial == serial)
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Bootstrap compliance
// ---------------------------------------------------------------------------

/// Distributed keys whose bootstrap evidence is missing, failed or superseded.
///
/// The three cases are one report because they are one question — "is what is on
/// this key what we currently intend to put on a key" — and they have three
/// different answers, which is why each row says which case it is.
pub fn bootstrap_compliance(data: &Dataset<'_>, by: &str) -> Report {
    let mut rows: Vec<Vec<String>> = Vec::new();

    for record in data.distributions.iter().filter(|d| d.is_open()) {
        let runs = data.runs_for(record.key_serial);
        let holder = data
            .holder(record.holder_id)
            .map(|h| h.full_name.clone())
            .unwrap_or_else(|| record.holder_display.clone());

        let completed: Vec<&&BootstrapRun> = runs
            .iter()
            .filter(|r| r.status == RunStatus::Completed)
            .collect();

        if completed.is_empty() {
            let failed = runs
                .iter()
                .filter(|r| r.status == RunStatus::Failed)
                .count();
            rows.push(vec![
                record.key_serial.to_string(),
                holder,
                "—".into(),
                if failed > 0 {
                    "no completed run".into()
                } else {
                    "no bootstrap run".into()
                },
                if failed > 0 {
                    format!(
                        "{failed} run(s) against this key failed and none completed — the key was \
                         handed over without the procedure finishing"
                    )
                } else {
                    "this key was handed over with no bootstrap run on record".into()
                },
            ]);
            continue;
        }

        // The newest completed run is the one that describes the key as it was
        // handed over.
        let newest = completed
            .iter()
            .max_by_key(|r| r.finished_at.unwrap_or(r.started_at))
            .expect("completed is not empty");
        let applied = format!("{}@{}", newest.template_id, newest.template_version);

        match data.newest_template_version.get(&newest.template_id) {
            Some(current) if current != &newest.template_version => rows.push(vec![
                record.key_serial.to_string(),
                holder,
                applied,
                "superseded template".into(),
                format!(
                    "the procedure has since been corrected to version {current}; what is on this \
                     key is what version {} said to apply",
                    newest.template_version
                ),
            ]),
            None => rows.push(vec![
                record.key_serial.to_string(),
                holder,
                applied,
                "template not on the register".into(),
                format!(
                    "template `{}` is no longer in the catalogue, so what this run applied cannot \
                     be looked up",
                    newest.template_id
                ),
            ]),
            _ => {}
        }
    }

    rows.sort_by(|a, b| a[0].cmp(&b[0]));

    let report = Report::new(
        ReportKind::BootstrapCompliance,
        by,
        data.now,
        &["Serial", "Holder", "Applied", "Finding", "What it means"],
        rows,
    )
    .with_scope(format!(
        "{} open hand-over(s) checked",
        data.distributions.iter().filter(|d| d.is_open()).count()
    ));

    if report.is_empty() {
        report.with_note("every key in somebody's hands carries a completed, current procedure")
    } else {
        report
    }
}

// ---------------------------------------------------------------------------
// Certificate expiry
// ---------------------------------------------------------------------------

/// How far ahead the expiry report looks by default.
///
/// Sixty days is the renewal-planning horizon the feature file asks for: long
/// enough that a CSR, an issuance and a hand-over fit inside it, short enough
/// that the list is a work queue rather than the whole register.
pub const DEFAULT_EXPIRY_WINDOW_DAYS: i64 = 60;

/// Signing certificates by expiry, with the holder.
///
/// Read out of the run's step details, like every other piece of run evidence —
/// `features/step-piv-signing-certificate.md` puts the whole certificate summary
/// there, so a register written before this report existed answers it in full and
/// there is no column to backfill.
///
/// A certificate whose validity could not be parsed is **listed**, not dropped.
/// Silently omitting one would produce a report that says a key is fine because
/// the tool could not read the date on it.
pub fn certificate_expiry(data: &Dataset<'_>, by: &str, within_days: i64) -> Report {
    let horizon = data.now + Duration::days(within_days);
    let mut rows: Vec<Vec<String>> = Vec::new();

    for certificate in certificates(data) {
        let holder = certificate
            .holder_id
            .and_then(|id| data.holder(id))
            .map(|h| format!("{} <{}>", h.full_name, h.email))
            .unwrap_or_else(|| "(no holder on the run)".to_owned());

        let (expires, days, state) = match certificate.not_after {
            Some(not_after) => {
                let days = (not_after - data.now).num_days();
                let state = if days < 0 {
                    "EXPIRED".to_owned()
                } else if not_after <= horizon {
                    format!("expires within {within_days} days")
                } else {
                    "valid".to_owned()
                };
                (
                    not_after.format("%Y-%m-%d").to_string(),
                    days.to_string(),
                    state,
                )
            }
            None => (
                "(not recorded)".to_owned(),
                "—".to_owned(),
                "validity not readable from the run".to_owned(),
            ),
        };

        // Everything is kept in the table; the window decides the *order*, not
        // the membership, so the report is also the answer to "what is on the
        // fleet" without a second query.
        rows.push(vec![
            certificate.serial_key.to_string(),
            holder,
            certificate.certificate_serial,
            expires,
            days,
            state,
        ]);
    }

    // Soonest first, and the unreadable ones at the top rather than the bottom:
    // a certificate whose date nobody can read is the one worth looking at.
    rows.sort_by(|a, b| {
        let key = |row: &Vec<String>| match row[4].parse::<i64>() {
            Ok(days) => (1, days),
            Err(_) => (0, 0),
        };
        key(a).cmp(&key(b))
    });

    let due = rows.iter().filter(|row| row[5] != "valid").count();

    Report::new(
        ReportKind::CertificateExpiry,
        by,
        data.now,
        &[
            "Serial",
            "Holder",
            "Certificate",
            "Expires",
            "Days left",
            "State",
        ],
        rows,
    )
    .with_scope(format!(
        "certificates imported by a bootstrap run; {due} need attention within {within_days} days"
    ))
}

/// One certificate a run put on a key.
struct ImportedCertificate {
    serial_key: u32,
    holder_id: Option<uuid::Uuid>,
    certificate_serial: String,
    not_after: Option<DateTime<Utc>>,
}

fn certificates(data: &Dataset<'_>) -> Vec<ImportedCertificate> {
    let mut out = Vec::new();
    for run in data.runs {
        for step in run
            .steps
            .iter()
            .filter(|s| s.kind == StepKind::PivCertImport && s.status == StepStatus::Done)
        {
            out.push(ImportedCertificate {
                serial_key: run.key_serial,
                holder_id: run.holder_id,
                certificate_serial: field(&step.detail, "serial")
                    .unwrap_or_else(|| "(serial not recorded)".to_owned()),
                not_after: field(&step.detail, "valid").as_deref().and_then(not_after),
            });
        }
    }
    out
}

/// The `name=value` token a step detail carries.
///
/// The same reading [`crate::domain::lifecycle`] does, and deliberately the same
/// shape: the evidence a run leaves lives in its step details, and two different
/// parsers over one format is how they come to disagree.
fn field(detail: &str, name: &str) -> Option<String> {
    detail
        .split_whitespace()
        .find_map(|token| token.strip_prefix(&format!("{name}=")))
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
}

/// The second half of `valid=<not_before>..<not_after>`.
///
/// The dates are what `x509_cert` prints — `2027-01-01T00:00:00Z`, RFC 3339
/// without a fractional part. Parsed rather than compared as text, because
/// "expires in 12 days" is the column an operator acts on.
fn not_after(value: &str) -> Option<DateTime<Utc>> {
    let (_, end) = value.split_once("..")?;
    DateTime::parse_from_rfc3339(end.trim())
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

// ---------------------------------------------------------------------------
// Custody model
// ---------------------------------------------------------------------------

/// Where the secrets each run set went.
///
/// The report `features/secrets-custody.md` asks for: the decided model is B, so
/// every row should say *transport secret, holder changes it*, and a row that
/// says `escrowed` is a key whose secret is in somebody's store and has to be
/// destroyed when the key is. A row that says the model is not recorded is a run
/// from before the vocabulary existed, and says so rather than being guessed at.
pub fn custody_model(data: &Dataset<'_>, by: &str) -> Report {
    let mut rows: Vec<Vec<String>> = Vec::new();

    for run in data.runs.iter().filter(|r| r.status != RunStatus::Planned) {
        let secrets: Vec<&'static str> = run
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Done && s.kind.sets_secret())
            .map(|s| s.kind.label())
            .collect();
        if secrets.is_empty() {
            continue;
        }

        let model = CustodyModel::parse(&run.custody);
        let enforced = run
            .steps
            .iter()
            .any(|s| s.kind == StepKind::Fido2ForcePinChange && s.status == StepStatus::Done);

        rows.push(vec![
            run.key_serial.to_string(),
            data.holder(run.holder_id.unwrap_or_default())
                .map(|h| h.full_name.clone())
                .unwrap_or_else(|| "(stock — no holder)".to_owned()),
            format!("{}@{}", run.template_id, run.template_version),
            model
                .map(|m| m.label().to_owned())
                .unwrap_or_else(|| format!("(not recorded: {})", run.custody.trim())),
            if run.custody.trim().is_empty() {
                "—".to_owned()
            } else {
                run.custody.trim().to_owned()
            },
            if enforced {
                "enforced by firmware".to_owned()
            } else {
                "instructed on the term".to_owned()
            },
            secrets.join(", "),
        ]);
    }

    rows.sort_by(|a, b| a[0].cmp(&b[0]));

    let escrowed = rows
        .iter()
        .filter(|row| row[4].starts_with(CustodyModel::Escrowed.as_str()))
        .count();

    Report::new(
        ReportKind::CustodyModel,
        by,
        data.now,
        &[
            "Serial",
            "Holder",
            "Procedure",
            "Model",
            "Custody note",
            "Change",
            "Secrets set",
        ],
        rows,
    )
    .with_scope(format!(
        "every run that set a secret; {escrowed} with an external escrow"
    ))
}

// ---------------------------------------------------------------------------
// Audit extract
// ---------------------------------------------------------------------------

const AUDIT_COLUMNS: &[&str] = &["Seq", "When", "Actor", "Event", "Target", "Detail", "Hash"];

/// What verifying the chain said at the moment the extract was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    /// The whole chain verified, this many entries.
    Verified { entries: usize },
    /// It did not, and this is what broke.
    Broken { reason: String },
}

impl Verification {
    /// The statement that goes in the file.
    ///
    /// This is what makes an extract evidence rather than a table somebody could
    /// have edited afterwards: it names the range, the chain head, and whether
    /// the chain the range was cut from still verified when it was cut.
    pub fn statement(&self, first: Option<u64>, last: Option<u64>, head: &str) -> String {
        let range = match (first, last) {
            (Some(first), Some(last)) => format!("entries {first}-{last}"),
            _ => "no entries in range".to_owned(),
        };
        match self {
            Verification::Verified { entries } => format!(
                "{range}, cut from a chain of {entries} entries that verified at export time; \
                 chain head {head}"
            ),
            Verification::Broken { reason } => format!(
                "{range}; chain head {head}. THE CHAIN DID NOT VERIFY AT EXPORT TIME: {reason}. \
                 This extract is a copy of what the register holds, and what it holds is not \
                 intact — do not treat it as evidence without investigating."
            ),
        }
    }

    pub fn is_intact(&self) -> bool {
        matches!(self, Verification::Verified { .. })
    }
}

/// A `YYYY-MM-DD` bound typed into the extract's range, or `None`.
///
/// `None` for anything that is not a whole date — which is what an operator has
/// typed for most of the time the field is in focus. A half-typed date must
/// narrow *nothing*: a bound that guessed at `2026-01` would quietly exclude
/// most of the trail while the screen still said the range was empty.
///
/// `end_of_day` matters because [`AuditFilter::until`] is inclusive against a
/// timestamp: "until 2026-12-31" has to mean the end of that day, or the last
/// day of a requested range is silently missing from the extract.
pub fn parse_day(value: &str, end_of_day: bool) -> Option<DateTime<Utc>> {
    let date = chrono::NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").ok()?;
    let time = if end_of_day {
        chrono::NaiveTime::from_hms_opt(23, 59, 59)?
    } else {
        chrono::NaiveTime::MIN
    };
    Some(date.and_time(time).and_utc())
}

/// The trail for a range, with the statement that makes it self-describing.
///
/// `entries` is the trail as the store read it (oldest first) and `filter` is
/// what the operator narrowed it to. Both are needed: the *statement* is about
/// the whole chain, and the *rows* are about the range — an extract that
/// verified only the rows it printed would pass while the entries around them
/// had been rewritten.
pub fn audit_extract(
    entries: &[AuditEntry],
    filter: &AuditFilter,
    verification: &Verification,
    by: &str,
    now: DateTime<Utc>,
) -> Report {
    let selected = filter.apply(entries);
    let rows = selected
        .iter()
        .map(|entry| {
            vec![
                entry.seq.to_string(),
                entry.at.to_rfc3339(),
                entry.actor.clone(),
                entry.event.clone(),
                entry.target.clone(),
                entry.details.clone(),
                entry.hash.clone(),
            ]
        })
        .collect();

    let head = entries
        .last()
        .map(|entry| entry.hash.clone())
        .unwrap_or_else(|| crate::audit::GENESIS.to_owned());

    Report::new(ReportKind::AuditExtract, by, now, AUDIT_COLUMNS, rows)
        .with_scope(filter.describe(selected.len(), entries.len()))
        .with_note(verification.statement(
            selected.first().map(|e| e.seq),
            selected.last().map(|e| e.seq),
            &head,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DeliveryMethod, SerialSource, StepOutcome};
    use uuid::Uuid;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-14T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn key(serial: u32, status: KeyStatus) -> YubiKeyRecord {
        let mut record = YubiKeyRecord::from_serial(serial, SerialSource::Device);
        record.model = "YubiKey 5 NFC".into();
        record.firmware = "5.7.4".into();
        record.batch = "NF 2026/1".into();
        record.status = status;
        record
    }

    fn holder(name: &str) -> Holder {
        Holder::new(
            name,
            &format!("{}@example.org", name.to_lowercase()),
            "TI",
            "",
        )
        .expect("valid holder")
    }

    fn handover(serial: u32, holder: &Holder, days_ago: i64) -> DistributionRecord {
        DistributionRecord {
            id: Uuid::new_v4(),
            key_id: Uuid::new_v4(),
            key_serial: serial,
            holder_id: holder.id,
            holder_display: holder.display(),
            distributed_at: now() - Duration::days(days_ago),
            distributed_by: "felipe".into(),
            method: DeliveryMethod::InPerson,
            receipt_ref: String::new(),
            bootstrap_run_id: None,
            returned_at: None,
            returned_to: None,
            notes: String::new(),
        }
    }

    fn run(serial: u32, holder: Option<&Holder>, version: &str, status: RunStatus) -> BootstrapRun {
        let mut run = BootstrapRun::new(
            serial,
            holder.map(|h| h.id),
            "org-standard",
            version,
            "felipe",
            Vec::new(),
        );
        run.status = status;
        run.finished_at = Some(now() - Duration::days(1));
        run.custody = CustodyModel::TransportPinForcedChange.as_str().to_owned();
        run
    }

    /// A valid chain over `(actor, event, target, details)`, hashed the way the
    /// store hashes it — so the extract's statement is checked against a chain
    /// that would actually verify.
    fn chain(entries: &[(&str, &str, &str, &str)]) -> Vec<AuditEntry> {
        let mut out: Vec<AuditEntry> = Vec::new();
        for (index, (actor, event, target, details)) in entries.iter().enumerate() {
            let mut entry = AuditEntry {
                seq: index as u64 + 1,
                at: now() + Duration::seconds(index as i64),
                actor: (*actor).to_owned(),
                event: (*event).to_owned(),
                target: (*target).to_owned(),
                details: (*details).to_owned(),
                prev_hash: out
                    .last()
                    .map(|e| e.hash.clone())
                    .unwrap_or_else(|| crate::audit::GENESIS.to_owned()),
                hash: String::new(),
            };
            entry.hash = entry.compute_hash();
            out.push(entry);
        }
        out
    }

    fn done(kind: StepKind, detail: &str) -> StepOutcome {
        let mut step = StepOutcome::planned(kind.slug(), kind, detail);
        step.status = StepStatus::Done;
        step.finished_at = Some(now() - Duration::days(1));
        step
    }

    struct Fixture {
        keys: Vec<YubiKeyRecord>,
        holders: Vec<Holder>,
        distributions: Vec<DistributionRecord>,
        runs: Vec<BootstrapRun>,
        remediations: Vec<Remediation>,
        newest: BTreeMap<String, String>,
    }

    impl Fixture {
        fn empty() -> Self {
            Self {
                keys: Vec::new(),
                holders: Vec::new(),
                distributions: Vec::new(),
                runs: Vec::new(),
                remediations: Vec::new(),
                newest: BTreeMap::new(),
            }
        }

        fn data(&self) -> Dataset<'_> {
            Dataset {
                keys: &self.keys,
                holders: &self.holders,
                distributions: &self.distributions,
                runs: &self.runs,
                remediations: &self.remediations,
                newest_template_version: self.newest.clone(),
                filed: BTreeMap::new(),
                policy: SignaturePolicy::default(),
                now: now(),
            }
        }
    }

    #[test]
    fn the_inventory_summary_counts_every_status_including_the_empty_ones() {
        let mut fixture = Fixture::empty();
        fixture.keys = vec![
            key(1, KeyStatus::InStock),
            key(2, KeyStatus::InStock),
            key(3, KeyStatus::Distributed),
        ];
        let data = fixture.data();
        let report = inventory_summary(&data, "felipe");

        let row = |value: &str| {
            report
                .rows
                .iter()
                .find(|r| r[1] == value)
                .unwrap_or_else(|| panic!("a row for {value}"))
                .clone()
        };
        assert_eq!(row("keys on the register")[2], "3");
        assert_eq!(row("In stock")[2], "2");
        assert_eq!(row("Distributed")[2], "1");
        // The point of the rule: zero is an answer, not a missing row.
        assert_eq!(row("Lost / stolen")[2], "0");
        assert_eq!(row("YubiKey 5 NFC")[2], "3");
        assert!(
            report.provenance().contains("felipe"),
            "{}",
            report.provenance()
        );
    }

    #[test]
    fn custody_counts_a_key_handed_out_twice_once() {
        // The awkward case the feature file names: returned and reissued must not
        // read as two people holding the same key.
        let ana = holder("Ana");
        let bruno = holder("Bruno");
        let mut fixture = Fixture::empty();

        let mut first = handover(20_423_633, &ana, 200);
        first.returned_at = Some(now() - Duration::days(100));
        let second = handover(20_423_633, &bruno, 30);

        fixture.holders = vec![ana, bruno];
        fixture.distributions = vec![first, second];

        let data = fixture.data();
        let report = custody(&data, "felipe");
        assert_eq!(report.rows.len(), 1, "{:?}", report.rows);
        assert_eq!(report.rows[0][1], "Bruno");
        assert_eq!(report.rows[0][5], "30");
        assert!(report.scope.contains("1 open"), "{}", report.scope);
    }

    #[test]
    fn a_key_filed_as_distributed_with_no_handover_is_unaccounted_for() {
        let mut fixture = Fixture::empty();
        fixture.keys = vec![key(20_423_633, KeyStatus::Distributed)];

        let data = fixture.data();
        let report = unaccounted(&data, "felipe");
        assert_eq!(report.rows.len(), 1, "{:?}", report.rows);
        assert_eq!(report.rows[0][2], "no open hand-over");
        // And the report says what it deliberately cannot answer.
        assert!(
            report.notes.iter().any(|n| n.contains("procurement")),
            "{:?}",
            report.notes
        );
    }

    #[test]
    fn an_open_handover_against_a_key_the_register_says_is_in_stock_is_a_gap() {
        let ana = holder("Ana");
        let mut fixture = Fixture::empty();
        fixture.keys = vec![key(20_423_633, KeyStatus::InStock)];
        fixture.distributions = vec![handover(20_423_633, &ana, 10)];
        fixture.holders = vec![ana];

        let data = fixture.data();
        let report = unaccounted(&data, "felipe");
        assert_eq!(report.rows.len(), 1, "{:?}", report.rows);
        assert!(
            report.rows[0][2].contains("not distributed"),
            "{:?}",
            report.rows[0]
        );
    }

    #[test]
    fn a_register_with_nothing_outstanding_says_so_rather_than_showing_an_empty_table() {
        let ana = holder("Ana");
        let mut fixture = Fixture::empty();
        fixture.keys = vec![key(20_423_633, KeyStatus::Distributed)];
        fixture.distributions = vec![handover(20_423_633, &ana, 10)];
        fixture.holders = vec![ana];

        let data = fixture.data();
        let report = unaccounted(&data, "felipe");
        assert!(report.is_empty(), "{:?}", report.rows);
        assert!(
            report.notes.iter().any(|n| n.contains("accounted for")),
            "{:?}",
            report.notes
        );
    }

    #[test]
    fn compliance_finds_a_handover_with_no_run_and_one_on_a_superseded_version() {
        let ana = holder("Ana");
        let bruno = holder("Bruno");
        let mut fixture = Fixture::empty();
        fixture.distributions = vec![handover(1, &ana, 10), handover(2, &bruno, 10)];
        fixture.runs = vec![run(2, Some(&bruno), "1", RunStatus::Completed)];
        fixture.newest.insert("org-standard".into(), "2".into());
        fixture.holders = vec![ana, bruno];

        let data = fixture.data();
        let report = bootstrap_compliance(&data, "felipe");
        assert_eq!(report.rows.len(), 2, "{:?}", report.rows);

        let no_run = &report.rows[0];
        assert_eq!(no_run[0], "1");
        assert_eq!(no_run[3], "no bootstrap run");

        let superseded = &report.rows[1];
        assert_eq!(superseded[0], "2");
        assert_eq!(superseded[3], "superseded template");
        assert!(superseded[4].contains("version 2"), "{superseded:?}");
    }

    #[test]
    fn a_failed_run_is_not_evidence_that_the_key_was_prepared() {
        let ana = holder("Ana");
        let mut fixture = Fixture::empty();
        fixture.distributions = vec![handover(1, &ana, 10)];
        fixture.runs = vec![run(1, Some(&ana), "2", RunStatus::Failed)];
        fixture.newest.insert("org-standard".into(), "2".into());
        fixture.holders = vec![ana];

        let data = fixture.data();
        let report = bootstrap_compliance(&data, "felipe");
        assert_eq!(report.rows.len(), 1, "{:?}", report.rows);
        assert_eq!(report.rows[0][3], "no completed run");
    }

    #[test]
    fn the_expiry_report_reads_the_date_out_of_the_run_and_counts_the_days() {
        let ana = holder("Ana");
        let mut run = run(20_423_633, Some(&ana), "2", RunStatus::Completed);
        run.steps = vec![done(
            StepKind::PivCertImport,
            "[native] certificate imported into slot 9c — subject=CN=Ana issuer=CN=CA \
             serial=0A1B2C valid=2026-01-01T00:00:00Z..2026-09-01T00:00:00Z",
        )];

        let mut fixture = Fixture::empty();
        fixture.runs = vec![run];
        fixture.holders = vec![ana];

        let data = fixture.data();
        let report = certificate_expiry(&data, "felipe", 60);
        assert_eq!(report.rows.len(), 1, "{:?}", report.rows);
        assert_eq!(report.rows[0][2], "0A1B2C");
        assert_eq!(report.rows[0][3], "2026-09-01");
        // Whole days remaining, rounded down: 17 days and 12 hours is 17, and an
        // operator planning a renewal is better served by the pessimistic figure.
        assert_eq!(report.rows[0][4], "17");
        assert_eq!(report.rows[0][5], "expires within 60 days");
        assert!(
            report.rows[0][1].contains("ana@example.org"),
            "{:?}",
            report.rows[0]
        );
    }

    #[test]
    fn a_certificate_whose_date_cannot_be_read_is_listed_rather_than_dropped() {
        // The failure this guards: a report that says everything is fine because
        // the tool could not read the one date that mattered.
        let mut run = run(1, None, "2", RunStatus::Completed);
        run.steps = vec![done(
            StepKind::PivCertImport,
            "certificate imported serial=AA",
        )];

        let mut fixture = Fixture::empty();
        fixture.runs = vec![run];

        let data = fixture.data();
        let report = certificate_expiry(&data, "felipe", 60);
        assert_eq!(report.rows.len(), 1, "{:?}", report.rows);
        assert_eq!(report.rows[0][3], "(not recorded)");
        assert!(
            report.rows[0][5].contains("not readable"),
            "{:?}",
            report.rows[0]
        );
    }

    #[test]
    fn an_expired_certificate_sorts_above_a_valid_one() {
        let mut expired = run(1, None, "2", RunStatus::Completed);
        expired.steps = vec![done(
            StepKind::PivCertImport,
            "serial=OLD valid=2020-01-01T00:00:00Z..2021-01-01T00:00:00Z",
        )];
        let mut fine = run(2, None, "2", RunStatus::Completed);
        fine.steps = vec![done(
            StepKind::PivCertImport,
            "serial=NEW valid=2026-01-01T00:00:00Z..2030-01-01T00:00:00Z",
        )];

        let mut fixture = Fixture::empty();
        fixture.runs = vec![fine, expired];

        let data = fixture.data();
        let report = certificate_expiry(&data, "felipe", 60);
        assert_eq!(report.rows[0][2], "OLD");
        assert_eq!(report.rows[0][5], "EXPIRED");
        assert_eq!(report.rows[1][5], "valid");
    }

    #[test]
    fn the_custody_model_report_only_lists_runs_that_set_a_secret() {
        let ana = holder("Ana");
        let mut with_secret = run(1, Some(&ana), "2", RunStatus::Completed);
        with_secret.steps = vec![
            done(StepKind::Fido2Pin, "pin set"),
            done(StepKind::Fido2ForcePinChange, "marked"),
        ];
        let mut without = run(2, Some(&ana), "2", RunStatus::Completed);
        without.steps = vec![done(StepKind::Verify, "read back")];

        let mut fixture = Fixture::empty();
        fixture.runs = vec![with_secret, without];
        fixture.holders = vec![ana];

        let data = fixture.data();
        let report = custody_model(&data, "felipe");
        assert_eq!(report.rows.len(), 1, "{:?}", report.rows);
        assert_eq!(report.rows[0][0], "1");
        assert_eq!(report.rows[0][5], "enforced by firmware");
        assert!(
            report.rows[0][6].contains("FIDO2 PIN"),
            "{:?}",
            report.rows[0]
        );
    }

    #[test]
    fn the_audit_extract_states_the_range_the_head_and_the_verification() {
        let entries = chain(&[
            ("felipe", "key.added", "serial:1", ""),
            ("felipe", "key.distributed", "serial:1", "holder=Ana"),
            ("ana", "template.saved", "template:org-standard", ""),
        ]);

        let filter = AuditFilter {
            actor: "felipe".into(),
            ..AuditFilter::default()
        };
        let report = audit_extract(
            &entries,
            &filter,
            &Verification::Verified { entries: 3 },
            "felipe",
            now(),
        );

        assert_eq!(report.rows.len(), 2);
        let statement = report.notes.first().expect("a statement");
        assert!(statement.contains("entries 1-2"), "{statement}");
        assert!(statement.contains(&entries[2].hash), "{statement}");
        assert!(statement.contains("verified"), "{statement}");
    }

    #[test]
    fn an_extract_from_a_broken_chain_says_so_in_the_file() {
        let entries = chain(&[("felipe", "key.added", "serial:1", "")]);
        let report = audit_extract(
            &entries,
            &AuditFilter::default(),
            &Verification::Broken {
                reason: "audit chain broken at entry 1".into(),
            },
            "felipe",
            now(),
        );
        let statement = report.notes.first().expect("a statement");
        assert!(statement.contains("DID NOT VERIFY"), "{statement}");
        assert!(
            !Verification::Broken {
                reason: String::new()
            }
            .is_intact()
        );
    }

    #[test]
    fn a_half_typed_date_narrows_nothing_and_until_means_the_end_of_that_day() {
        assert_eq!(parse_day("", false), None);
        assert_eq!(parse_day("2026-01", false), None, "still being typed");
        assert_eq!(parse_day("not a date", false), None);

        let from = parse_day("2026-01-01", false).expect("a whole date");
        assert_eq!(from.to_rfc3339(), "2026-01-01T00:00:00+00:00");

        // The off-by-one that would silently drop the last day of every requested
        // range: `until` is compared inclusively against a timestamp.
        let until = parse_day(" 2026-12-31 ", true).expect("a whole date");
        assert_eq!(until.to_rfc3339(), "2026-12-31T23:59:59+00:00");

        let entry = chain(&[("felipe", "key.added", "serial:1", "")]).remove(0);
        let filter = AuditFilter {
            from: parse_day("2026-08-14", false),
            until: parse_day("2026-08-14", true),
            ..AuditFilter::default()
        };
        assert!(filter.matches(&entry), "the day it happened is in range");
    }

    #[test]
    fn every_report_has_its_own_name_slug_and_question() {
        let slugs: BTreeSet<&str> = ReportKind::ALL.iter().map(|k| k.slug()).collect();
        assert_eq!(slugs.len(), ReportKind::ALL.len());
        let labels: BTreeSet<&str> = ReportKind::ALL.iter().map(|k| k.label()).collect();
        assert_eq!(labels.len(), ReportKind::ALL.len());
        for kind in ReportKind::ALL {
            assert!(!kind.question().trim().is_empty(), "{kind:?}");
        }
        // The three that name people are the three that warn.
        assert!(ReportKind::Custody.carries_personal_data());
        assert!(!ReportKind::InventorySummary.carries_personal_data());
    }

    #[test]
    fn build_dispatches_every_kind_that_reads_the_records() {
        let fixture = Fixture::empty();
        let data = fixture.data();
        for kind in ReportKind::ALL {
            let report = build(kind, &data, "felipe");
            assert_eq!(report.kind, kind);
            assert!(!report.columns.is_empty(), "{kind:?}");
        }
    }
}
