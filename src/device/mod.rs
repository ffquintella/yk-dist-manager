//! Discovery and inspection of physical YubiKeys.
//!
//! Everything the app knows about a device goes through [`YubiKeyBackend`], so
//! the GUI and the bootstrap engine can be exercised in tests against
//! [`mock::MockBackend`] with no hardware attached.
//!
//! Two real transports exist, in this order of preference:
//!
//! 1. [`native`] — pure-Rust access to the applets (`yubikey` over PC/SC for
//!    PIV, `ctap-hid-fido2` for FIDO2, `hidapi` for OTP). No external process,
//!    typed errors, no output parsing. Behind the `native-*` features.
//! 2. [`ykman`] — subprocess fallback, for the operations no crate covers yet
//!    (management-applet metadata such as form factor and per-application
//!    enable flags) and as a diagnostic cross-check.
//!
//! See `docs/yubikey-reference.md` for the capability matrix and
//! `features/native-device-transport.md` for the migration plan.

pub mod mock;
#[cfg(feature = "native-piv")]
pub mod native;
#[cfg(feature = "native-fido")]
pub mod native_fido;
pub mod write;
pub mod ykman;

pub use mock::MockBackend;
#[cfg(feature = "native-piv")]
pub use native::NativeBackend;
pub use ykman::YkmanBackend;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DeviceInfo {
    pub serial: u32,
    pub model: String,
    pub firmware: String,
    pub form_factor: String,
    pub nfc: bool,
    /// Applications enabled over USB (`Yubico OTP`, `FIDO2`, `PIV`, …).
    pub usb_applications: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("`{binary}` was not found — install yubikey-manager (ykman)")]
    ToolMissing { binary: String },
    #[error("no YubiKey detected")]
    NoDevice,
    #[error("more than one YubiKey is connected ({0} found) — leave only one attached")]
    Ambiguous(usize),
    #[error("`{command}` failed: {message}")]
    Command { command: String, message: String },
    #[error("could not parse `{command}` output: {reason}")]
    Parse { command: String, reason: String },
}

pub type Result<T> = std::result::Result<T, DeviceError>;

/// Read-only view of the attached hardware.
///
/// Mutating operations (setting a PIN, generating a key) deliberately live in
/// the bootstrap engine, not here: this trait stays safe to call at any time,
/// including from the GUI's polling loop.
pub trait YubiKeyBackend: Send {
    /// Serial numbers of every attached key.
    fn list_serials(&self) -> Result<Vec<u32>>;

    /// Full identity of one key; `None` selects the only attached key.
    fn info(&self, serial: Option<u32>) -> Result<DeviceInfo>;

    /// Short description of the backend, for the settings screen.
    fn describe(&self) -> String;
}
