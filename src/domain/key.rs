//! The physical YubiKey: identity read off the device plus lifecycle state.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::device::DeviceInfo;

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
            created_at: now,
            updated_at: now,
        }
    }

    /// Refresh the device-derived fields without touching lifecycle or notes.
    pub fn refresh_from_device(&mut self, info: &DeviceInfo) {
        self.model = info.model.clone();
        self.firmware = info.firmware.clone();
        self.form_factor = info.form_factor.clone();
        self.applications = info.usb_applications.clone();
        self.updated_at = Utc::now();
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
}
