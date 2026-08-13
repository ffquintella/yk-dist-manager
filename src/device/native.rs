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
            found.push(identified(
                device_serial,
                name,
                format!("{}.{}.{}", version.major, version.minor, version.patch),
            ));
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

/// Everything the PIV applet can answer for, and nothing beyond it.
///
/// **`usb_applications` is left empty on purpose, and that is not the same as "only
/// PIV".** The per-application enable flags live in the *management* applet (CCID
/// `00 1D`), which no crate covers and this transport does not read. It used to report
/// `["PIV"]` — the applet it had just spoken to — and that one word was a claim that
/// FIDO2 and OTP were *disabled*: the pre-flight then skipped every FIDO2 and OTP step
/// of the procedure on a key that had them all enabled, which is most of a bootstrap
/// silently not happening.
///
/// Empty means "not read", which is what [`crate::domain::YubiKeyRecord::from_serial`]
/// already means by it, what the Inventory screen renders as `—`, and what the
/// pre-flight treats as no reason to skip a step.
fn identified(serial: u32, reader: String, firmware: String) -> DeviceInfo {
    DeviceInfo {
        serial,
        // The PIV applet does not carry a marketing name; the reader name is the
        // closest identification available natively.
        model: reader,
        firmware,
        form_factor: String::new(),
        nfc: false,
        usb_applications: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_native_read_never_claims_an_application_is_disabled() {
        // The management applet is what carries the enable flags, and this transport
        // does not read it. Naming the one applet it did speak to would be read
        // downstream as "the others are off" — and the pre-flight would skip them.
        let info = identified(20_423_633, "YubiKey CCID".to_owned(), "5.7.4".to_owned());
        assert_eq!(info.serial, 20_423_633);
        assert_eq!(info.firmware, "5.7.4");
        assert!(
            info.usb_applications.is_empty(),
            "an unread field stays empty rather than guessed: {:?}",
            info.usb_applications
        );
    }
}
