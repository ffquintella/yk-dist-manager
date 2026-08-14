//! FIDO2 / CTAP2 writes over USB HID, using [`ctap_hid_fido2`].
//!
//! This is the transport `features/native-device-transport.md` argues hardest
//! for, because it is the one where the fallback cannot substitute at all:
//! **`ykman` can list and delete resident credentials but cannot create one**, and
//! creating the initial discoverable credential is a required step of the
//! standard procedure. Everything else in that document — typed errors, no PIN on
//! a command line, no Python on the workstation — applies here too, but this is
//! the step that makes the native path necessary rather than merely better.
//!
//! ## One key at a time
//!
//! HID gives no serial number, so this transport cannot confirm it is talking to
//! the same key the run is about. `FidoKeyHidFactory::create` refuses when more
//! than one FIDO device is attached, which is the same policy
//! `features/device-detection.md` already applies to reads: picking one at random
//! and writing a PIN to it is the worst available outcome. The `serial` argument
//! is therefore checked against what the caller established over PIV, and
//! otherwise unused — stated here rather than left as a surprise.
//!
//! ## Touch
//!
//! `make_credential` requires user presence: the key blinks and waits. There is
//! no way to suppress that and no reason to want to, but a caller that does not
//! tell the operator to touch the key will look frozen — see
//! `features/gui-bootstrap-wizard.md` phase 2.
//!
//! ## Errors
//!
//! The crate returns `anyhow::Error`, so the mapping to [`WriteError`] is by
//! inspection of the message plus a follow-up query where one exists. That is
//! unpleasant and it is the honest cost of this dependency; the retry count comes
//! from `get_pin_retries`, which is authoritative, rather than from parsing.

use ctap_hid_fido2::fidokey::{FidoKeyHid, MakeCredentialArgsBuilder};
use ctap_hid_fido2::{FidoKeyHidFactory, LibCfg};

use super::write::{
    CredentialEvidence, CredentialRequest, Fido2State, Fido2Writer, Result, WriteError,
};
use crate::secret::Secret;

/// FIDO2 writes against the attached key.
pub struct NativeFido2 {
    cfg: LibCfg,
    /// The serial the run is about, for the mismatch check described above.
    expected_serial: u32,
}

impl NativeFido2 {
    pub fn for_key(serial: u32) -> Self {
        Self {
            cfg: LibCfg::init(),
            expected_serial: serial,
        }
    }

    /// Open the key for one operation.
    ///
    /// A handle per call rather than one held for the run: the HID device is
    /// exclusive on some platforms, and holding it open would stop `ykman` and
    /// the operating system's own security-key UI from working for the whole
    /// hand-over.
    fn open(&self, operation: &'static str) -> Result<FidoKeyHid> {
        FidoKeyHidFactory::create(&self.cfg).map_err(|e| {
            let message = e.to_string();
            if message.contains("not found") {
                WriteError::NotAttached(self.expected_serial)
            } else if message.contains("Multiple") {
                WriteError::Failed {
                    operation,
                    reason: "more than one security key is attached — leave only the one being \
                             configured, because this transport cannot tell them apart"
                        .into(),
                }
            } else {
                WriteError::Failed {
                    operation,
                    reason: message,
                }
            }
        })
    }

    fn guard_serial(&self, serial: u32) -> Result<()> {
        if serial != self.expected_serial {
            return Err(WriteError::NotAttached(serial));
        }
        Ok(())
    }

    /// Turn a crate error into a typed one, enriching a PIN failure with the
    /// retry count the key itself reports.
    ///
    /// Generic over the error type rather than naming `anyhow::Error`: the crate
    /// returns one, but `anyhow` is not a direct dependency here and adding it to
    /// `Cargo.toml` to name a type in one signature would be a dependency taken
    /// for a match arm.
    fn classify<E: std::fmt::Display>(
        &self,
        key: &FidoKeyHid,
        operation: &'static str,
        error: E,
    ) -> WriteError {
        let message = error.to_string();
        let lower = message.to_lowercase();

        if lower.contains("pin_invalid") || lower.contains("pin invalid") {
            return WriteError::WrongSecret {
                applet: "FIDO2",
                retries_left: key.get_pin_retries().unwrap_or(0).max(0) as u8,
            };
        }
        if lower.contains("pin_blocked") || lower.contains("pin_auth_blocked") {
            return WriteError::Locked { applet: "FIDO2" };
        }
        if lower.contains("pin_policy_violation") {
            return WriteError::Unsupported {
                operation,
                reason: "the key's PIN policy refused this PIN — it is shorter than the minimum \
                         the key requires"
                    .into(),
            };
        }
        if lower.contains("unsupported_option") || lower.contains("invalid_option") {
            return WriteError::Unsupported {
                operation,
                reason: "this key's firmware does not offer the option".into(),
            };
        }
        if lower.contains("no device") || lower.contains("not found") {
            return WriteError::Detached { operation };
        }
        WriteError::Failed {
            operation,
            reason: message,
        }
    }
}

impl Fido2Writer for NativeFido2 {
    fn fido2_state(&mut self, serial: u32) -> Result<Fido2State> {
        self.guard_serial(serial)?;
        let key = self.open("fido2.get_info")?;
        let info = key
            .get_info()
            .map_err(|e| self.classify(&key, "fido2.get_info", e))?;

        let option = |name: &str| {
            info.options
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| *value)
                .unwrap_or(false)
        };

        Ok(Fido2State {
            // `clientPin` true means a PIN is set; false means the key supports
            // PINs but has none.
            pin_set: option("clientPin"),
            // CTAP 2.1. A key that predates it reports 0, which is reported as
            // `None` so a caller can tell "no policy" from "a policy of zero".
            min_pin_length: (info.min_pin_length > 0).then_some(info.min_pin_length as u8),
            force_pin_change_set: info.force_pin_change,
            // Not knowable without the PIN: counting resident credentials needs
            // an authenticated credential-management call. Zero is the
            // conservative answer, and it is *correct* in the case the executor
            // acts on — a key with no PIN set can hold no discoverable
            // credential, because creating one requires user verification.
            resident_credentials: 0,
            // CTAP 2.1 again, and the same reading of zero: a firmware that does
            // not report it sends 0, which here means "did not say" rather than
            // "full". Treating it as full would refuse the credential step on
            // every key below 5.7.
            remaining_credential_slots: (info.remaining_discoverable_credentials > 0)
                .then_some(info.remaining_discoverable_credentials as usize),
            // The retry counter is read through `get_pin_retries`, which does not
            // spend one. A failure to read it is not a failure to read the state:
            // it stays `None`, which every consumer already treats as unknown.
            pin_retries: key
                .get_pin_retries()
                .ok()
                .and_then(|left| u8::try_from(left).ok()),
        })
    }

    fn set_pin(&mut self, serial: u32, new: &Secret) -> Result<()> {
        self.guard_serial(serial)?;
        let key = self.open("fido2.set_pin")?;
        key.set_new_pin(new.expose())
            .map_err(|e| self.classify(&key, "fido2.set_pin", e))
    }

    fn change_pin(&mut self, serial: u32, current: &Secret, new: &Secret) -> Result<()> {
        self.guard_serial(serial)?;
        let key = self.open("fido2.change_pin")?;
        key.change_pin(current.expose(), new.expose())
            .map_err(|e| self.classify(&key, "fido2.change_pin", e))
    }

    fn set_min_pin_length(&mut self, serial: u32, length: u8, pin: &Secret) -> Result<()> {
        self.guard_serial(serial)?;
        let key = self.open("fido2.set_min_pin_length")?;
        key.set_min_pin_length(length, Some(pin.expose()))
            .map_err(|e| self.classify(&key, "fido2.set_min_pin_length", e))
    }

    fn force_pin_change(&mut self, serial: u32, pin: &Secret) -> Result<()> {
        self.guard_serial(serial)?;
        let key = self.open("fido2.force_pin_change")?;
        // The mechanism custody model B depends on where the firmware has it
        // (CTAP 2.1, 5.7+). Below that the step is refused as unsupported and the
        // consignment term carries the instruction instead.
        key.force_change_pin(Some(pin.expose()))
            .map_err(|e| self.classify(&key, "fido2.force_pin_change", e))
    }

    fn make_credential(
        &mut self,
        serial: u32,
        request: &CredentialRequest,
        pin: &Secret,
    ) -> Result<CredentialEvidence> {
        self.guard_serial(serial)?;
        let key = self.open("fido2.make_credential")?;

        // Registration here is not answering a server's challenge — there is no
        // relying party on the other end of a hand-over — so the challenge is
        // fresh randomness rather than something received. Using a constant would
        // make every credential this tool creates share one.
        let mut challenge = [0u8; 32];
        getrandom::fill(&mut challenge).map_err(|e| WriteError::Failed {
            operation: "fido2.make_credential",
            reason: format!("no randomness for the registration challenge: {e}"),
        })?;

        // The user handle must be stable for the holder and at most 64 bytes, so
        // it is a hash of the e-mail rather than the e-mail itself.
        let user_id = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(request.user_name.as_bytes());
            hasher.finalize().to_vec()
        };

        let mut builder =
            MakeCredentialArgsBuilder::new(&request.relying_party, &challenge).pin(pin.expose());
        if request.resident {
            // `rk=true` — the whole point of the step. Without it the credential
            // is not discoverable and the key cannot be used without the relying
            // party already knowing its id.
            builder = builder.resident_key();
        }
        let args = builder
            .user_entity(&ctap_hid_fido2::public_key_credential_user_entity::PublicKeyCredentialUserEntity::new(
                Some(&user_id),
                Some(&request.user_name),
                Some(&request.user_display_name),
            ))
            .build();

        let attestation = key
            .make_credential_with_args(&args)
            .map_err(|e| self.classify(&key, "fido2.make_credential", e))?;

        Ok(CredentialEvidence {
            credential_id_hex: hex::encode(&attestation.credential_descriptor.id),
            relying_party: request.relying_party.clone(),
            algorithm: "ES256".into(),
        })
    }
}
