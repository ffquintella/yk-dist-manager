//! One key, three applets, and however many transports this build has.
//!
//! [`super::write::WriteBackend`] is a supertrait of all three applet traits,
//! because there is one physical key. But the transports arrive one at a time —
//! FIDO2 is hardware-verified, PIV is partly there, OTP has not started — and
//! they sit behind separate Cargo features. This routes each applet to whatever
//! is compiled in, and answers [`WriteError::TransportUnavailable`] for the rest.
//!
//! That answer matters more than it looks. A missing transport is **not** an
//! error the operator caused, and it is not a key that is broken: it is a build
//! that cannot do a thing. Saying so with the feature name attached is what lets
//! the pre-flight turn it into "this step will skip" on a screen rather than a
//! failure seven steps into a key.

use super::write::{
    CredentialEvidence, CredentialRequest, Fido2State, Fido2Writer, KeygenEvidence, OtpState,
    OtpWriter, PivState, PivWriter, Result, WriteError,
};
use crate::secret::Secret;

/// Every write transport this build has, for one key.
pub struct NativeBackend {
    #[allow(dead_code)]
    serial: u32,
    #[cfg(feature = "native-fido")]
    fido2: super::native_fido::NativeFido2,
    #[cfg(feature = "native-piv")]
    piv: super::native_piv::NativePiv,
}

impl NativeBackend {
    pub fn for_key(serial: u32) -> Self {
        Self {
            serial,
            #[cfg(feature = "native-fido")]
            fido2: super::native_fido::NativeFido2::for_key(serial),
            #[cfg(feature = "native-piv")]
            piv: super::native_piv::NativePiv::for_key(serial),
        }
    }

    /// Does this build have any transport at all?
    pub const fn is_available() -> bool {
        cfg!(any(feature = "native-fido", feature = "native-piv"))
    }
}

fn unavailable(operation: &'static str, feature: &'static str) -> WriteError {
    WriteError::TransportUnavailable { operation, feature }
}

impl Fido2Writer for NativeBackend {
    fn fido2_state(&mut self, serial: u32) -> Result<Fido2State> {
        #[cfg(feature = "native-fido")]
        return self.fido2.fido2_state(serial);
        #[cfg(not(feature = "native-fido"))]
        {
            let _ = serial;
            Err(unavailable("fido2.get_info", "native-fido"))
        }
    }

    fn set_pin(&mut self, serial: u32, new: &Secret) -> Result<()> {
        #[cfg(feature = "native-fido")]
        return self.fido2.set_pin(serial, new);
        #[cfg(not(feature = "native-fido"))]
        {
            let _ = (serial, new);
            Err(unavailable("fido2.set_pin", "native-fido"))
        }
    }

    fn change_pin(&mut self, serial: u32, current: &Secret, new: &Secret) -> Result<()> {
        #[cfg(feature = "native-fido")]
        return self.fido2.change_pin(serial, current, new);
        #[cfg(not(feature = "native-fido"))]
        {
            let _ = (serial, current, new);
            Err(unavailable("fido2.change_pin", "native-fido"))
        }
    }

    fn set_min_pin_length(&mut self, serial: u32, length: u8, pin: &Secret) -> Result<()> {
        #[cfg(feature = "native-fido")]
        return self.fido2.set_min_pin_length(serial, length, pin);
        #[cfg(not(feature = "native-fido"))]
        {
            let _ = (serial, length, pin);
            Err(unavailable("fido2.set_min_pin_length", "native-fido"))
        }
    }

    fn force_pin_change(&mut self, serial: u32, pin: &Secret) -> Result<()> {
        #[cfg(feature = "native-fido")]
        return self.fido2.force_pin_change(serial, pin);
        #[cfg(not(feature = "native-fido"))]
        {
            let _ = (serial, pin);
            Err(unavailable("fido2.force_pin_change", "native-fido"))
        }
    }

    fn make_credential(
        &mut self,
        serial: u32,
        request: &CredentialRequest,
        pin: &Secret,
    ) -> Result<CredentialEvidence> {
        #[cfg(feature = "native-fido")]
        return self.fido2.make_credential(serial, request, pin);
        #[cfg(not(feature = "native-fido"))]
        {
            let _ = (serial, request, pin);
            Err(unavailable("fido2.make_credential", "native-fido"))
        }
    }
}

impl PivWriter for NativeBackend {
    fn piv_state(&mut self, serial: u32) -> Result<PivState> {
        #[cfg(feature = "native-piv")]
        return self.piv.piv_state(serial);
        #[cfg(not(feature = "native-piv"))]
        {
            let _ = serial;
            Err(unavailable("piv.metadata", "native-piv"))
        }
    }

    fn set_pin_and_puk(
        &mut self,
        serial: u32,
        current_pin: Option<&Secret>,
        new_pin: &Secret,
        current_puk: Option<&Secret>,
        new_puk: &Secret,
    ) -> Result<()> {
        #[cfg(feature = "native-piv")]
        return self
            .piv
            .set_pin_and_puk(serial, current_pin, new_pin, current_puk, new_puk);
        #[cfg(not(feature = "native-piv"))]
        {
            let _ = (serial, current_pin, new_pin, current_puk, new_puk);
            Err(unavailable("piv.set_pin_and_puk", "native-piv"))
        }
    }

    fn set_management_key(
        &mut self,
        serial: u32,
        current: Option<&Secret>,
        new: &Secret,
        protect: bool,
        pin: &Secret,
    ) -> Result<()> {
        #[cfg(feature = "native-piv")]
        return self
            .piv
            .set_management_key(serial, current, new, protect, pin);
        #[cfg(not(feature = "native-piv"))]
        {
            let _ = (serial, current, new, protect, pin);
            Err(unavailable("piv.set_management_key", "native-piv"))
        }
    }

    fn generate_key(
        &mut self,
        serial: u32,
        slot: &str,
        algorithm: &str,
        pin: &Secret,
    ) -> Result<KeygenEvidence> {
        #[cfg(feature = "native-piv")]
        return self.piv.generate_key(serial, slot, algorithm, pin);
        #[cfg(not(feature = "native-piv"))]
        {
            let _ = (serial, slot, algorithm, pin);
            Err(unavailable("piv.generate_key", "native-piv"))
        }
    }

    fn attest(&mut self, serial: u32, slot: &str) -> Result<String> {
        #[cfg(feature = "native-piv")]
        return self.piv.attest(serial, slot);
        #[cfg(not(feature = "native-piv"))]
        {
            let _ = (serial, slot);
            Err(unavailable("piv.attest", "native-piv"))
        }
    }

    fn create_csr(
        &mut self,
        serial: u32,
        slot: &str,
        subject: &str,
        san_email: &str,
        pin: &Secret,
    ) -> Result<String> {
        #[cfg(feature = "native-piv")]
        return self.piv.create_csr(serial, slot, subject, san_email, pin);
        #[cfg(not(feature = "native-piv"))]
        {
            let _ = (serial, slot, subject, san_email, pin);
            Err(unavailable("piv.create_csr", "native-piv"))
        }
    }

    fn import_certificate(
        &mut self,
        serial: u32,
        slot: &str,
        certificate_pem: &str,
        pin: &Secret,
    ) -> Result<()> {
        #[cfg(feature = "native-piv")]
        return self
            .piv
            .import_certificate(serial, slot, certificate_pem, pin);
        #[cfg(not(feature = "native-piv"))]
        {
            let _ = (serial, slot, certificate_pem, pin);
            Err(unavailable("piv.import_certificate", "native-piv"))
        }
    }

    fn read_certificate(&mut self, serial: u32, slot: &str) -> Result<Option<String>> {
        #[cfg(feature = "native-piv")]
        return self.piv.read_certificate(serial, slot);
        #[cfg(not(feature = "native-piv"))]
        {
            let _ = (serial, slot);
            Err(unavailable("piv.read_certificate", "native-piv"))
        }
    }
}

/// OTP goes through **`ykman`**, and is the one applet that does.
///
/// Not an oversight and not laziness: no crate in this dependency graph exposes
/// the Yubico OTP configuration frame, so the native path means hand-rolling the
/// protocol — and `features/step-otp-access-code.md` phase 4 keeps that
/// deliberately unwritten until there is a key to verify it against, because the
/// failure mode of a wrong frame is a slot write-protected by a code nobody holds.
///
/// So this routes to the subprocess, which is what
/// `features/step-otp-access-code.md` phase 2 asks for, and it is honest on screen:
/// the planner marks the OTP steps' native op as unavailable, so the plan the
/// operator confirms already reads `ykman (fallback)` against them.
impl OtpWriter for NativeBackend {
    fn otp_state(&mut self, serial: u32) -> Result<OtpState> {
        super::ykman::otp_state(&super::YkmanBackend::default(), serial).map_err(|e| {
            WriteError::Failed {
                operation: "otp.state",
                reason: e.to_string(),
            }
        })
    }

    fn set_access_code(&mut self, serial: u32, slot: u8, code: &Secret) -> Result<()> {
        super::ykman::set_otp_access_code(&super::YkmanBackend::default(), serial, slot, code)
    }

    fn program_slot(
        &mut self,
        _serial: u32,
        _slot: u8,
        _configuration: &str,
        _access_code: Option<&Secret>,
    ) -> Result<()> {
        // Optional slot programming is `features/step-otp-access-code.md` phase 5,
        // and it is a *different* operation from protecting a slot: it writes a
        // credential, which needs the deployment to have decided what the slot is
        // for (challenge-response for a local unlock, a static password, a Yubico
        // OTP registered with somebody). Nobody has, so this stays unimplemented
        // rather than guessing at a configuration to put on a key.
        Err(unavailable("otp.program_slot", "native-otp"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::SecretKind;

    #[test]
    fn a_transport_this_build_lacks_names_the_feature_that_would_provide_it() {
        // The message is what the pre-flight turns into "this step will skip",
        // so it has to say what is missing rather than that something failed.
        //
        // Slot *programming* is the one that is still unimplemented — protecting a
        // slot goes through `ykman` since `step-otp-access-code.md` phase 2.
        let mut backend = NativeBackend::for_key(20_423_633);
        let error = backend
            .program_slot(20_423_633, 2, "challenge-response", None)
            .unwrap_err();
        assert!(
            matches!(
                error,
                WriteError::TransportUnavailable {
                    feature: "native-otp",
                    ..
                }
            ),
            "got {error:?}"
        );
        assert!(error.detail().contains("native-otp"), "{}", error.detail());
    }

    #[test]
    fn an_access_code_of_the_wrong_length_is_refused_before_any_subprocess_starts() {
        // The check is on the length, which is the one thing about this value that
        // can be checked without looking at it — and it has to happen here, because
        // `ykman`'s own refusal would arrive as a parse error about a value this
        // code must never print.
        let short = Secret::generate(SecretKind::Fido2Pin, 6).unwrap();
        let error = super::super::ykman::set_otp_access_code(
            &super::super::YkmanBackend::default(),
            1,
            1,
            &short,
        )
        .unwrap_err();
        assert!(error.detail().contains("six bytes"), "{}", error.detail());
        assert!(
            !error.detail().contains(short.expose()),
            "the refusal must not carry the value"
        );
    }

    #[test]
    fn a_slot_a_yubikey_does_not_have_is_refused_rather_than_attempted() {
        let code = Secret::generate(SecretKind::OtpAccessCode, 0).unwrap();
        let error = super::super::ykman::set_otp_access_code(
            &super::super::YkmanBackend::default(),
            1,
            3,
            &code,
        )
        .unwrap_err();
        assert!(
            error.detail().contains("slots 1 and 2"),
            "{}",
            error.detail()
        );
    }

    #[test]
    fn availability_follows_the_features_actually_compiled_in() {
        assert_eq!(
            NativeBackend::is_available(),
            cfg!(any(feature = "native-fido", feature = "native-piv"))
        );
    }
}
