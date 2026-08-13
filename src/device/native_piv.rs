//! PIV writes over PC/SC, using the [`yubikey`] crate.
//!
//! Companion to [`super::native`], which is the read-only half. Everything here
//! changes the card, and the split exists so a screen holding only a reader
//! cannot reach these by accident.
//!
//! ## This module depends on `yubikey/untested`
//!
//! Stated up front because it is the most important thing about it. Every
//! mutating PIV call — `change_pin`, `change_puk`, and the three `MgmKey`
//! setters — is behind that crate feature, which is upstream's own name for
//! "we have not exercised this". The read paths are not gated.
//!
//! The decision to enable it was taken deliberately rather than by default, and
//! the alternatives are recorded in
//! `features/step-piv-pin-puk-management-key.md`. What that decision obliges:
//! **every operation here is verified against a dedicated test key before it is
//! relied on**, by `examples/verify_piv_write.rs`, and the results go in the
//! phase notes. The failure this guards against is not theoretical — a
//! management key set to a value nobody holds leaves the applet
//! administratively dead, recoverable only by a reset that destroys the signing
//! certificate and the key behind it.
//!
//! ## Idempotency without burning a retry
//!
//! PIV allows three PIN attempts and then blocks the applet, so "is the PIN
//! still the factory default?" must never be answered by trying it. Firmware
//! 5.2.3+ answers it properly: `GET METADATA` on the PIN, PUK and management
//! slots reports whether each is still default. On older firmware the answer is
//! unknown, and unknown is reported as *not* default — the conservative
//! direction, because it makes the executor attempt the change and fail visibly
//! rather than skip and leave a factory PIN on a key going out to a holder.
//!
//! ## The management key, and why `set_protected`
//!
//! Custody model B wants nothing retained. `MgmKey::set_protected` stores a
//! random management key on the card, guarded by the PIN, so there is nothing to
//! hand over, escrow or lose. That is the default; `protect = false` falls back
//! to `set_manual`, which produces a key somebody then has to keep.

use yubikey::YubiKey;
use yubikey::piv::{self, AlgorithmId, ManagementSlotId, SlotId};

use super::write::{KeygenEvidence, PivState, PivWriter, Result, WriteError};
use crate::secret::Secret;

/// The PIV applet's factory PIN and PUK, as published by the vendor.
///
/// These are **not credentials of any deployment**: they are the documented
/// values every PIV applet ships with, the same constants `ykman` and
/// `yubico-piv-tool` carry, and they are what "this key is factory fresh" means.
/// `AGENTS.md` §2 bans credentials in the repository — a published factory
/// default is the one thing that is categorically not one, and the alternative
/// (asking the operator to type a value the applet already documents) would
/// invite them to type a real PIN into a field labelled "current".
///
/// They are used only when the caller passes `None`, which is its way of saying
/// "this key has not been configured yet".
const FACTORY_PIN: &[u8] = b"123456";
const FACTORY_PUK: &[u8] = b"12345678";
/// The published factory management key, for the same reason as the two above.
const FACTORY_MANAGEMENT_KEY: &[u8] = &[
    1, 2, 3, 4, 5, 6, 7, 8, 1, 2, 3, 4, 5, 6, 7, 8, 1, 2, 3, 4, 5, 6, 7, 8,
];

/// PIV writes against one key, selected by serial.
///
/// Unlike the FIDO2 transport, PC/SC *does* expose the serial, so this opens the
/// exact key the run is about rather than "the only one attached".
pub struct NativePiv {
    serial: u32,
}

impl NativePiv {
    pub fn for_key(serial: u32) -> Self {
        Self { serial }
    }

    fn open(&self, serial: u32, operation: &'static str) -> Result<YubiKey> {
        if serial != self.serial {
            return Err(WriteError::NotAttached(serial));
        }
        YubiKey::open_by_serial(serial.into()).map_err(|e| match e {
            yubikey::Error::NotFound => WriteError::NotAttached(serial),
            other => WriteError::Failed {
                operation,
                reason: other.to_string(),
            },
        })
    }

    fn classify(operation: &'static str, error: yubikey::Error, retries: Option<u8>) -> WriteError {
        match error {
            yubikey::Error::WrongPin { tries } => WriteError::WrongSecret {
                applet: "PIV",
                retries_left: tries,
            },
            yubikey::Error::PinLocked => WriteError::Locked { applet: "PIV" },
            yubikey::Error::NotSupported => WriteError::Unsupported {
                operation,
                reason: "this key's firmware does not support the operation".into(),
            },
            yubikey::Error::NotFound => WriteError::Detached { operation },
            yubikey::Error::AuthenticationError => WriteError::WrongSecret {
                applet: "PIV",
                retries_left: retries.unwrap_or(0),
            },
            other => WriteError::Failed {
                operation,
                reason: other.to_string(),
            },
        }
    }

    /// Is this management slot still holding the factory default?
    ///
    /// `None` means the firmware cannot say (below 5.2.3).
    fn is_default(key: &mut YubiKey, slot: ManagementSlotId) -> Option<bool> {
        piv::metadata(key, SlotId::Management(slot))
            .ok()
            .and_then(|m| m.default)
    }

    fn parse_slot(slot: &str, operation: &'static str) -> Result<SlotId> {
        let raw = u8::from_str_radix(slot.trim().trim_start_matches("0x"), 16).map_err(|_| {
            WriteError::Failed {
                operation,
                reason: format!("`{slot}` is not a PIV slot id (expected 9a, 9c, 9d or 9e)"),
            }
        })?;
        SlotId::try_from(raw).map_err(|_| WriteError::Failed {
            operation,
            reason: format!("`{slot}` is not a PIV slot this key has"),
        })
    }

    fn parse_algorithm(algorithm: &str, operation: &'static str) -> Result<AlgorithmId> {
        match algorithm
            .to_ascii_uppercase()
            .replace(['-', '_'], "")
            .as_str()
        {
            "ECCP256" | "ECDSAP256" | "P256" => Ok(AlgorithmId::EccP256),
            "ECCP384" | "ECDSAP384" | "P384" => Ok(AlgorithmId::EccP384),
            "RSA2048" => Ok(AlgorithmId::Rsa2048),
            "RSA1024" => Ok(AlgorithmId::Rsa1024),
            other => Err(WriteError::Unsupported {
                operation,
                reason: format!("`{other}` is not an algorithm this transport can generate"),
            }),
        }
    }

    /// The name for an algorithm the card reported, so the CSR builder is told what
    /// the slot actually holds rather than what the template asked for.
    ///
    /// The two can differ: a slot generated by another tool, or by an earlier version
    /// of this procedure, is still a slot this step has to build a request for.
    fn describe_algorithm(
        algorithm: piv::ManagementAlgorithmId,
        operation: &'static str,
    ) -> Result<String> {
        match algorithm {
            piv::ManagementAlgorithmId::Asymmetric(AlgorithmId::EccP256) => Ok("ECCP256".into()),
            piv::ManagementAlgorithmId::Asymmetric(AlgorithmId::EccP384) => Ok("ECCP384".into()),
            piv::ManagementAlgorithmId::Asymmetric(AlgorithmId::Rsa1024) => Ok("RSA1024".into()),
            piv::ManagementAlgorithmId::Asymmetric(AlgorithmId::Rsa2048) => Ok("RSA2048".into()),
            // The PIN/PUK and 3DES management slots. Reaching here means a slot id was
            // passed that does not hold a signing key at all, which is a mistake in the
            // template rather than a state of the card.
            other => Err(WriteError::Failed {
                operation,
                reason: format!(
                    "this slot holds {other:?}, not an asymmetric key — a certificate request \
                     needs a signing slot such as 9c"
                ),
            }),
        }
    }

    /// Return the PIV applet to factory default
    /// (`features/key-lifecycle-and-revocation.md` phase 5).
    ///
    /// **Destroys the private key and certificate in every slot**, and puts the
    /// PIN, PUK and management key back to the applet's published defaults. Not
    /// on [`PivWriter`] on purpose: that trait is the bootstrap's write surface,
    /// where every method borrows a secret, and a reset is the one operation that
    /// needs none — it is the path for a key whose PIN nobody has.
    ///
    /// ## Why it blocks the PIN and the PUK first
    ///
    /// The applet only accepts `RESET` once **both** counters are exhausted. That
    /// is the PIV card-edge's own protection against a reset being the easiest
    /// way past a PIN, and it means the sequence below is what every tool does,
    /// `ykman` included: present a wrong PIN until there are no attempts left,
    /// block the PUK, then send the command.
    ///
    /// It is therefore *deliberately destructive before it is destructive*, and
    /// an interruption between the blocking and the command leaves the applet
    /// locked. That state is recoverable — a locked applet is exactly the state
    /// `RESET` is accepted in, so running this again finishes the job — and it is
    /// why the caller confirms first.
    ///
    /// The two candidate PINs alternate for one reason: if the first happened to
    /// *be* the key's PIN, verifying it would succeed and reset the counter to
    /// three, and a loop that kept presenting it would never finish.
    pub fn reset_applet(&self, serial: u32) -> Result<String> {
        const OP: &str = "piv.reset";
        /// Enough attempts for both candidates against the maximum retry count a
        /// YubiKey allows, and a bound so a firmware that answers unexpectedly
        /// cannot spin here for ever.
        const MAX_ATTEMPTS: usize = 32;
        /// Two values that are not each other, so at least one is wrong.
        const WRONG: [&[u8]; 2] = [b"00000000", b"11111111"];

        let mut key = self.open(serial, OP)?;

        let mut which = 0;
        let mut blocked = false;
        for _ in 0..MAX_ATTEMPTS {
            match key.verify_pin(WRONG[which]) {
                // That was the PIN. One in a hundred million, and cheaper to
                // handle than to argue about: switch, and the next one fails.
                Ok(()) => which = 1 - which,
                Err(yubikey::Error::WrongPin { tries: 0 }) | Err(yubikey::Error::PinLocked) => {
                    blocked = true;
                    break;
                }
                Err(yubikey::Error::WrongPin { .. }) => continue,
                Err(e) => return Err(Self::classify(OP, e, None)),
            }
        }
        if !blocked {
            return Err(WriteError::Failed {
                operation: OP,
                reason: format!(
                    "the PIN retry counter did not reach zero in {MAX_ATTEMPTS} attempts, so the \
                     applet would refuse the reset — nothing further was tried"
                ),
            });
        }

        key.block_puk()
            .map_err(|e| Self::classify(OP, e, Some(0)))?;
        key.reset_device()
            .map_err(|e| Self::classify(OP, e, Some(0)))?;

        Ok(
            "PIV applet reset: slots cleared, PIN, PUK and management key back to the applet's \
            factory defaults"
                .to_owned(),
        )
    }

    /// Open a session that is authenticated with the management key.
    ///
    /// A key configured by this tool has its management key protected on the
    /// card, so it is fetched with the PIN rather than held anywhere.
    ///
    /// **Not the crate.** `MgmKey::get_protected` reads the right object and then
    /// authenticates with a 3DES algorithm identifier, which a firmware-5.7 slot
    /// refuses — so every operation behind it (`generate`, certificate import)
    /// failed on current hardware for a reason that had nothing to do with the
    /// operation. [`super::piv_session`] reads the slot's real algorithm and
    /// speaks that, and because PIV authentication belongs to the *session*, the
    /// write has to go out on the same connection that authenticated.
    fn authenticated(
        serial: u32,
        pin: &Secret,
        operation: &'static str,
    ) -> Result<super::piv_session::Session> {
        let mut session = super::piv_session::Session::open(serial, operation)?;
        session.authenticate_management(pin.expose(), FACTORY_MANAGEMENT_KEY, operation)?;
        Ok(session)
    }
}

impl PivWriter for NativePiv {
    fn piv_state(&mut self, serial: u32) -> Result<PivState> {
        let mut key = self.open(serial, "piv.metadata")?;

        let occupied_slots = piv::Key::list(&mut key)
            .map(|keys| {
                keys.iter()
                    .map(|k| format!("{:x}", u8::from(k.slot())))
                    .collect()
            })
            .unwrap_or_default();

        // `Some(false)` is the only reading that means "somebody has changed it".
        // `Some(true)` and `None` both leave the executor to try.
        let pin_changed_from_default =
            Self::is_default(&mut key, ManagementSlotId::Pin) == Some(false);

        // Read, never reset. `get_pin_retries` reports what is left; a failure to
        // read it is `None` rather than an error, because a retry counter is context
        // for the operator and not a reason to refuse to describe the applet.
        let pin_retries = key.get_pin_retries().ok();

        // The management slot is read on our own session rather than through the
        // crate. Measured reason: after this tool had successfully changed the
        // key, `piv::metadata` on slot 9B still reported *default*, disagreeing
        // with `ykman`. The write was right and the read was wrong, and a read
        // that under-reports here is the one that matters — it is what the
        // pre-flight uses to decide whether a key still carries a factory
        // management key.
        //
        // The crate's handle is dropped first: two connections to one reader at
        // the same time is how a session gets refused.
        drop(key);
        let management_key_changed = super::piv_session::Session::open(serial, "piv.metadata")
            .ok()
            .and_then(|mut session| session.management_key_is_default("piv.metadata"))
            == Some(false);

        Ok(PivState {
            occupied_slots,
            management_key_changed,
            pin_changed_from_default,
            pin_retries,
        })
    }

    fn set_pin_and_puk(
        &mut self,
        serial: u32,
        current_pin: Option<&Secret>,
        new_pin: &Secret,
        current_puk: Option<&Secret>,
        new_puk: &Secret,
    ) -> Result<()> {
        const OP: &str = "piv.set_pin_and_puk";
        let mut key = self.open(serial, OP)?;

        let pin_now = current_pin.map(|s| s.expose().as_bytes().to_vec());
        let puk_now = current_puk.map(|s| s.expose().as_bytes().to_vec());
        let retries = key.get_pin_retries().ok();

        key.change_pin(
            pin_now.as_deref().unwrap_or(FACTORY_PIN),
            new_pin.expose().as_bytes(),
        )
        .map_err(|e| Self::classify(OP, e, retries))?;

        key.change_puk(
            puk_now.as_deref().unwrap_or(FACTORY_PUK),
            new_puk.expose().as_bytes(),
        )
        .map_err(|e| Self::classify(OP, e, retries))?;

        Ok(())
    }

    fn set_management_key(
        &mut self,
        serial: u32,
        current: Option<&Secret>,
        new: &Secret,
        protect: bool,
        _pin: &Secret,
    ) -> Result<()> {
        const OP: &str = "piv.set_management_key";

        // Not the `yubikey` crate. Its MgmKey is a 24-byte 3DES type and its
        // authenticate sends a 3DES algorithm id, which firmware 5.7 rejects
        // outright — measured, not assumed. `super::piv_mgm` reads the slot's
        // actual algorithm and speaks that.
        let current_bytes = match current {
            Some(secret) => Some(
                hex::decode(secret.expose()).map_err(|_| WriteError::Failed {
                    operation: OP,
                    reason: "the current management key is not hex".into(),
                })?,
            ),
            None => None,
        };
        let new_bytes = hex::decode(new.expose()).map_err(|_| WriteError::Failed {
            operation: OP,
            reason: "the generated management key is not hex".into(),
        })?;

        let store = if protect {
            super::piv_mgm::ProtectedStore::OnCardUnderPin
        } else {
            super::piv_mgm::ProtectedStore::NotStored
        };

        let algorithm = super::piv_mgm::set_management_key(
            serial,
            current_bytes.as_deref(),
            &new_bytes,
            // The published factory default, supplied by the caller so the
            // constant stays in one documented place.
            FACTORY_MANAGEMENT_KEY,
            store,
            false,
        )?;

        tracing::info!(
            event = "piv.management_key.set",
            serial,
            algorithm = algorithm.label(),
            protected = protect
        );
        Ok(())
    }

    fn generate_key(
        &mut self,
        serial: u32,
        slot: &str,
        algorithm: &str,
        pin: &Secret,
    ) -> Result<KeygenEvidence> {
        const OP: &str = "piv.generate_key";
        let slot_id = Self::parse_slot(slot, OP)?;
        // Validated before anything is opened, so a template with a name the
        // transport cannot generate fails without touching the key.
        let key_algorithm = super::piv_session::KeyAlgorithm::from_name(algorithm, OP)?;

        // The session is scoped: it authenticates, generates, and is dropped
        // before the crate opens its own connection for the attestation below.
        // Two handles on one reader at once is how a session gets refused.
        let public_key_der = {
            let mut session = Self::authenticated(serial, pin, OP)?;
            session.generate_key(
                u8::from(slot_id),
                key_algorithm,
                // The signing key must require the PIN every time: this slot signs
                // documents in the holder's name, so consent per signature is the
                // point (`features/step-piv-signing-certificate.md`).
                super::piv_session::PinPolicy::Always,
                super::piv_session::TouchPolicy::Cached,
                OP,
            )?
        };

        let mut key = self.open(serial, OP)?;

        // Attest immediately, while this generation is the one in the slot. A proof
        // read later would prove whatever is there then, which is a different claim.
        // A refusal is `None`, not an error: firmware below 4.3 has no attestation,
        // and a missing proof must not turn a successful generation into a failure the
        // operator has to reason about with a key already changed.
        let attestation_pem = match piv::attest(&mut key, slot_id) {
            Ok(der) => Some(pem_encode("CERTIFICATE", der.as_ref())),
            Err(e) => {
                tracing::warn!(
                    event = "piv.attest.unavailable",
                    slot = slot,
                    reason = %e,
                    detail = "the key was generated; on-device generation is unproven"
                );
                None
            }
        };

        Ok(KeygenEvidence {
            slot: slot.to_owned(),
            algorithm: algorithm.to_owned(),
            public_key_pem: pem_encode("PUBLIC KEY", &public_key_der),
            attestation_pem,
        })
    }

    fn attest(&mut self, serial: u32, slot: &str) -> Result<String> {
        const OP: &str = "piv.attest";
        let mut key = self.open(serial, OP)?;
        let slot_id = Self::parse_slot(slot, OP)?;
        let der = piv::attest(&mut key, slot_id).map_err(|e| Self::classify(OP, e, None))?;
        Ok(pem_encode("CERTIFICATE", der.as_ref()))
    }

    fn create_csr(
        &mut self,
        serial: u32,
        slot: &str,
        subject: &str,
        san_email: &str,
        pin: &Secret,
    ) -> Result<String> {
        const OP: &str = "piv.create_csr";
        let slot_id = Self::parse_slot(slot, OP)?;

        // The public key comes from the slot's metadata rather than from the caller,
        // so the request is about the key that is actually there. A request built
        // around a public key handed in from elsewhere is a request the card cannot
        // sign, and the mismatch would only surface as a CA rejection.
        let mut key = self.open(serial, OP)?;
        let metadata = piv::metadata(&mut key, slot_id).map_err(|e| Self::classify(OP, e, None))?;
        let public = metadata.public.ok_or_else(|| WriteError::Failed {
            operation: OP,
            reason: format!(
                "PIV slot {slot} reports no public key — generate one before requesting a \
                 certificate for it"
            ),
        })?;
        let public_key_pem = encode_public_key(&public, OP)?;
        let algorithm = Self::describe_algorithm(metadata.algorithm, OP)?;

        // The PIN, every time. This slot is configured `PinPolicy::Always`, which is
        // the point of a signing slot: consent per signature.
        key.verify_pin(pin.expose().as_bytes())
            .map_err(|e| Self::classify(OP, e, key.get_pin_retries().ok()))?;

        super::csr::build(subject, &public_key_pem, san_email, &algorithm, |digest| {
            let algorithm_id = Self::parse_algorithm(&algorithm, OP)?;
            piv::sign_data(&mut key, digest, algorithm_id, slot_id)
                .map(|signature| signature.to_vec())
                .map_err(|e| Self::classify(OP, e, None))
        })
    }

    fn import_certificate(
        &mut self,
        serial: u32,
        slot: &str,
        certificate_pem: &str,
        pin: &Secret,
    ) -> Result<()> {
        const OP: &str = "piv.import_certificate";
        let slot_id = Self::parse_slot(slot, OP)?;

        let der = super::certificate::der_from_pem(certificate_pem).ok_or_else(|| {
            WriteError::Failed {
                operation: OP,
                reason: "the certificate is not a PEM `CERTIFICATE` document".into(),
            }
        })?;

        // `--verify` semantics, before the write rather than after
        // (`features/step-piv-signing-certificate.md` phase 5): the certificate
        // has to belong to the key that is in this slot. A mismatch imports
        // cleanly and then fails at every signature the holder tries to make,
        // which is the worst place to find out.
        {
            let mut key = self.open(serial, OP)?;
            let metadata =
                piv::metadata(&mut key, slot_id).map_err(|e| Self::classify(OP, e, None))?;
            let public = metadata.public.ok_or_else(|| WriteError::Failed {
                operation: OP,
                reason: format!(
                    "PIV slot {slot} holds no key, so nothing can be said about whether this \
                     certificate belongs to it — generate the key before importing"
                ),
            })?;
            let slot_key_der = {
                use x509_cert::der::Encode;
                public.to_der().map_err(|e| WriteError::Failed {
                    operation: OP,
                    reason: format!("the slot's public key could not be encoded: {e}"),
                })?
            };
            super::certificate::must_match_public_key(&der, &slot_key_der, OP)?;
        }

        let mut session = Self::authenticated(serial, pin, OP)?;
        session.import_certificate(u8::from(slot_id), &der, OP)
    }
}

fn encode_public_key(
    public: &x509_cert::spki::SubjectPublicKeyInfoOwned,
    operation: &'static str,
) -> Result<String> {
    use x509_cert::der::Encode;
    let der = public.to_der().map_err(|e| WriteError::Failed {
        operation,
        reason: format!("the generated public key could not be encoded: {e}"),
    })?;
    Ok(pem_encode("PUBLIC KEY", &der))
}

/// Minimal PEM, so a public key can be pasted into a CSR tool or a ticket.
fn pem_encode(label: &str, der: &[u8]) -> String {
    use std::fmt::Write as _;
    let body = base64(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for chunk in body.as_bytes().chunks(64) {
        let _ = writeln!(out, "{}", String::from_utf8_lossy(chunk));
    }
    let _ = writeln!(out, "-----END {label}-----");
    out
}

/// Base64, written here rather than pulled in: the only consumer is the function
/// above, and a dependency for forty lines is one to keep updated forever.
///
/// Decoding lives in [`super::certificate`], which is where the documents that
/// arrive from outside are read.
fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_published_encodings() {
        // RFC 4648's own examples, including every padding case. Checked against
        // fixed strings rather than round-tripped through a decoder in this file,
        // so a matching pair of bugs cannot pass.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xFF, 0xFF, 0xFF]), "////");
    }

    #[test]
    fn pem_wraps_at_sixty_four_columns_under_its_label() {
        let der: Vec<u8> = (0u8..=200).collect();
        let pem = pem_encode("PUBLIC KEY", &der);
        assert!(pem.starts_with("-----BEGIN PUBLIC KEY-----"));
        assert!(pem.trim_end().ends_with("-----END PUBLIC KEY-----"));
        assert!(pem.lines().all(|l| l.len() <= 64));

        // The document has to be readable by something that is not this code:
        // `x509-cert` is what the CSR builder parses a public key with.
        let body: String = pem
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(
            super::super::certificate::der_from_pem(&format!(
                "-----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----"
            )),
            Some(der),
            "the base64 body has to decode back to the same DER"
        );
    }

    #[test]
    fn a_slot_that_is_not_a_piv_slot_is_refused_by_name() {
        let err = NativePiv::parse_slot("zz", "test").unwrap_err();
        assert!(err.detail().contains("zz"), "{}", err.detail());
        assert!(NativePiv::parse_slot("9c", "test").is_ok());
    }

    #[test]
    fn an_unknown_algorithm_is_refused_rather_than_defaulted() {
        assert!(NativePiv::parse_algorithm("ECCP256", "test").is_ok());
        assert!(NativePiv::parse_algorithm("rsa2048", "test").is_ok());
        assert!(matches!(
            NativePiv::parse_algorithm("magic", "test"),
            Err(WriteError::Unsupported { .. })
        ));
    }
}
