//! The operations that **change** a key, and the mock that lets them be tested
//! without one.
//!
//! Kept apart from [`crate::device::YubiKeyBackend`] on purpose. That trait is
//! read-only and safe to call at any time, including from a polling loop; these
//! traits are not safe to call at any time, and the type system should say so.
//! A screen that only holds a `&dyn YubiKeyBackend` cannot write to a key by
//! accident, which is `AGENTS.md`'s "nothing mutates hardware as a side effect of
//! opening a screen" expressed as a signature rather than a rule to remember.
//!
//! ## Three traits, one per applet
//!
//! FIDO2, PIV and OTP are separate applets reached over different transports
//! (CTAP over HID, PIV over PC/SC, OTP over HID config frames), gated by separate
//! Cargo features, and they fail in different ways. One combined trait would mean
//! every implementation stubbing two thirds of itself.
//!
//! ## Secrets are borrowed, never taken
//!
//! Every method takes `&Secret`. An implementation gets to read the value for the
//! duration of the call and cannot keep it: there is no `Clone`, so storing one
//! would mean moving it out of the caller's hands, which the signature forbids.
//! [`MockWriter`] records that a call *happened* and never what it carried —
//! asserted by a test, because a mock that logged its arguments would be the one
//! place in the codebase where secrets accumulate in memory.
//!
//! ## What is not here
//!
//! The live implementations. `features/bootstrap-engine.md` phases 5–7 land those
//! behind `native-fido`, `native-piv` and `native-otp`, and each needs hardware to
//! verify. The executor is written against these traits so that work changes no
//! engine code.

use crate::secret::Secret;

/// Why a write did not happen.
///
/// Typed rather than stringly, because the executor's behaviour differs per
/// case: a wrong PIN with retries left is worth reporting to the operator and
/// stopping, a locked applet needs a reset, and an unsupported operation is a
/// skip rather than a failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    #[error("no key with serial {0} is attached")]
    NotAttached(u32),
    #[error("the current {applet} secret was not accepted ({retries_left} attempts left)")]
    WrongSecret {
        applet: &'static str,
        retries_left: u8,
    },
    #[error("the {applet} applet is locked and needs a reset before it can be configured")]
    Locked { applet: &'static str },
    #[error("this key does not support {operation}: {reason}")]
    Unsupported {
        operation: &'static str,
        reason: String,
    },
    #[error("{operation} needs the `{feature}` feature, which this build does not have")]
    TransportUnavailable {
        operation: &'static str,
        feature: &'static str,
    },
    #[error("the key was removed during {operation}")]
    Detached { operation: &'static str },
    #[error("{operation} failed: {reason}")]
    Failed {
        operation: &'static str,
        reason: String,
    },
}

impl WriteError {
    /// Can the run carry on to the next step after this?
    ///
    /// A detached key is the one case where continuing is meaningless: every
    /// later step would fail the same way, and the run should suspend so it can
    /// be resumed once the key is back (`features/bootstrap-engine.md` phase 9).
    pub fn is_fatal_to_the_run(&self) -> bool {
        matches!(
            self,
            WriteError::Detached { .. } | WriteError::NotAttached(_)
        )
    }

    /// Secret-free, and short enough for a `StepOutcome::detail`.
    ///
    /// The `Display` impls above are already secret-free — none of them
    /// interpolates a value, only a retry count and an applet name — so this is
    /// the whole redaction story for the error path.
    pub fn detail(&self) -> String {
        self.to_string()
    }
}

pub type Result<T> = std::result::Result<T, WriteError>;

/// What the FIDO2 applet currently looks like, for idempotency decisions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fido2State {
    pub pin_set: bool,
    /// `None` when the firmware predates the CTAP 2.1 minimum-length policy.
    pub min_pin_length: Option<u8>,
    pub force_pin_change_set: bool,
    pub resident_credentials: usize,
}

/// What the PIV applet currently looks like.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PivState {
    /// Slots holding a certificate, e.g. `["9c"]`.
    pub occupied_slots: Vec<String>,
    /// False while the applet still has Yubico's published default.
    pub management_key_changed: bool,
    pub pin_changed_from_default: bool,
}

impl PivState {
    pub fn slot_occupied(&self, slot: &str) -> bool {
        self.occupied_slots.iter().any(|s| s == slot)
    }
}

/// What the OTP applet currently looks like.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OtpState {
    pub slot_one_programmed: bool,
    pub slot_two_programmed: bool,
    /// True when a slot is already write-protected by an access code we would
    /// have to supply to change it.
    pub access_code_set: bool,
}

/// A request to create the initial resident credential.
///
/// `ykman` cannot do this at all — it can list and delete credentials but not
/// create one — which is the single strongest argument for the native transport
/// (`features/step-fido2-credentials.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialRequest {
    /// The relying-party id the credential is bound to.
    pub relying_party: String,
    pub relying_party_name: String,
    /// The holder's identifier within that relying party.
    pub user_name: String,
    pub user_display_name: String,
    /// Resident (discoverable) — the point of the step.
    pub resident: bool,
    pub require_user_verification: bool,
}

/// Evidence a credential was created. Never the credential's private key, which
/// never leaves the device by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialEvidence {
    pub credential_id_hex: String,
    pub relying_party: String,
    pub algorithm: String,
}

/// Evidence of an on-device key generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeygenEvidence {
    pub slot: String,
    pub algorithm: String,
    /// PEM public key. Public by definition, so safe to store with the run.
    pub public_key_pem: String,
}

// ----------------------------------------------------------------- the traits

/// FIDO2 / CTAP2 writes.
pub trait Fido2Writer {
    /// Read the applet's current state, so a step can decide to skip.
    fn fido2_state(&mut self, serial: u32) -> Result<Fido2State>;

    /// Set the PIN on a key that has none.
    fn set_pin(&mut self, serial: u32, new: &Secret) -> Result<()>;

    /// Change a PIN that is already set.
    fn change_pin(&mut self, serial: u32, current: &Secret, new: &Secret) -> Result<()>;

    /// Raise the minimum PIN length (firmware 5.7+).
    fn set_min_pin_length(&mut self, serial: u32, length: u8, pin: &Secret) -> Result<()>;

    /// Mark the key so the holder must replace the transport PIN before first
    /// use. The mechanism model B depends on, where the firmware has it.
    fn force_pin_change(&mut self, serial: u32, pin: &Secret) -> Result<()>;

    /// Create the initial discoverable credential, resident on the key.
    fn make_credential(
        &mut self,
        serial: u32,
        request: &CredentialRequest,
        pin: &Secret,
    ) -> Result<CredentialEvidence>;
}

/// PIV writes.
pub trait PivWriter {
    fn piv_state(&mut self, serial: u32) -> Result<PivState>;

    /// Replace the PIN and PUK.
    ///
    /// `current_*` is `None` for a factory-fresh key, meaning "authenticate with
    /// the applet's published default". The default itself deliberately does not
    /// appear in this repository: `AGENTS.md` §2 bans credentials in the source,
    /// and the distinction between a vendor's published default and a real
    /// credential is not one worth relying on in a grep. The implementation gets
    /// it from the transport crate, which has to know it anyway.
    fn set_pin_and_puk(
        &mut self,
        serial: u32,
        current_pin: Option<&Secret>,
        new_pin: &Secret,
        current_puk: Option<&Secret>,
        new_puk: &Secret,
    ) -> Result<()>;

    /// Replace the management key. `protect` stores it on the key under the PIN,
    /// so nothing has to be handed over or retained — the preferred form under
    /// model B (`features/step-piv-pin-puk-management-key.md`).
    fn set_management_key(
        &mut self,
        serial: u32,
        current: Option<&Secret>,
        new: &Secret,
        protect: bool,
        pin: &Secret,
    ) -> Result<()>;

    /// Generate the signing key **on the device**. It is never imported, so the
    /// private key cannot have existed anywhere else.
    fn generate_key(
        &mut self,
        serial: u32,
        slot: &str,
        algorithm: &str,
        pin: &Secret,
    ) -> Result<KeygenEvidence>;

    /// Build a CSR for the generated key, with the holder's e-mail as an
    /// `rfc822Name` SAN — the requirement `ykman` cannot meet, and the reason
    /// this step is native (`features/step-piv-signing-certificate.md`).
    fn create_csr(
        &mut self,
        serial: u32,
        slot: &str,
        subject: &str,
        san_email: &str,
        pin: &Secret,
    ) -> Result<String>;

    /// Import the issued certificate.
    fn import_certificate(
        &mut self,
        serial: u32,
        slot: &str,
        certificate_pem: &str,
        pin: &Secret,
    ) -> Result<()>;
}

/// Yubico OTP writes.
pub trait OtpWriter {
    fn otp_state(&mut self, serial: u32) -> Result<OtpState>;

    /// Write the six-byte access code that write-protects a slot.
    fn set_access_code(&mut self, serial: u32, slot: u8, code: &Secret) -> Result<()>;

    /// Program a slot. `access_code` is required once the slot is protected.
    fn program_slot(
        &mut self,
        serial: u32,
        slot: u8,
        configuration: &str,
        access_code: Option<&Secret>,
    ) -> Result<()>;
}

/// Everything needed to write to one key.
///
/// A blanket supertrait rather than three separate handles, because there is one
/// physical key: three `&mut dyn` would model three, and the borrow checker
/// rightly refuses to hand out three mutable borrows of the one mock. A build
/// that lacks a transport implements its methods by returning
/// [`WriteError::TransportUnavailable`], which is the honest answer and is what
/// the plan already shows the operator.
pub trait WriteBackend: Fido2Writer + PivWriter + OtpWriter {}

impl<T: Fido2Writer + PivWriter + OtpWriter> WriteBackend for T {}

// ------------------------------------------------------------------- the mock

/// One recorded call. **Never the secret**, only that one was supplied.
///
/// This is the shape that makes the leak test possible: if the mock kept the
/// values it was handed, the "no sink holds a secret" sweep would have to make an
/// exception for it, and an exception in that sweep is how a real leak survives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCall {
    pub operation: &'static str,
    pub serial: u32,
    /// Non-secret arguments, for asserting a step passed the right slot, length
    /// or subject.
    pub arguments: Vec<String>,
    /// How many secrets the call carried.
    pub secrets_supplied: usize,
}

/// A write backend that records what it was asked to do and can be made to fail.
///
/// Implements all three applet traits, because a test drives a whole run and a
/// separate mock per applet would mean three sets of expectations to keep in
/// step.
#[derive(Debug, Default)]
pub struct MockWriter {
    calls: Vec<RecordedCall>,
    /// Queued failures, keyed by operation name, popped as they are hit.
    failures: Vec<(&'static str, WriteError)>,
    fido2: Fido2State,
    piv: PivState,
    otp: OtpState,
    attached: Option<u32>,
}

impl MockWriter {
    /// A key that is attached and completely factory-fresh.
    pub fn factory_fresh(serial: u32) -> Self {
        Self {
            attached: Some(serial),
            ..Default::default()
        }
    }

    /// Start from a given applet state — for the idempotency scenarios, where the
    /// point is that the key has already been part-configured.
    pub fn with_fido2_state(mut self, state: Fido2State) -> Self {
        self.fido2 = state;
        self
    }

    pub fn with_piv_state(mut self, state: PivState) -> Self {
        self.piv = state;
        self
    }

    pub fn with_otp_state(mut self, state: OtpState) -> Self {
        self.otp = state;
        self
    }

    /// Make the next call to `operation` fail.
    pub fn fail(mut self, operation: &'static str, error: WriteError) -> Self {
        self.failures.push((operation, error));
        self
    }

    pub fn calls(&self) -> &[RecordedCall] {
        &self.calls
    }

    /// Every operation performed, in order — the readable form for an assertion.
    pub fn operations(&self) -> Vec<&'static str> {
        self.calls.iter().map(|c| c.operation).collect()
    }

    pub fn was_called(&self, operation: &str) -> bool {
        self.calls.iter().any(|c| c.operation == operation)
    }

    /// Operations that authenticate with the FIDO2 PIN.
    ///
    /// Everything here becomes unusable once `forcePINChange` is set, which is
    /// the rule below.
    const USES_FIDO2_PIN: [&'static str; 3] = [
        "fido2.set_min_pin_length",
        "fido2.force_pin_change",
        "fido2.make_credential",
    ];

    fn record(
        &mut self,
        operation: &'static str,
        serial: u32,
        arguments: Vec<String>,
        secrets_supplied: usize,
    ) -> Result<()> {
        if self.attached != Some(serial) {
            return Err(WriteError::NotAttached(serial));
        }

        // A key marked `forcePINChange` refuses its PIN for everything except
        // changing it. That is what the flag *means*, and it is the ordering
        // constraint that made the shipped standard procedure impossible to run:
        // marking the key before creating the resident credential left the
        // credential step holding a PIN the authenticator would no longer accept.
        //
        // Found on real hardware (YubiKey 5.7.4) rather than here, because the
        // mock used to allow it. It is modelled now so that a template which
        // reintroduces the ordering fails in `cargo test` instead of in front of
        // an operator with a key in their hand.
        if self.fido2.force_pin_change_set && Self::USES_FIDO2_PIN.contains(&operation) {
            self.calls.push(RecordedCall {
                operation,
                serial,
                arguments,
                secrets_supplied,
            });
            return Err(WriteError::WrongSecret {
                applet: "FIDO2",
                retries_left: 8,
            });
        }
        if let Some(index) = self.failures.iter().position(|(op, _)| *op == operation) {
            let (_, error) = self.failures.remove(index);
            // Recorded even though it failed: an executor that retried a failing
            // step, or skipped it silently, should be visible to a test.
            self.calls.push(RecordedCall {
                operation,
                serial,
                arguments,
                secrets_supplied,
            });
            return Err(error);
        }
        self.calls.push(RecordedCall {
            operation,
            serial,
            arguments,
            secrets_supplied,
        });
        Ok(())
    }
}

impl Fido2Writer for MockWriter {
    fn fido2_state(&mut self, serial: u32) -> Result<Fido2State> {
        if self.attached != Some(serial) {
            return Err(WriteError::NotAttached(serial));
        }
        Ok(self.fido2.clone())
    }

    fn set_pin(&mut self, serial: u32, new: &Secret) -> Result<()> {
        self.record("fido2.set_pin", serial, vec![], 1)?;
        let _ = new;
        self.fido2.pin_set = true;
        Ok(())
    }

    fn change_pin(&mut self, serial: u32, current: &Secret, new: &Secret) -> Result<()> {
        self.record("fido2.change_pin", serial, vec![], 2)?;
        let _ = (current, new);
        self.fido2.pin_set = true;
        Ok(())
    }

    fn set_min_pin_length(&mut self, serial: u32, length: u8, pin: &Secret) -> Result<()> {
        self.record(
            "fido2.set_min_pin_length",
            serial,
            vec![length.to_string()],
            1,
        )?;
        let _ = pin;
        self.fido2.min_pin_length = Some(length);
        Ok(())
    }

    fn force_pin_change(&mut self, serial: u32, pin: &Secret) -> Result<()> {
        self.record("fido2.force_pin_change", serial, vec![], 1)?;
        let _ = pin;
        self.fido2.force_pin_change_set = true;
        Ok(())
    }

    fn make_credential(
        &mut self,
        serial: u32,
        request: &CredentialRequest,
        pin: &Secret,
    ) -> Result<CredentialEvidence> {
        self.record(
            "fido2.make_credential",
            serial,
            vec![
                request.relying_party.clone(),
                request.user_name.clone(),
                format!("resident={}", request.resident),
            ],
            1,
        )?;
        let _ = pin;
        self.fido2.resident_credentials += 1;
        Ok(CredentialEvidence {
            credential_id_hex: format!("{:08x}", serial),
            relying_party: request.relying_party.clone(),
            algorithm: "ES256".into(),
        })
    }
}

impl PivWriter for MockWriter {
    fn piv_state(&mut self, serial: u32) -> Result<PivState> {
        if self.attached != Some(serial) {
            return Err(WriteError::NotAttached(serial));
        }
        Ok(self.piv.clone())
    }

    fn set_pin_and_puk(
        &mut self,
        serial: u32,
        current_pin: Option<&Secret>,
        new_pin: &Secret,
        current_puk: Option<&Secret>,
        new_puk: &Secret,
    ) -> Result<()> {
        let supplied = 2 + usize::from(current_pin.is_some()) + usize::from(current_puk.is_some());
        self.record(
            "piv.set_pin_and_puk",
            serial,
            vec![format!("from_default={}", current_pin.is_none())],
            supplied,
        )?;
        let _ = (current_pin, new_pin, current_puk, new_puk);
        self.piv.pin_changed_from_default = true;
        Ok(())
    }

    fn set_management_key(
        &mut self,
        serial: u32,
        current: Option<&Secret>,
        new: &Secret,
        protect: bool,
        pin: &Secret,
    ) -> Result<()> {
        self.record(
            "piv.set_management_key",
            serial,
            vec![format!("protect={protect}")],
            2 + usize::from(current.is_some()),
        )?;
        let _ = (current, new, pin);
        self.piv.management_key_changed = true;
        Ok(())
    }

    fn generate_key(
        &mut self,
        serial: u32,
        slot: &str,
        algorithm: &str,
        pin: &Secret,
    ) -> Result<KeygenEvidence> {
        self.record(
            "piv.generate_key",
            serial,
            vec![slot.to_owned(), algorithm.to_owned()],
            1,
        )?;
        let _ = pin;
        Ok(KeygenEvidence {
            slot: slot.to_owned(),
            algorithm: algorithm.to_owned(),
            public_key_pem: "-----BEGIN PUBLIC KEY-----\nMOCK\n-----END PUBLIC KEY-----\n".into(),
        })
    }

    fn create_csr(
        &mut self,
        serial: u32,
        slot: &str,
        subject: &str,
        san_email: &str,
        pin: &Secret,
    ) -> Result<String> {
        self.record(
            "piv.create_csr",
            serial,
            vec![slot.to_owned(), subject.to_owned(), san_email.to_owned()],
            1,
        )?;
        let _ = pin;
        Ok("-----BEGIN CERTIFICATE REQUEST-----\nMOCK\n-----END CERTIFICATE REQUEST-----\n".into())
    }

    fn import_certificate(
        &mut self,
        serial: u32,
        slot: &str,
        certificate_pem: &str,
        pin: &Secret,
    ) -> Result<()> {
        self.record(
            "piv.import_certificate",
            serial,
            vec![slot.to_owned(), format!("bytes={}", certificate_pem.len())],
            1,
        )?;
        let _ = pin;
        self.piv.occupied_slots.push(slot.to_owned());
        Ok(())
    }
}

impl OtpWriter for MockWriter {
    fn otp_state(&mut self, serial: u32) -> Result<OtpState> {
        if self.attached != Some(serial) {
            return Err(WriteError::NotAttached(serial));
        }
        Ok(self.otp.clone())
    }

    fn set_access_code(&mut self, serial: u32, slot: u8, code: &Secret) -> Result<()> {
        self.record("otp.set_access_code", serial, vec![slot.to_string()], 1)?;
        let _ = code;
        self.otp.access_code_set = true;
        Ok(())
    }

    fn program_slot(
        &mut self,
        serial: u32,
        slot: u8,
        configuration: &str,
        access_code: Option<&Secret>,
    ) -> Result<()> {
        self.record(
            "otp.program_slot",
            serial,
            vec![slot.to_string(), configuration.to_owned()],
            usize::from(access_code.is_some()),
        )?;
        match slot {
            1 => self.otp.slot_one_programmed = true,
            _ => self.otp.slot_two_programmed = true,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::SecretKind;

    fn pin() -> Secret {
        Secret::generate(SecretKind::Fido2Pin, 8).unwrap()
    }

    #[test]
    fn the_mock_records_that_a_secret_was_supplied_and_never_which() {
        // The property that lets the secret-leak sweep treat the mock as just
        // another sink instead of an exception.
        let mut writer = MockWriter::factory_fresh(20_423_633);
        let secret = pin();
        writer.set_pin(20_423_633, &secret).unwrap();

        let call = &writer.calls()[0];
        assert_eq!(call.operation, "fido2.set_pin");
        assert_eq!(call.secrets_supplied, 1);

        let rendered = format!("{writer:?}");
        assert!(
            !rendered.contains(secret.expose()),
            "the mock must not retain the value it was handed"
        );
    }

    #[test]
    fn a_write_to_a_key_that_is_not_there_is_refused_before_anything_else() {
        let mut writer = MockWriter::factory_fresh(20_423_633);
        assert_eq!(
            writer.set_pin(999, &pin()),
            Err(WriteError::NotAttached(999))
        );
        assert!(writer.calls().is_empty(), "nothing was attempted");
    }

    #[test]
    fn a_queued_failure_fires_once_and_is_still_recorded() {
        let mut writer = MockWriter::factory_fresh(20_423_633).fail(
            "fido2.set_pin",
            WriteError::WrongSecret {
                applet: "FIDO2",
                retries_left: 2,
            },
        );

        assert!(writer.set_pin(20_423_633, &pin()).is_err());
        assert!(
            writer.was_called("fido2.set_pin"),
            "a failed attempt is still an attempt, and a test should see it"
        );
        // The queue is spent, so a retry succeeds.
        assert!(writer.set_pin(20_423_633, &pin()).is_ok());
    }

    #[test]
    fn write_errors_say_what_happened_without_naming_a_value() {
        let errors = [
            WriteError::WrongSecret {
                applet: "PIV",
                retries_left: 1,
            },
            WriteError::Locked { applet: "FIDO2" },
            WriteError::Unsupported {
                operation: "minimum PIN length",
                reason: "firmware 5.4.3 predates CTAP 2.1".into(),
            },
            WriteError::Detached {
                operation: "piv.generate_key",
            },
        ];
        for error in errors {
            let detail = error.detail();
            assert!(!detail.is_empty());
            assert!(
                !detail.contains("123456"),
                "no error interpolates a secret: {detail}"
            );
        }
    }

    #[test]
    fn a_detached_key_is_fatal_to_the_run_and_a_wrong_pin_is_not() {
        assert!(
            WriteError::Detached {
                operation: "fido2.set_pin"
            }
            .is_fatal_to_the_run()
        );
        assert!(
            !WriteError::WrongSecret {
                applet: "FIDO2",
                retries_left: 2
            }
            .is_fatal_to_the_run(),
            "a wrong PIN stops the step, not the ability to talk to the key"
        );
    }

    #[test]
    fn the_mock_advances_the_applet_state_so_idempotency_can_be_tested() {
        let mut writer = MockWriter::factory_fresh(20_423_633);
        assert!(!writer.fido2_state(20_423_633).unwrap().pin_set);

        writer.set_pin(20_423_633, &pin()).unwrap();
        writer.force_pin_change(20_423_633, &pin()).unwrap();

        let state = writer.fido2_state(20_423_633).unwrap();
        assert!(state.pin_set);
        assert!(state.force_pin_change_set);
    }

    #[test]
    fn a_part_configured_key_can_be_described_up_front() {
        // The starting point for "this key is already bootstrapped".
        let mut writer = MockWriter::factory_fresh(20_423_633).with_piv_state(PivState {
            occupied_slots: vec!["9c".into()],
            management_key_changed: true,
            pin_changed_from_default: true,
        });
        let state = writer.piv_state(20_423_633).unwrap();
        assert!(state.slot_occupied("9c"));
        assert!(!state.slot_occupied("9a"));
    }

    #[test]
    fn non_secret_arguments_are_recorded_so_a_step_can_be_checked() {
        let mut writer = MockWriter::factory_fresh(20_423_633);
        writer
            .create_csr(
                20_423_633,
                "9c",
                "CN=Ana Silva,OU=ESI",
                "ana@example.org",
                &pin(),
            )
            .unwrap();
        let call = &writer.calls()[0];
        assert_eq!(call.arguments[0], "9c");
        assert!(
            call.arguments[2].contains("ana@example.org"),
            "the SAN is the point of this step"
        );
    }
}
