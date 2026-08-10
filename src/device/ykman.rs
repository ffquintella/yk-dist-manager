//! `ykman`-backed implementation of [`YubiKeyBackend`].
//!
//! The parsers are separated from the process handling so they can be unit
//! tested against recorded output (`tests/fixtures/`). Output captured from
//! `ykman` 5.9.2 against a YubiKey 5 NFC, firmware 5.4.3.

use std::path::PathBuf;
use std::process::Command;

use super::{DeviceError, DeviceInfo, Result, YubiKeyBackend};

/// Default binary name; overridable in the settings screen for non-standard
/// installs (Homebrew on Apple silicon, portable Windows builds).
pub const DEFAULT_BINARY: &str = "ykman";

pub struct YkmanBackend {
    binary: PathBuf,
}

impl Default for YkmanBackend {
    fn default() -> Self {
        Self::new(DEFAULT_BINARY)
    }
}

impl YkmanBackend {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    pub fn binary(&self) -> &PathBuf {
        &self.binary
    }

    /// Run `ykman` with the given arguments and return stdout.
    ///
    /// No user-supplied string is ever interpolated into a shell — arguments are
    /// passed as an argv vector.
    fn run(&self, args: &[&str]) -> Result<String> {
        let rendered = format!("{} {}", self.binary.display(), args.join(" "));
        let output = Command::new(&self.binary)
            .args(args)
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    DeviceError::ToolMissing {
                        binary: self.binary.display().to_string(),
                    }
                } else {
                    DeviceError::Command {
                        command: rendered.clone(),
                        message: e.to_string(),
                    }
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(DeviceError::Command {
                command: rendered,
                message: if stderr.is_empty() {
                    format!("exit status {}", output.status)
                } else {
                    stderr
                },
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

impl YubiKeyBackend for YkmanBackend {
    fn list_serials(&self) -> Result<Vec<u32>> {
        let out = self.run(&["list", "--serials"])?;
        Ok(parse_serials(&out))
    }

    fn info(&self, serial: Option<u32>) -> Result<DeviceInfo> {
        let serial_arg = serial.map(|s| s.to_string());
        let mut args: Vec<&str> = Vec::new();
        if let Some(s) = serial_arg.as_deref() {
            args.extend_from_slice(&["--device", s]);
        }
        args.push("info");

        let out = self.run(&args)?;
        parse_info(&out)
    }

    fn describe(&self) -> String {
        format!("ykman ({})", self.binary.display())
    }
}

/// Parse `ykman list --serials`: one decimal serial per line.
pub fn parse_serials(stdout: &str) -> Vec<u32> {
    stdout
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .collect()
}

/// Parse `ykman info`.
///
/// Expected shape (real capture):
///
/// ```text
/// Device type: YubiKey 5 NFC
/// Serial number: 20423633
/// Firmware version: 5.4.3
/// Form factor: Keychain (USB-A)
/// Enabled USB interfaces: OTP, FIDO, CCID
/// NFC transport is enabled
///
/// Applications  USB      NFC
/// Yubico OTP   Enabled  Enabled
/// FIDO2        Enabled  Enabled
/// ```
///
/// (the real output separates the columns with tab characters)
pub fn parse_info(stdout: &str) -> Result<DeviceInfo> {
    let mut info = DeviceInfo::default();
    let mut in_app_table = false;

    for line in stdout.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() {
            continue;
        }

        if trimmed.trim_start().starts_with("Applications") {
            in_app_table = true;
            continue;
        }

        if in_app_table {
            if let Some((name, usb)) = parse_app_row(trimmed)
                && usb.eq_ignore_ascii_case("Enabled")
            {
                info.usb_applications.push(name);
            }
            continue;
        }

        if trimmed.contains("NFC transport is enabled") {
            info.nfc = true;
            continue;
        }

        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = value.trim().to_owned();
        match key.trim() {
            "Device type" => info.model = value,
            "Serial number" => {
                info.serial = value.parse().map_err(|_| DeviceError::Parse {
                    command: "ykman info".into(),
                    reason: format!("serial number `{value}` is not a number"),
                })?
            }
            "Firmware version" => info.firmware = value,
            "Form factor" => info.form_factor = value,
            _ => {}
        }
    }

    if info.serial == 0 {
        return Err(DeviceError::Parse {
            command: "ykman info".into(),
            reason: "no serial number in output".into(),
        });
    }

    Ok(info)
}

/// Split one row of the applications table into `(name, usb_state)`.
///
/// Columns are tab-separated, but the name column is also space-padded, so both
/// separators are handled.
fn parse_app_row(line: &str) -> Option<(String, String)> {
    let mut cols = line
        .split('\t')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .collect::<Vec<_>>();

    if cols.len() < 2 {
        // Fall back to run-of-two-spaces splitting for space-aligned output.
        cols = line
            .split("  ")
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .collect();
    }

    match cols.as_slice() {
        [name, usb, ..] => Some(((*name).to_owned(), (*usb).to_owned())),
        _ => None,
    }
}

/// Whether the firmware implements the CTAP 2.1 `authenticatorConfig` commands
/// (`setMinPINLength`, `forcePINChange`, `alwaysUv`) — YubiKey 5.7 and newer.
///
/// One gate, used by every step that depends on it, so a firmware fact is not
/// re-derived in three places. See `docs/yubikey-reference.md`.
pub fn supports_ctap21_config(firmware: &str) -> bool {
    let mut it = firmware.split('.');
    let major: u32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let minor: u32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    (major, minor) >= (5, 7)
}

/// Firmware gate for the minimum-PIN-length policy.
pub fn supports_min_pin_length(firmware: &str) -> bool {
    supports_ctap21_config(firmware)
}
