//! In-memory backend used by tests and by the GUI's demo mode.

use std::sync::Mutex;

use super::{DeviceError, DeviceInfo, Result, YubiKeyBackend};

/// A backend serving a fixed set of devices.
pub struct MockBackend {
    devices: Mutex<Vec<DeviceInfo>>,
    /// When set, every call fails with this message — used to exercise the
    /// GUI's error paths.
    fail_with: Option<String>,
}

impl MockBackend {
    pub fn new(devices: Vec<DeviceInfo>) -> Self {
        Self {
            devices: Mutex::new(devices),
            fail_with: None,
        }
    }

    /// One device matching the real YubiKey 5 NFC used to record the fixtures.
    pub fn single_5nfc() -> Self {
        Self::new(vec![DeviceInfo {
            serial: 20_423_633,
            model: "YubiKey 5 NFC".into(),
            firmware: "5.4.3".into(),
            form_factor: "Keychain (USB-A)".into(),
            nfc: true,
            usb_applications: vec![
                "Yubico OTP".into(),
                "FIDO U2F".into(),
                "FIDO2".into(),
                "OATH".into(),
                "PIV".into(),
                "OpenPGP".into(),
                "YubiHSM Auth".into(),
            ],
        }])
    }

    pub fn failing(message: impl Into<String>) -> Self {
        Self {
            devices: Mutex::new(Vec::new()),
            fail_with: Some(message.into()),
        }
    }

    /// Simulate plugging a key in or out.
    pub fn set_devices(&self, devices: Vec<DeviceInfo>) {
        *self.devices.lock().expect("mock lock") = devices;
    }

    fn guard(&self) -> Result<()> {
        match &self.fail_with {
            Some(message) => Err(DeviceError::Command {
                command: "mock".into(),
                message: message.clone(),
            }),
            None => Ok(()),
        }
    }
}

impl YubiKeyBackend for MockBackend {
    fn list_serials(&self) -> Result<Vec<u32>> {
        self.guard()?;
        Ok(self
            .devices
            .lock()
            .expect("mock lock")
            .iter()
            .map(|d| d.serial)
            .collect())
    }

    fn info(&self, serial: Option<u32>) -> Result<DeviceInfo> {
        self.guard()?;
        let devices = self.devices.lock().expect("mock lock");
        match serial {
            Some(wanted) => devices
                .iter()
                .find(|d| d.serial == wanted)
                .cloned()
                .ok_or(DeviceError::NoDevice),
            None => match devices.len() {
                0 => Err(DeviceError::NoDevice),
                1 => Ok(devices[0].clone()),
                n => Err(DeviceError::Ambiguous(n)),
            },
        }
    }

    fn describe(&self) -> String {
        "mock backend (no hardware)".into()
    }
}
