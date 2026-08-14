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

    /// Run `ykman` with a secret written to the child's **standard input**.
    ///
    /// The whole reason this exists rather than an extra argument: an argument
    /// vector is readable by every process on the workstation (`ps`, `/proc`), so a
    /// six-byte access code on a command line is a secret in a place it can be
    /// read from — which `AGENTS.md` §2 forbids. `ykman` offers `-` on the options
    /// that take a code, meaning "prompt for it", and a prompt reads stdin.
    ///
    /// `lines` is written verbatim and **never logged**: `rendered` below is built
    /// from the arguments alone, which by construction hold no secret. The caller
    /// supplies as many lines as the prompt asks for — some of `ykman`'s prompts
    /// confirm, and want the value twice.
    fn run_with_stdin(&self, args: &[&str], lines: &[&str]) -> Result<String> {
        use std::io::Write as _;
        use std::process::Stdio;

        let rendered = format!("{} {}", self.binary.display(), args.join(" "));
        let mut child = Command::new(&self.binary)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
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

        {
            // Dropped at the end of this block, which closes the pipe — a prompt
            // waiting on more input would otherwise hang the run in front of an
            // operator with no way to see why.
            let mut stdin = child.stdin.take().ok_or_else(|| DeviceError::Command {
                command: rendered.clone(),
                message: "the subprocess offered no standard input to write the code to".into(),
            })?;
            for line in lines {
                stdin
                    .write_all(line.as_bytes())
                    .and_then(|()| stdin.write_all(b"\n"))
                    .map_err(|e| DeviceError::Command {
                        command: rendered.clone(),
                        // `e` is an I/O error about a pipe and carries no value.
                        message: format!("the code could not be handed to the subprocess: {e}"),
                    })?;
            }
        }

        let output = child.wait_with_output().map_err(|e| DeviceError::Command {
            command: rendered.clone(),
            message: e.to_string(),
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

/// An OTP slot access code is six bytes, written as twelve hex characters — which
/// is both what the applet stores and what `ykman` parses.
const OTP_ACCESS_CODE_HEX: usize = 12;

/// Write-protect an OTP slot with an access code, through
/// `ykman otp settings --new-access-code -` (`features/step-otp-access-code.md`
/// phase 2).
///
/// **A labelled fallback, and the only implementation there is.** The native path
/// is the OTP HID configuration frame, which no crate in this graph exposes; phase
/// 4 keeps it deliberately unwritten until there is a key to verify it against,
/// because the failure mode of a wrong frame is a slot write-protected by a code
/// nobody holds. The plan already shows this step as `ykman (fallback)`, so what
/// the operator agreed to and what runs are the same thing.
///
/// Three facts about the command, each read out of `ykman` 5.9.2's own source
/// rather than guessed, because each one changes whether this works at all:
///
/// * `--new-access-code -` prompts **with confirmation**, so the code is written to
///   stdin twice. One line would leave the prompt waiting.
/// * `settings` refuses an **empty** slot — "Not possible to update settings on an
///   empty slot". An access code protects a configuration; there is nothing to
///   protect on a slot that holds none. The step checks that first, so an operator
///   gets a sentence rather than a subprocess error.
/// * `--force` skips the interactive "all existing settings will be overwritten"
///   confirmation. That overwrite is real and is why the step's preview says so:
///   this command rewrites the slot's other settings to their defaults.
///
/// **Not hardware-verified.** The invocation is derived from `ykman`'s source; no
/// key was attached when it was written.
pub fn set_otp_access_code(
    backend: &YkmanBackend,
    serial: u32,
    slot: u8,
    code: &crate::secret::Secret,
) -> crate::device::write::Result<()> {
    use crate::device::write::WriteError;
    const OP: &str = "otp.set_access_code";

    if !matches!(slot, 1 | 2) {
        return Err(WriteError::Unsupported {
            operation: OP,
            reason: format!("a YubiKey has OTP slots 1 and 2, and this template asked for {slot}"),
        });
    }
    // Checked here rather than left to `ykman`: its refusal names a parse failure,
    // and the length is the one thing about this value that can be checked without
    // ever looking at it.
    if code.len() != OTP_ACCESS_CODE_HEX {
        return Err(WriteError::Failed {
            operation: OP,
            reason: format!(
                "an OTP access code is exactly {OTP_ACCESS_CODE_HEX} hex characters (six bytes) \
                 and this one is {} — nothing was sent to the key",
                code.len()
            ),
        });
    }

    let serial = serial.to_string();
    let slot = slot.to_string();
    backend
        .run_with_stdin(
            &[
                "--device",
                &serial,
                "otp",
                "settings",
                &slot,
                "--force",
                "--new-access-code",
                "-",
            ],
            // Twice: the prompt confirms.
            &[code.expose(), code.expose()],
        )
        .map(|_| ())
        .map_err(|e| translate(OP, e))
}

/// Turn a subprocess failure into the typed error the executor branches on.
///
/// The distinctions that matter are the ones the executor acts on differently: a
/// key that is not there stops the run, an unsupported operation is a skip, and
/// everything else is a failure of this step. `ykman`'s own message is kept
/// because it is what an operator will search for.
fn translate(operation: &'static str, error: DeviceError) -> crate::device::write::WriteError {
    use crate::device::write::WriteError;
    match error {
        DeviceError::NoDevice => WriteError::Detached { operation },
        // `feature` names a build feature everywhere else, and no feature installs
        // `ykman` — so the honest answer names the tool instead. The same variant,
        // because the caller's handling is identical: this workstation cannot
        // perform the operation.
        DeviceError::ToolMissing { .. } => WriteError::TransportUnavailable {
            operation,
            feature: "ykman on PATH",
        },
        other => WriteError::Failed {
            operation,
            reason: other.to_string(),
        },
    }
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
