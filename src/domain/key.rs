//! The physical YubiKey: identity read off the device plus lifecycle state.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::device::DeviceInfo;

/// How this tool learned the serial number.
///
/// A serial read from the device is *verified*; one read from a box label or typed
/// by hand is a *claim* about a key nobody has plugged in. The distinction
/// matters for the certificate: a mis-scanned digit would bind a credential to the
/// wrong physical key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SerialSource {
    /// Read from the hardware over PC/SC or `ykman`.
    Device,
    /// Decoded from a barcode on the packaging.
    ScannedLabel,
    /// Typed by an operator.
    ManualEntry,
}

impl SerialSource {
    pub const ALL: [SerialSource; 3] = [
        SerialSource::Device,
        SerialSource::ScannedLabel,
        SerialSource::ManualEntry,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            SerialSource::Device => "read from the key",
            SerialSource::ScannedLabel => "scanned from the label",
            SerialSource::ManualEntry => "typed",
        }
    }

    /// Only a device read confirms that this key exists and is reachable.
    pub fn is_verified(&self) -> bool {
        matches!(self, SerialSource::Device)
    }

    /// Stable snake-case name, for the database column and for audit details.
    ///
    /// One spelling for both, so an audit entry reads the way the column does.
    pub fn audit_name(&self) -> &'static str {
        match self {
            SerialSource::Device => "device",
            SerialSource::ScannedLabel => "scanned-label",
            SerialSource::ManualEntry => "manual-entry",
        }
    }
}

/// Where a key is in its lifecycle. See `features/key-lifecycle-and-revocation.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyStatus {
    /// Received from the supplier, nothing applied yet.
    InStock,
    /// Bootstrap template applied, not yet handed over.
    Bootstrapped,
    /// In the hands of a holder.
    Distributed,
    /// Handed back, pending reset or re-issue.
    Returned,
    /// Reported lost or stolen; credentials must be revoked.
    Lost,
    /// Permanently out of service (destroyed / RMA'd).
    Retired,
}

impl KeyStatus {
    pub fn label(&self) -> &'static str {
        match self {
            KeyStatus::InStock => "In stock",
            KeyStatus::Bootstrapped => "Bootstrapped",
            KeyStatus::Distributed => "Distributed",
            KeyStatus::Returned => "Returned",
            KeyStatus::Lost => "Lost / stolen",
            KeyStatus::Retired => "Retired",
        }
    }

    /// Stable snake-case name, for the database column and for audit details.
    pub fn audit_name(&self) -> &'static str {
        match self {
            KeyStatus::InStock => "in_stock",
            KeyStatus::Bootstrapped => "bootstrapped",
            KeyStatus::Distributed => "distributed",
            KeyStatus::Returned => "returned",
            KeyStatus::Lost => "lost",
            KeyStatus::Retired => "retired",
        }
    }

    /// Allowed transitions. Anything not listed here is rejected by the store
    /// and reported to the operator instead of silently applied.
    pub fn can_transition_to(&self, next: KeyStatus) -> bool {
        use KeyStatus::*;
        match (self, next) {
            (InStock, Bootstrapped | Retired | Lost) => true,
            (Bootstrapped, Distributed | InStock | Retired | Lost) => true,
            (Distributed, Returned | Lost | Retired) => true,
            (Returned, InStock | Bootstrapped | Retired) => true,
            (Lost, Returned | Retired) => true,
            (Retired, _) => false,
            _ => false,
        }
    }
}

/// One physical key in the inventory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YubiKeyRecord {
    pub id: Uuid,
    /// Yubico serial number, as printed on the key and reported by `ykman list --serials`.
    pub serial: u32,
    /// e.g. `YubiKey 5 NFC`.
    pub model: String,
    /// e.g. `5.4.3`.
    pub firmware: String,
    /// e.g. `Keychain (USB-A)`.
    pub form_factor: String,
    /// True when the device reports a FIPS-validated series.
    pub fips: bool,
    /// Applications enabled over USB, as reported by `ykman info`.
    pub applications: Vec<String>,
    pub status: KeyStatus,
    /// Purchase batch / invoice reference, for asset reconciliation.
    pub batch: String,
    pub notes: String,
    /// How the serial was learned. Never downgraded by the store.
    pub serial_source: SerialSource,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl YubiKeyRecord {
    /// Build an inventory record from a freshly inspected device.
    pub fn from_device(info: &DeviceInfo) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            serial: info.serial,
            model: info.model.clone(),
            firmware: info.firmware.clone(),
            form_factor: info.form_factor.clone(),
            fips: info.model.to_ascii_uppercase().contains("FIPS"),
            applications: info.usb_applications.clone(),
            status: KeyStatus::InStock,
            batch: String::new(),
            notes: String::new(),
            serial_source: SerialSource::Device,
            created_at: now,
            updated_at: now,
        }
    }

    /// Record a key from a serial alone — a scanned label or a typed number.
    ///
    /// Everything the hardware would have told us (model, firmware, form factor,
    /// enabled applications) is unknown, and stays empty rather than guessed. The
    /// record is completed the first time the key is actually read.
    pub fn from_serial(serial: u32, source: SerialSource) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            serial,
            model: String::new(),
            firmware: String::new(),
            form_factor: String::new(),
            fips: false,
            applications: Vec::new(),
            status: KeyStatus::InStock,
            batch: String::new(),
            notes: String::new(),
            serial_source: source,
            created_at: now,
            updated_at: now,
        }
    }

    /// Refresh the device-derived fields without touching lifecycle or notes.
    ///
    /// Reading the key also *verifies* the serial, so the provenance is upgraded —
    /// which is how a scanned label becomes a confirmed record.
    pub fn refresh_from_device(&mut self, info: &DeviceInfo) {
        self.model = info.model.clone();
        self.firmware = info.firmware.clone();
        self.form_factor = info.form_factor.clone();
        self.applications = info.usb_applications.clone();
        self.fips = info.model.to_ascii_uppercase().contains("FIPS");
        self.serial_source = SerialSource::Device;
        self.updated_at = Utc::now();
    }

    /// True when the key has been read from the hardware at least once.
    pub fn is_verified(&self) -> bool {
        self.serial_source.is_verified()
    }

    /// Parsed firmware as `(major, minor, patch)`; used for the feature gates in
    /// `docs/yubikey-reference.md` (e.g. min-PIN-length needs 5.7+).
    pub fn firmware_triple(&self) -> Option<(u32, u32, u32)> {
        let mut it = self.firmware.split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next()?.parse().ok()?;
        let patch = it.next().unwrap_or("0").parse().unwrap_or(0);
        Some((major, minor, patch))
    }

    /// Whether the firmware supports the 5.7-era FIDO configuration commands.
    pub fn supports_fido_min_pin_length(&self) -> bool {
        matches!(self.firmware_triple(), Some((major, minor, _)) if (major, minor) >= (5, 7))
    }

    /// Audit detail for the removal of this record: what the register is losing.
    ///
    /// The observation is summarised by length rather than quoted. An audit entry
    /// cannot be edited or deleted, and an operator's free text is the one field
    /// here that may need correcting later — so the trail records that there *was*
    /// an observation, not what it said.
    pub fn removal_audit_detail(&self) -> String {
        format!(
            "status={} source={} model={} note_chars={}",
            self.status.audit_name(),
            self.serial_source.audit_name(),
            if self.model.is_empty() {
                "(unknown)"
            } else {
                &self.model
            },
            self.notes.chars().count()
        )
    }
}

/// One-line rendering of an observation for a table cell.
///
/// An observation can be [`crate::domain::MAX_NOTE`] characters, and a table cell
/// is one line — so it is cut to `max` *characters* (never bytes, so an accented
/// note cannot be split mid-character), with newlines folded to spaces and an
/// ellipsis marking that there is more. Empty reads as an em dash, like every
/// other absent cell on the screen.
pub fn summarise_note(note: &str, max: usize) -> String {
    let folded = note.split_whitespace().collect::<Vec<_>>().join(" ");
    if folded.is_empty() {
        return "—".to_owned();
    }
    match folded.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &folded[..cut].trim_end()),
        None => folded,
    }
}

/// Audit detail for a change to a key's observation.
///
/// Says which way the field moved and how long it now is, and quotes neither the
/// old text nor the new one — for the same reason
/// [`YubiKeyRecord::removal_audit_detail`] does not.
pub fn note_audit_detail(before: &str, after: &str) -> String {
    let (before, after) = (before.chars().count(), after.chars().count());
    let what = match (before, after) {
        (0, 0) => "unchanged",
        (0, _) => "set",
        (_, 0) => "cleared",
        _ => "changed",
    };
    format!("note={what} chars={after} was_chars={before}")
}
