//! Native, pure-Rust access to the hardware — the **preferred** transport.
//!
//! Rather than shelling out to `ykman`, the app talks to the key directly:
//!
//! | Applet | Crate | Transport | State |
//! |---|---|---|---|
//! | PIV | [`yubikey`] | PC/SC (CCID) | implemented here (identification); write ops in Wave 1 |
//! | FIDO2 / CTAP2 | `ctap-hid-fido2` | USB HID | `features/step-fido2-pin.md`, `features/step-fido2-credentials.md` |
//! | Yubico OTP | `hidapi` | USB HID feature reports | `features/step-otp-access-code.md` |
//! | Management (form factor, capabilities) | — | CCID `00 1D` | no crate covers it; `features/ykman-abstraction.md` |
//!
//! Compiled only with the `native-piv` feature, because `pcsc` links against a
//! system library.

use yubikey::reader::Context;

use super::{DeviceError, DeviceInfo, Result, YubiKeyBackend};

/// Reads identity straight off the PIV applet over PC/SC.
///
/// No `ykman`, no subprocess, no PATH dependency. The trade-off is coverage:
/// the PIV applet reports serial and firmware version but not the form factor
/// or the per-application enable flags, which live in the management applet.
#[derive(Debug, Default)]
pub struct NativeBackend {
    _private: (),
}

impl NativeBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn context() -> Result<Context> {
        Context::open().map_err(|e| DeviceError::Command {
            command: "pcsc: establish context".into(),
            message: e.to_string(),
        })
    }
}

impl YubiKeyBackend for NativeBackend {
    fn list_serials(&self) -> Result<Vec<u32>> {
        let mut ctx = Self::context()?;
        let readers = ctx.iter().map_err(|e| DeviceError::Command {
            command: "pcsc: list readers".into(),
            message: e.to_string(),
        })?;

        let mut serials = Vec::new();
        for reader in readers {
            let name = reader.name().to_string();
            // A reader that fails to open is not fatal: another process may hold
            // an exclusive transaction on it. Record and move on.
            match reader.open() {
                Ok(key) => serials.push(u32::from(key.serial())),
                Err(e) => {
                    tracing::warn!(
                        event = "device.reader.unavailable",
                        reader = name.as_str(),
                        reason = %e
                    );
                }
            }
        }
        Ok(serials)
    }

    fn info(&self, serial: Option<u32>) -> Result<DeviceInfo> {
        let mut ctx = Self::context()?;
        let readers = ctx.iter().map_err(|e| DeviceError::Command {
            command: "pcsc: list readers".into(),
            message: e.to_string(),
        })?;

        let mut found: Vec<DeviceInfo> = Vec::new();
        for reader in readers {
            let name = reader.name().to_string();
            let Ok(key) = reader.open() else { continue };
            let device_serial = u32::from(key.serial());
            if serial.is_some_and(|wanted| wanted != device_serial) {
                continue;
            }
            let version = key.version();
            found.push(DeviceInfo {
                serial: device_serial,
                // The PIV applet does not carry a marketing name; the reader
                // name is the closest identification available natively.
                model: name,
                firmware: format!("{}.{}.{}", version.major, version.minor, version.patch),
                form_factor: String::new(),
                nfc: false,
                usb_applications: vec!["PIV".to_owned()],
            });
        }

        match (found.len(), serial) {
            (0, _) => Err(DeviceError::NoDevice),
            (1, _) => Ok(found.remove(0)),
            (_, Some(_)) => Ok(found.remove(0)),
            (n, None) => Err(DeviceError::Ambiguous(n)),
        }
    }

    fn describe(&self) -> String {
        "native (yubikey crate over PC/SC)".into()
    }
}
