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

/// Read which OTP slots are programmed, through `ykman otp info`.
///
/// **A labelled fallback, and the only read there is.** The native path would be the
/// OTP HID status frame, which no crate in this graph exposes — writing it means
/// hand-rolling the protocol, and `features/native-device-transport.md` phase 4 is
/// explicit that the read path must be verified against this command on a real key
/// before any of it is trusted. Until that happens, this *is* the read: it is
/// read-only, it is the source `ykman info` itself uses, and having it means the
/// pre-flight can describe the OTP applet instead of reporting it as unknown.
pub fn otp_state(backend: &YkmanBackend, serial: u32) -> Result<crate::device::write::OtpState> {
    let serial = serial.to_string();
    let out = backend.run(&["--device", &serial, "otp", "info"])?;
    parse_otp_info(&out)
}

/// Parse `ykman otp info`.
///
/// The output is two lines naming each slot and whether it holds a configuration:
///
/// ```text
/// Slot 1: programmed
/// Slot 2: empty
/// ```
///
/// Parsed by *matching the words rather than the position*, because a line that has
/// gained a field between `ykman` versions must not silently turn a programmed slot
/// into an empty one — a wrong answer here says a slot is free to overwrite when it
/// is not.
pub fn parse_otp_info(stdout: &str) -> Result<crate::device::write::OtpState> {
    let mut state = crate::device::write::OtpState::default();
    let mut seen = 0;

    for line in stdout.lines() {
        let line = line.trim();
        let Some((slot, rest)) = line.split_once(':') else {
            continue;
        };
        let slot = slot.trim().to_ascii_lowercase();
        let programmed = match rest.trim().to_ascii_lowercase() {
            // Both words `ykman` uses. Anything else is not treated as empty:
            // "unknown" must not read as "free".
            state_word if state_word.starts_with("programmed") => true,
            state_word if state_word.starts_with("empty") => false,
            _ => continue,
        };
        match slot.as_str() {
            "slot 1" => {
                state.slot_one_programmed = programmed;
                seen += 1;
            }
            "slot 2" => {
                state.slot_two_programmed = programmed;
                seen += 1;
            }
            _ => {}
        }
    }

    if seen == 0 {
        return Err(DeviceError::Parse {
            command: "ykman otp info".into(),
            reason: "no `Slot N: programmed|empty` line — the applet may be disabled over USB"
                .into(),
        });
    }
    Ok(state)
}

/// Return the FIDO2 applet to factory default, through `ykman fido reset`.
///
/// **A labelled fallback, and the only implementation there is.** No crate in
/// this dependency graph sends CTAP's `authenticatorReset`: `ctap-hid-fido2`
/// exposes `get_info`, the PIN commands, credential management and
/// `make_credential`, and nothing that resets. Writing the command by hand would
/// mean hand-rolling CTAP framing for the one operation whose failure mode is
/// "every credential on this key is gone" — so this shells out, and
/// `device::reset` shows the operator which transport ran.
///
/// The authenticator refuses a reset that does not arrive within a few seconds of
/// power-up and that is not confirmed by touch. That is CTAP, not `ykman`, and it
/// is why [`crate::device::reset::Applet::instruction`] exists.
pub fn reset_fido2(backend: &YkmanBackend, serial: u32) -> Result<String> {
    let serial = serial.to_string();
    backend.run(&["--device", &serial, "fido", "reset", "--force"])
}

/// Return the PIV applet to factory default, through `ykman piv reset`.
///
/// The fallback for a session that does not read through the native transport —
/// `device::native_piv::NativePiv::reset_applet` is the in-process path, and
/// `device::reset::route` decides between them.
pub fn reset_piv(backend: &YkmanBackend, serial: u32) -> Result<String> {
    let serial = serial.to_string();
    backend.run(&["--device", &serial, "piv", "reset", "--force"])
}

/// Clear one OTP slot, through `ykman otp delete`.
///
/// There is no "reset the OTP applet": the applet *is* its two slots, so
/// returning it to factory default means deleting each programmed one. A slot
/// protected by an access code refuses, and the refusal is the transport's own
/// message — this tool records custody of an access code and never the value, so
/// there is nothing here to supply.
pub fn delete_otp_slot(backend: &YkmanBackend, serial: u32, slot: u8) -> Result<String> {
    let serial = serial.to_string();
    let slot = slot.to_string();
    backend.run(&["--device", &serial, "otp", "delete", &slot, "--force"])
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
