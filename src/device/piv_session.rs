//! One PIV session, spoken to the card directly over PC/SC.
//!
//! This module exists for a protocol fact that is easy to miss: **management-key
//! authentication is a property of the card session, not of the process.** The
//! [`yubikey`] crate cannot authenticate to a firmware-5.7 management slot at all
//! — its `MgmKey` is a 24-byte 3DES type and its `authenticate` sends a 3DES
//! algorithm identifier, while 5.7 removed 3DES and defaults the slot to AES-192
//! (measured; see `features/step-piv-pin-puk-management-key.md`). So it is not
//! enough to authenticate correctly *somewhere* and then call the crate: whatever
//! authenticates must also be what issues the write, on the same connection.
//!
//! Hence a session rather than a function. [`Session`] selects the PIV applet,
//! reads the management slot's **actual** algorithm, does the mutual
//! authentication with AES, and then carries the two operations that need that
//! authentication:
//!
//! * `GENERATE ASYMMETRIC KEY PAIR` — the on-device signing key;
//! * `PUT DATA` of a certificate — the issued certificate coming back from the CA.
//!
//! ## What stays with the crate, deliberately
//!
//! `change_pin`, `change_puk`, `piv::metadata`, `piv::sign_data` and
//! `piv::attest` were all measured working through [`yubikey`] and stay there.
//! This is not a reimplementation of PIV; it is the exchange the crate gets
//! wrong, plus the two writes that are unreachable without it.
//!
//! ## Reading the algorithm instead of guessing it
//!
//! Guessing the cipher fails in a way indistinguishable from a wrong key, which
//! is the trap the crate fell into and an hour was spent proving a good key was
//! good. `GET METADATA` on slot `9B` answers it (firmware 5.3+); its absence
//! means a pre-5.3 key, which is 3DES-only, which is what the absence implies.
//!
//! ## Not hardware-verified
//!
//! The AES management-key exchange and `SET MANAGEMENT KEY` were verified against
//! a 5C NFC (5.7.4) on 2026-08-11. The `GENERATE` and `PUT DATA` paths added here
//! were written with **no key attached**, so they are built and unproven — stated
//! here because the module's whole subject is a failure that looked like success.

use zeroize::Zeroizing;

use super::write::{Result, WriteError};

/// PIV application id.
const PIV_AID: [u8; 5] = [0xA0, 0x00, 0x00, 0x03, 0x08];

/// The management key slot.
pub const SLOT_MANAGEMENT: u8 = 0x9B;

/// The PIN-protected data object, where a management key set by this tool lives
/// so that nothing has to be retained (custody model B).
const OBJECT_PRINTED: [u8; 3] = [0x5F, 0xC1, 0x09];

/// PIV algorithm identifiers for the management key slot.
const ALG_3DES: u8 = 0x03;
const ALG_AES128: u8 = 0x08;
const ALG_AES192: u8 = 0x0A;
const ALG_AES256: u8 = 0x0C;

/// The largest data field a short APDU carries. Anything longer is chained.
const MAX_APDU_DATA: usize = 255;

/// Which cipher the management key slot is using.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MgmAlgorithm {
    Tdes,
    Aes128,
    Aes192,
    Aes256,
}

impl MgmAlgorithm {
    pub fn id(&self) -> u8 {
        match self {
            MgmAlgorithm::Tdes => ALG_3DES,
            MgmAlgorithm::Aes128 => ALG_AES128,
            MgmAlgorithm::Aes192 => ALG_AES192,
            MgmAlgorithm::Aes256 => ALG_AES256,
        }
    }

    /// Key length in bytes.
    pub fn key_len(&self) -> usize {
        match self {
            MgmAlgorithm::Tdes | MgmAlgorithm::Aes192 => 24,
            MgmAlgorithm::Aes128 => 16,
            MgmAlgorithm::Aes256 => 32,
        }
    }

    /// Cipher block size in bytes. The witness and challenge are one block each,
    /// and getting this wrong makes every exchange fail in a way that looks like
    /// a wrong key.
    pub fn block_len(&self) -> usize {
        match self {
            MgmAlgorithm::Tdes => 8,
            _ => 16,
        }
    }

    pub fn from_id(id: u8) -> Option<Self> {
        Some(match id {
            ALG_3DES => MgmAlgorithm::Tdes,
            ALG_AES128 => MgmAlgorithm::Aes128,
            ALG_AES192 => MgmAlgorithm::Aes192,
            ALG_AES256 => MgmAlgorithm::Aes256,
            _ => return None,
        })
    }

    pub fn label(&self) -> &'static str {
        match self {
            MgmAlgorithm::Tdes => "3DES",
            MgmAlgorithm::Aes128 => "AES128",
            MgmAlgorithm::Aes192 => "AES192",
            MgmAlgorithm::Aes256 => "AES256",
        }
    }
}

/// A key algorithm a slot can be told to generate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAlgorithm {
    EccP256,
    EccP384,
    Rsa1024,
    Rsa2048,
}

impl KeyAlgorithm {
    /// The names the templates use, in the forms they appear in.
    pub fn from_name(name: &str, operation: &'static str) -> Result<Self> {
        match name.to_ascii_uppercase().replace(['-', '_'], "").as_str() {
            "ECCP256" | "ECDSAP256" | "P256" => Ok(KeyAlgorithm::EccP256),
            "ECCP384" | "ECDSAP384" | "P384" => Ok(KeyAlgorithm::EccP384),
            "RSA2048" => Ok(KeyAlgorithm::Rsa2048),
            "RSA1024" => Ok(KeyAlgorithm::Rsa1024),
            other => Err(WriteError::Unsupported {
                operation,
                reason: format!("`{other}` is not an algorithm this transport can generate"),
            }),
        }
    }

    pub fn id(&self) -> u8 {
        match self {
            KeyAlgorithm::Rsa1024 => 0x06,
            KeyAlgorithm::Rsa2048 => 0x07,
            KeyAlgorithm::EccP256 => 0x11,
            KeyAlgorithm::EccP384 => 0x14,
        }
    }

    fn is_ec(&self) -> bool {
        matches!(self, KeyAlgorithm::EccP256 | KeyAlgorithm::EccP384)
    }
}

/// How often the PIN is required to use a generated key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinPolicy {
    Never,
    Once,
    Always,
}

/// How often the key's owner has to touch the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchPolicy {
    Never,
    Always,
    Cached,
}

impl PinPolicy {
    fn id(&self) -> u8 {
        match self {
            PinPolicy::Never => 0x01,
            PinPolicy::Once => 0x02,
            PinPolicy::Always => 0x03,
        }
    }
}

impl TouchPolicy {
    fn id(&self) -> u8 {
        match self {
            TouchPolicy::Never => 0x01,
            TouchPolicy::Always => 0x02,
            TouchPolicy::Cached => 0x03,
        }
    }
}

/// One card session, held for as long as the caller needs the authentication it
/// carries — and no longer.
///
/// Opened and dropped inside a single operation rather than kept: the [`yubikey`]
/// crate holds its own connection for PIN, PUK and attestation work, and two
/// exclusive handles on one reader is how a session gets refused.
pub struct Session {
    card: pcsc::Card,
}

impl Session {
    /// Connect to the key with this serial and select the PIV applet.
    pub fn open(serial: u32, operation: &'static str) -> Result<Self> {
        let ctx = pcsc::Context::establish(pcsc::Scope::User).map_err(|e| WriteError::Failed {
            operation,
            reason: format!("no PC/SC service: {e}"),
        })?;

        let mut names = vec![0u8; ctx.list_readers_len().unwrap_or(2048)];
        let readers = ctx
            .list_readers(&mut names)
            .map_err(|e| WriteError::Failed {
                operation,
                reason: format!("no readers: {e}"),
            })?;

        // A YubiKey's reader name carries the model, not the serial, so every
        // Yubico reader is tried and the applet's own serial is the check. That
        // is the same thing `device::native` does for reads.
        for reader in readers {
            let name = reader.to_string_lossy();
            if !name.to_lowercase().contains("yubikey") {
                continue;
            }
            let Ok(card) = ctx.connect(reader, pcsc::ShareMode::Shared, pcsc::Protocols::ANY)
            else {
                continue;
            };
            let mut candidate = Self { card };
            if candidate.select_piv(operation).is_ok() && candidate.serial(operation) == Ok(serial)
            {
                return Ok(candidate);
            }
        }
        Err(WriteError::NotAttached(serial))
    }

    /// Send one APDU, following `61xx` continuations, and return the data with
    /// the final two status bytes.
    ///
    /// The continuation matters as soon as a response is longer than 256 bytes,
    /// which is every RSA public key and every certificate read — without it the
    /// answer arrives truncated and parses as garbage rather than as an error.
    fn transmit(&mut self, apdu: &[u8], operation: &'static str) -> Result<(Vec<u8>, u16)> {
        let mut collected = Vec::new();
        let mut request = apdu.to_vec();

        loop {
            let mut buf = vec![0u8; 4096];
            let response =
                self.card
                    .transmit(&request, &mut buf)
                    .map_err(|e| WriteError::Failed {
                        operation,
                        reason: format!("the card did not answer: {e}"),
                    })?;
            if response.len() < 2 {
                return Err(WriteError::Failed {
                    operation,
                    reason: "truncated response from the card".into(),
                });
            }
            let split = response.len() - 2;
            let sw = u16::from(response[split]) << 8 | u16::from(response[split + 1]);
            collected.extend_from_slice(&response[..split]);

            // `61 xx`: more data waiting, xx bytes of it (0 meaning "unknown, ask
            // for the maximum").
            if sw & 0xFF00 == 0x6100 {
                request = vec![0x00, 0xC0, 0x00, 0x00, (sw & 0x00FF) as u8];
                continue;
            }
            return Ok((collected, sw));
        }
    }

    /// Send a command whose data field may exceed one APDU, using command
    /// chaining (`CLA` bit 0x10 on every block but the last).
    ///
    /// A certificate is the reason this exists: even a small one is past the
    /// 255-byte limit of a short APDU.
    fn transmit_chained(
        &mut self,
        header: [u8; 4],
        data: &[u8],
        operation: &'static str,
    ) -> Result<(Vec<u8>, u16)> {
        let blocks = chain(data, MAX_APDU_DATA);
        let last = blocks.len().saturating_sub(1);
        let mut result = (Vec::new(), 0x9000);

        for (index, block) in blocks.iter().enumerate() {
            let mut apdu = vec![
                if index == last {
                    header[0]
                } else {
                    header[0] | 0x10
                },
                header[1],
                header[2],
                header[3],
                block.len() as u8,
            ];
            apdu.extend_from_slice(block);
            let (response, sw) = self.transmit(&apdu, operation)?;
            // A block the card refuses stops the sequence: continuing would send
            // the tail of a structure whose head was rejected.
            if index != last && sw != 0x9000 {
                return Ok((response, sw));
            }
            result = (response, sw);
        }
        Ok(result)
    }

    fn select_piv(&mut self, operation: &'static str) -> Result<()> {
        let mut apdu = vec![0x00, 0xA4, 0x04, 0x00, PIV_AID.len() as u8];
        apdu.extend_from_slice(&PIV_AID);
        let (_, sw) = self.transmit(&apdu, operation)?;
        expect_ok(sw, operation, "selecting the PIV application")
    }

    fn serial(&mut self, operation: &'static str) -> Result<u32> {
        // YubiKey-proprietary GET SERIAL on the PIV applet.
        let (data, sw) = self.transmit(&[0x00, 0xF8, 0x00, 0x00], operation)?;
        if sw != 0x9000 || data.len() < 4 {
            return Err(WriteError::Failed {
                operation,
                reason: "the card did not report a serial".into(),
            });
        }
        Ok(u32::from_be_bytes([data[0], data[1], data[2], data[3]]))
    }

    /// Raw `GET METADATA` for a slot, or `None` when the firmware has no such
    /// command (below 5.3).
    fn metadata(&mut self, slot: u8, operation: &'static str) -> Result<Option<Vec<u8>>> {
        let (data, sw) = self.transmit(&[0x00, 0xF7, 0x00, slot], operation)?;
        Ok(if sw == 0x9000 { Some(data) } else { None })
    }

    /// Which cipher the management key slot currently uses.
    pub fn management_algorithm(&mut self, operation: &'static str) -> Result<MgmAlgorithm> {
        let Some(data) = self.metadata(SLOT_MANAGEMENT, operation)? else {
            // Pre-5.3 firmware cannot say. Those keys are 3DES-only, which is
            // exactly what the metadata's absence implies.
            return Ok(MgmAlgorithm::Tdes);
        };
        // Tag 0x01 carries the algorithm.
        match find_tlv(&data, 0x01).and_then(|v| v.first().copied()) {
            Some(id) => MgmAlgorithm::from_id(id).ok_or(WriteError::Unsupported {
                operation,
                reason: format!("the management key uses algorithm 0x{id:02x}"),
            }),
            None => Ok(MgmAlgorithm::Tdes),
        }
    }

    /// Is the management key still the factory default?
    ///
    /// `None` means the card did not say — pre-5.3 firmware, or a metadata
    /// response without the flag. Unknown is reported as unknown rather than as
    /// `false`, because the caller's two readings of it are opposite.
    ///
    /// This is read here rather than through the crate for a measured reason: the
    /// crate's `piv::metadata` on slot `9B` kept reporting *default* after this
    /// module had successfully changed the key, which disagreed with `ykman`. The
    /// write was right and the read was wrong.
    pub fn management_key_is_default(&mut self, operation: &'static str) -> Option<bool> {
        let data = self.metadata(SLOT_MANAGEMENT, operation).ok()??;
        // Tag 0x05 is the "is default value" flag: one byte, 1 for default.
        find_tlv(&data, 0x05)
            .and_then(|v| v.first().copied())
            .map(|flag| flag == 0x01)
    }

    /// `VERIFY` the PIN on this session.
    ///
    /// Required before the PIN-protected management key can be read, and before a
    /// key generated with `PinPolicy::Always` will sign.
    pub fn verify_pin(&mut self, pin: &str, operation: &'static str) -> Result<()> {
        let padded = pad_pin(pin, operation)?;
        let mut apdu = vec![0x00, 0x20, 0x00, 0x80, padded.len() as u8];
        apdu.extend_from_slice(&padded);

        let (_, sw) = self.transmit(&apdu, operation)?;
        match sw {
            0x9000 => Ok(()),
            // `63 Cx` — wrong PIN, x attempts left. The count is the one thing
            // worth translating precisely: it is what tells the operator whether
            // to stop.
            other if other & 0xFFF0 == 0x63C0 => Err(WriteError::WrongSecret {
                applet: "PIV",
                retries_left: (other & 0x000F) as u8,
            }),
            0x6983 => Err(WriteError::Locked { applet: "PIV" }),
            other => Err(WriteError::Failed {
                operation,
                reason: format!("the PIN was not accepted: card status 0x{other:04x}"),
            }),
        }
    }

    /// Authenticate with the management key the caller holds.
    ///
    /// Mutual authentication, as the card expects (SP 800-73-4, and what
    /// `yubico-piv-tool` does):
    ///
    /// 1. Ask for a **witness** — a block the card encrypted with the key it holds.
    /// 2. Decrypt it. Getting this right is what proves *we* know the key.
    /// 3. Send it back with a **challenge** of our own.
    /// 4. Check the card's answer. That is what proves the *card* knows the key,
    ///    and it is why the extra round trip is worth taking: a card that cannot
    ///    prove itself is not one to write a new management key into.
    pub fn authenticate(
        &mut self,
        algorithm: MgmAlgorithm,
        key: &[u8],
        operation: &'static str,
    ) -> Result<()> {
        let block = algorithm.block_len();
        if key.len() != algorithm.key_len() {
            return Err(WriteError::Failed {
                operation,
                reason: format!(
                    "the management key is {} bytes and this slot's {} needs {}",
                    key.len(),
                    algorithm.label(),
                    algorithm.key_len()
                ),
            });
        }

        // 1. Ask for a witness: `7C 02 80 00`.
        let request = [0x7C, 0x02, 0x80, 0x00];
        let mut apdu = vec![
            0x00,
            0x87,
            algorithm.id(),
            SLOT_MANAGEMENT,
            request.len() as u8,
        ];
        apdu.extend_from_slice(&request);
        apdu.push(0x00);
        let (data, sw) = self.transmit(&apdu, operation)?;
        expect_ok(sw, operation, "requesting the authentication witness")?;

        let witness = inner_tlv(&data, 0x7C, 0x80).ok_or(WriteError::Failed {
            operation,
            reason: "the card did not return a witness".into(),
        })?;
        if witness.len() != block {
            return Err(WriteError::Failed {
                operation,
                reason: format!(
                    "the witness is {} bytes and {} blocks are {block}",
                    witness.len(),
                    algorithm.label()
                ),
            });
        }

        // 2. Decrypting it correctly is what proves we hold the key.
        let decrypted = decrypt_block(algorithm, key, witness)?;

        // 3. Send it back with a challenge of our own.
        let mut challenge = vec![0u8; block];
        getrandom::fill(&mut challenge).map_err(|e| WriteError::Failed {
            operation,
            reason: format!("no randomness for the authentication challenge: {e}"),
        })?;

        let mut body = Vec::new();
        body.push(0x80);
        body.push(block as u8);
        body.extend_from_slice(&decrypted);
        body.push(0x81);
        body.push(block as u8);
        body.extend_from_slice(&challenge);

        let mut wrapped = vec![0x7C, body.len() as u8];
        wrapped.extend_from_slice(&body);

        let mut apdu = vec![
            0x00,
            0x87,
            algorithm.id(),
            SLOT_MANAGEMENT,
            wrapped.len() as u8,
        ];
        apdu.extend_from_slice(&wrapped);
        apdu.push(0x00);
        let (data, sw) = self.transmit(&apdu, operation)?;
        // A wrong management key surfaces here, and this is the one status worth
        // translating precisely: it is the difference between "you have the wrong
        // key" and "something is broken".
        if sw == 0x6982 || sw == 0x6983 {
            return Err(WriteError::WrongSecret {
                applet: "PIV",
                retries_left: 0,
            });
        }
        expect_ok(sw, operation, "answering the authentication witness")?;

        // 4. Check the card's answer to our challenge.
        let response = inner_tlv(&data, 0x7C, 0x82).ok_or(WriteError::Failed {
            operation,
            reason: "the card did not answer the challenge".into(),
        })?;
        let expected = encrypt_block(algorithm, key, &challenge)?;
        if response != expected.as_slice() {
            return Err(WriteError::Failed {
                operation,
                reason: "the card failed to prove it holds the management key — refusing to \
                         continue against it"
                    .into(),
            });
        }
        Ok(())
    }

    /// Authenticate the way a key configured by this tool has to be authenticated:
    /// with the management key stored **on the card** under the PIN.
    ///
    /// This is the replacement for the crate's `MgmKey::get_protected` +
    /// `authenticate` pair, which reads the same object and then speaks 3DES to
    /// it. Custody model B is what makes the object the normal case: the key is
    /// random, PIN-protected onto the card, never handed over and never retained,
    /// so the PIN is the only thing anybody holds.
    ///
    /// Falls back to `default_key` when the object is absent, which is what a
    /// factory-fresh applet looks like.
    pub fn authenticate_management(
        &mut self,
        pin: &str,
        default_key: &[u8],
        operation: &'static str,
    ) -> Result<MgmAlgorithm> {
        let algorithm = self.management_algorithm(operation)?;
        self.verify_pin(pin, operation)?;

        // `Zeroizing`, because this is the one credential that gives full control
        // of the applet and it must not linger in freed memory.
        let stored = self.read_protected_management_key(operation)?;
        let key: Zeroizing<Vec<u8>> = match stored {
            Some(key) => key,
            None => Zeroizing::new(default_key.to_vec()),
        };

        self.authenticate(algorithm, &key, operation)?;
        Ok(algorithm)
    }

    /// Read the management key out of the PIN-protected data object.
    ///
    /// `None` when the object is not there — an applet nobody has protected a key
    /// onto, which is a state and not an error.
    fn read_protected_management_key(
        &mut self,
        operation: &'static str,
    ) -> Result<Option<Zeroizing<Vec<u8>>>> {
        let mut apdu = vec![0x00, 0xCB, 0x3F, 0xFF, 0x05, 0x5C, 0x03];
        apdu.extend_from_slice(&OBJECT_PRINTED);
        apdu.push(0x00);

        let (data, sw) = self.transmit(&apdu, operation)?;
        if sw != 0x9000 {
            return Ok(None);
        }
        // `53 { 88 { 89 <key> } }` — the object, the protected-data wrapper, and
        // the management key itself.
        let key = find_tlv(&data, 0x53)
            .and_then(|content| find_tlv(content, 0x88).map(|inner| (content, inner)))
            .and_then(|(_, inner)| find_tlv(inner, 0x89))
            .map(|key| Zeroizing::new(key.to_vec()));
        Ok(key)
    }

    /// `SET MANAGEMENT KEY`, tagged with the algorithm the new key is for.
    ///
    /// Authenticate first: the card refuses this otherwise, and that refusal is
    /// the protection.
    pub fn set_management_key(
        &mut self,
        algorithm: MgmAlgorithm,
        new: &[u8],
        require_touch: bool,
        operation: &'static str,
    ) -> Result<()> {
        if new.len() != algorithm.key_len() {
            return Err(WriteError::Failed {
                operation,
                reason: format!(
                    "the new management key is {} bytes and this slot's {} needs {}",
                    new.len(),
                    algorithm.label(),
                    algorithm.key_len()
                ),
            });
        }

        let mut data = vec![algorithm.id(), SLOT_MANAGEMENT, new.len() as u8];
        data.extend_from_slice(new);

        let mut apdu = vec![
            0x00,
            0xFF,
            0xFF,
            if require_touch { 0xFE } else { 0xFF },
            data.len() as u8,
        ];
        apdu.extend_from_slice(&data);

        let (_, sw) = self.transmit(&apdu, operation)?;
        expect_ok(sw, operation, "writing the new management key")
    }

    /// Store a management key in the PIN-protected data object (`0x5FC109`).
    ///
    /// This is what `--protect` means: the key lives on the card, reachable only
    /// after the PIN, so under custody model B there is nothing to hand over and
    /// nothing to retain.
    pub fn store_management_key_under_pin(
        &mut self,
        key: &[u8],
        operation: &'static str,
    ) -> Result<()> {
        let mut inner = vec![0x89, key.len() as u8];
        inner.extend_from_slice(key);
        let mut content = vec![0x88, inner.len() as u8];
        content.extend_from_slice(&inner);

        let mut data = vec![0x5C, 0x03];
        data.extend_from_slice(&OBJECT_PRINTED);
        data.push(0x53);
        push_len(&mut data, content.len());
        data.extend_from_slice(&content);

        let (_, sw) = self.transmit_chained([0x00, 0xDB, 0x3F, 0xFF], &data, operation)?;
        expect_ok(sw, operation, "storing the management key under the PIN")
    }

    /// `GENERATE ASYMMETRIC KEY PAIR` in a slot, returning the public key as a
    /// DER `SubjectPublicKeyInfo`.
    ///
    /// Needs management-key authentication on this same session — which is the
    /// reason this function is here rather than left to the crate.
    pub fn generate_key(
        &mut self,
        slot: u8,
        algorithm: KeyAlgorithm,
        pin_policy: PinPolicy,
        touch_policy: TouchPolicy,
        operation: &'static str,
    ) -> Result<Vec<u8>> {
        let body = [
            0x80,
            0x01,
            algorithm.id(),
            0xAA,
            0x01,
            pin_policy.id(),
            0xAB,
            0x01,
            touch_policy.id(),
        ];
        let mut data = vec![0xAC, body.len() as u8];
        data.extend_from_slice(&body);

        let mut apdu = vec![0x00, 0x47, 0x00, slot, data.len() as u8];
        apdu.extend_from_slice(&data);
        apdu.push(0x00);

        let (response, sw) = self.transmit(&apdu, operation)?;
        expect_ok(sw, operation, "generating the key on the card")?;
        spki_from_generated(algorithm, &response, operation)
    }

    /// `PUT DATA` an issued certificate into the object belonging to a slot.
    ///
    /// Also needs management-key authentication on this session.
    pub fn import_certificate(
        &mut self,
        slot: u8,
        certificate_der: &[u8],
        operation: &'static str,
    ) -> Result<()> {
        let object = certificate_object_id(slot, operation)?;

        // `70 <cert>` the certificate, `71 01 00` uncompressed, `FE 00` the empty
        // LRC the data model asks for.
        let mut content = vec![0x70];
        push_len(&mut content, certificate_der.len());
        content.extend_from_slice(certificate_der);
        content.extend_from_slice(&[0x71, 0x01, 0x00, 0xFE, 0x00]);

        let mut data = vec![0x5C, 0x03];
        data.extend_from_slice(&object);
        data.push(0x53);
        push_len(&mut data, content.len());
        data.extend_from_slice(&content);

        let (_, sw) = self.transmit_chained([0x00, 0xDB, 0x3F, 0xFF], &data, operation)?;
        expect_ok(sw, operation, "writing the certificate to the slot")
    }
}

/// Pad a PIN to the 8 bytes the applet expects, with `0xFF`.
///
/// A PIN longer than 8 bytes is refused rather than truncated: truncating would
/// set a PIN the operator did not type, and the holder would find out at the
/// first signature.
fn pad_pin(pin: &str, operation: &'static str) -> Result<Zeroizing<Vec<u8>>> {
    let bytes = pin.as_bytes();
    if bytes.is_empty() || bytes.len() > 8 {
        return Err(WriteError::Failed {
            operation,
            reason: format!(
                "a PIV PIN is 1 to 8 bytes and this one is {} — nothing was sent to the card",
                bytes.len()
            ),
        });
    }
    let mut padded = Zeroizing::new(vec![0xFFu8; 8]);
    padded[..bytes.len()].copy_from_slice(bytes);
    Ok(padded)
}

/// Which data object holds the certificate for a slot (SP 800-73-4 part 1).
pub fn certificate_object_id(slot: u8, operation: &'static str) -> Result<[u8; 3]> {
    Ok(match slot {
        0x9A => [0x5F, 0xC1, 0x05],
        0x9C => [0x5F, 0xC1, 0x0A],
        0x9D => [0x5F, 0xC1, 0x0B],
        0x9E => [0x5F, 0xC1, 0x01],
        other => {
            return Err(WriteError::Unsupported {
                operation,
                reason: format!(
                    "slot {other:02x} has no certificate object — a signing certificate goes in \
                     9a, 9c, 9d or 9e"
                ),
            });
        }
    })
}

fn expect_ok(sw: u16, operation: &'static str, what: &str) -> Result<()> {
    match sw {
        0x9000 => Ok(()),
        0x6982 | 0x6983 => Err(WriteError::WrongSecret {
            applet: "PIV",
            retries_left: 0,
        }),
        0x6A80 => Err(WriteError::Failed {
            operation,
            reason: format!("{what}: the card rejected the data"),
        }),
        0x6D00 | 0x6A81 => Err(WriteError::Unsupported {
            operation,
            reason: format!("{what}: this key does not support it"),
        }),
        other => Err(WriteError::Failed {
            operation,
            reason: format!("{what}: card status 0x{other:04x}"),
        }),
    }
}

/// Split a data field into the blocks command chaining sends.
fn chain(data: &[u8], max: usize) -> Vec<&[u8]> {
    if data.is_empty() {
        return vec![&[]];
    }
    data.chunks(max).collect()
}

/// Append a BER length: short form, or `81`/`82` with the count.
fn push_len(out: &mut Vec<u8>, len: usize) {
    if len < 0x80 {
        out.push(len as u8);
    } else if len <= 0xFF {
        out.push(0x81);
        out.push(len as u8);
    } else {
        out.push(0x82);
        out.push((len >> 8) as u8);
        out.push(len as u8);
    }
}

/// Walk a BER-TLV sequence one level deep, handling multi-byte tags and long
/// lengths.
///
/// Both forms are needed here and neither is exotic: the generate response is
/// tagged `7F 49` (two bytes) and a certificate object carries a length in two
/// bytes. A parser that assumed one byte of each read the witness correctly and
/// then silently mis-read everything larger.
fn tlvs(data: &[u8]) -> Vec<(u32, &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;

    while i < data.len() {
        let mut tag = u32::from(data[i]);
        // A tag whose low five bits are all set continues into the next bytes,
        // each with the high bit meaning "one more".
        if data[i] & 0x1F == 0x1F {
            loop {
                i += 1;
                if i >= data.len() {
                    return out;
                }
                tag = (tag << 8) | u32::from(data[i]);
                if data[i] & 0x80 == 0 {
                    break;
                }
            }
        }
        i += 1;
        if i >= data.len() {
            return out;
        }

        let first = data[i];
        i += 1;
        let len = if first < 0x80 {
            first as usize
        } else {
            let count = (first & 0x7F) as usize;
            if count == 0 || count > 4 || i + count > data.len() {
                return out;
            }
            let mut len = 0usize;
            for _ in 0..count {
                len = (len << 8) | data[i] as usize;
                i += 1;
            }
            len
        };

        if i + len > data.len() {
            return out;
        }
        out.push((tag, &data[i..i + len]));
        i += len;
    }
    out
}

fn find_tlv(data: &[u8], tag: u32) -> Option<&[u8]> {
    tlvs(data)
        .into_iter()
        .find(|(t, _)| *t == tag)
        .map(|(_, v)| v)
}

/// Find a tag nested one level inside another.
fn inner_tlv(data: &[u8], outer: u32, inner: u32) -> Option<&[u8]> {
    find_tlv(data, outer).and_then(|value| find_tlv(value, inner))
}

/// Turn a `GENERATE` response into a DER `SubjectPublicKeyInfo`.
///
/// The card answers with its own encoding — `7F 49 { 86 <point> }` for an elliptic
/// curve, `7F 49 { 81 <modulus> 82 <exponent> }` for RSA — and everything
/// downstream (the CSR builder, the certificate check, the evidence record) wants
/// a `SubjectPublicKeyInfo`. Converting here keeps that translation in one place,
/// next to the response it belongs to.
fn spki_from_generated(
    algorithm: KeyAlgorithm,
    response: &[u8],
    operation: &'static str,
) -> Result<Vec<u8>> {
    use x509_cert::der::asn1::BitString;
    use x509_cert::der::{Any, Decode, Encode};
    use x509_cert::spki::{AlgorithmIdentifierOwned, ObjectIdentifier, SubjectPublicKeyInfoOwned};

    const ID_EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
    const PRIME256V1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
    const SECP384R1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.34");
    const RSA_ENCRYPTION: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");

    let body = find_tlv(response, 0x7F49).ok_or_else(|| WriteError::Failed {
        operation,
        reason: "the card's response holds no generated public key".into(),
    })?;

    let (oid, parameters, key_bytes) = if algorithm.is_ec() {
        let point = find_tlv(body, 0x86).ok_or_else(|| WriteError::Failed {
            operation,
            reason: "the card returned no public point for the generated key".into(),
        })?;
        let curve = match algorithm {
            KeyAlgorithm::EccP384 => SECP384R1,
            _ => PRIME256V1,
        };
        let der = curve.to_der().map_err(|e| encoding_failed(operation, e))?;
        let parameters = Any::from_der(&der).map_err(|e| encoding_failed(operation, e))?;
        (ID_EC_PUBLIC_KEY, Some(parameters), point.to_vec())
    } else {
        let modulus = find_tlv(body, 0x81).ok_or_else(|| WriteError::Failed {
            operation,
            reason: "the card returned no modulus for the generated key".into(),
        })?;
        let exponent = find_tlv(body, 0x82).ok_or_else(|| WriteError::Failed {
            operation,
            reason: "the card returned no exponent for the generated key".into(),
        })?;
        let null = Any::from_der(&[0x05, 0x00]).map_err(|e| encoding_failed(operation, e))?;
        (
            RSA_ENCRYPTION,
            Some(null),
            rsa_public_key_der(modulus, exponent),
        )
    };

    let spki = SubjectPublicKeyInfoOwned {
        algorithm: AlgorithmIdentifierOwned { oid, parameters },
        subject_public_key: BitString::from_bytes(&key_bytes)
            .map_err(|e| encoding_failed(operation, e))?,
    };
    spki.to_der().map_err(|e| encoding_failed(operation, e))
}

fn encoding_failed(operation: &'static str, error: impl std::fmt::Display) -> WriteError {
    WriteError::Failed {
        operation,
        reason: format!("the generated public key could not be encoded: {error}"),
    }
}

/// `RSAPublicKey ::= SEQUENCE { modulus INTEGER, publicExponent INTEGER }`.
fn rsa_public_key_der(modulus: &[u8], exponent: &[u8]) -> Vec<u8> {
    let mut body = der_integer(modulus);
    body.extend(der_integer(exponent));
    let mut out = vec![0x30];
    push_len(&mut out, body.len());
    out.extend(body);
    out
}

/// A DER INTEGER from a big-endian magnitude: leading zeros dropped, one added
/// back when the top bit would otherwise read as a negative number.
fn der_integer(magnitude: &[u8]) -> Vec<u8> {
    let trimmed = magnitude
        .iter()
        .position(|b| *b != 0)
        .map(|start| &magnitude[start..])
        .unwrap_or(&[]);

    let mut body = Vec::with_capacity(trimmed.len() + 1);
    if trimmed.is_empty() {
        body.push(0x00);
    } else {
        if trimmed[0] & 0x80 != 0 {
            body.push(0x00);
        }
        body.extend_from_slice(trimmed);
    }

    let mut out = vec![0x02];
    push_len(&mut out, body.len());
    out.extend(body);
    out
}

fn encrypt_block(algorithm: MgmAlgorithm, key: &[u8], block: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{BlockCipherEncrypt, KeyInit};
    let mut out = block.to_vec();
    match algorithm {
        MgmAlgorithm::Aes192 => {
            let cipher = aes::Aes192::new_from_slice(key).map_err(|_| bad_key())?;
            cipher.encrypt_block(as_block(&mut out)?);
        }
        MgmAlgorithm::Aes128 => {
            let cipher = aes::Aes128::new_from_slice(key).map_err(|_| bad_key())?;
            cipher.encrypt_block(as_block(&mut out)?);
        }
        MgmAlgorithm::Aes256 => {
            let cipher = aes::Aes256::new_from_slice(key).map_err(|_| bad_key())?;
            cipher.encrypt_block(as_block(&mut out)?);
        }
        MgmAlgorithm::Tdes => return Err(tdes_unsupported()),
    }
    Ok(out)
}

fn decrypt_block(algorithm: MgmAlgorithm, key: &[u8], block: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{BlockCipherDecrypt, KeyInit};
    let mut out = block.to_vec();
    match algorithm {
        MgmAlgorithm::Aes192 => {
            let cipher = aes::Aes192::new_from_slice(key).map_err(|_| bad_key())?;
            cipher.decrypt_block(as_block(&mut out)?);
        }
        MgmAlgorithm::Aes128 => {
            let cipher = aes::Aes128::new_from_slice(key).map_err(|_| bad_key())?;
            cipher.decrypt_block(as_block(&mut out)?);
        }
        MgmAlgorithm::Aes256 => {
            let cipher = aes::Aes256::new_from_slice(key).map_err(|_| bad_key())?;
            cipher.decrypt_block(as_block(&mut out)?);
        }
        MgmAlgorithm::Tdes => return Err(tdes_unsupported()),
    }
    Ok(out)
}

/// Borrow a 16-byte slice as the cipher's block type.
///
/// Fallible rather than the panicking `from_mut_slice`: a short block is what a
/// truncated card response looks like, and that is an error to report, not a
/// crash in front of a half-configured key.
fn as_block(bytes: &mut [u8]) -> Result<&mut aes::cipher::Array<u8, aes::cipher::consts::U16>> {
    <&mut aes::cipher::Array<u8, aes::cipher::consts::U16>>::try_from(bytes)
        .map_err(|_| bad_block())
}

fn bad_key() -> WriteError {
    WriteError::Failed {
        operation: "piv.authenticate",
        reason: "the management key is the wrong length for the slot's algorithm".into(),
    }
}

fn bad_block() -> WriteError {
    WriteError::Failed {
        operation: "piv.authenticate",
        reason: "the card returned a block of the wrong size".into(),
    }
}

fn tdes_unsupported() -> WriteError {
    WriteError::Unsupported {
        operation: "piv.authenticate",
        reason: "this key's management slot still uses 3DES; that path stays with the `yubikey` \
                 crate, which handles it"
            .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn algorithms_agree_on_their_key_and_block_sizes() {
        // Getting a block size wrong makes every exchange fail in a way that is
        // indistinguishable from a wrong key, which is the trap this module was
        // written to escape.
        assert_eq!(MgmAlgorithm::Aes192.key_len(), 24);
        assert_eq!(MgmAlgorithm::Aes192.block_len(), 16);
        assert_eq!(MgmAlgorithm::Tdes.key_len(), 24);
        assert_eq!(
            MgmAlgorithm::Tdes.block_len(),
            8,
            "3DES blocks are half an AES block — the same key length, a different exchange"
        );
        assert_eq!(MgmAlgorithm::Aes256.key_len(), 32);
    }

    #[test]
    fn algorithm_ids_round_trip() {
        for algorithm in [
            MgmAlgorithm::Tdes,
            MgmAlgorithm::Aes128,
            MgmAlgorithm::Aes192,
            MgmAlgorithm::Aes256,
        ] {
            assert_eq!(MgmAlgorithm::from_id(algorithm.id()), Some(algorithm));
        }
        assert_eq!(MgmAlgorithm::from_id(0x99), None);
    }

    #[test]
    fn aes_round_trips_a_block() {
        let key = [7u8; 24];
        let block = [3u8; 16];
        let encrypted = encrypt_block(MgmAlgorithm::Aes192, &key, &block).unwrap();
        assert_ne!(encrypted, block, "encryption must change the block");
        assert_eq!(
            decrypt_block(MgmAlgorithm::Aes192, &key, &encrypted).unwrap(),
            block
        );
    }

    #[test]
    fn a_wrong_length_key_is_refused_rather_than_padded() {
        assert!(encrypt_block(MgmAlgorithm::Aes192, &[0u8; 16], &[0u8; 16]).is_err());
    }

    #[test]
    fn tdes_is_declined_and_says_where_it_is_handled() {
        let err = encrypt_block(MgmAlgorithm::Tdes, &[0u8; 24], &[0u8; 8]).unwrap_err();
        assert!(err.detail().contains("yubikey"), "{}", err.detail());
    }

    #[test]
    fn tlv_parsing_finds_a_nested_tag() {
        // The witness as the card actually frames it: 0x80 inside 0x7C.
        let response = [
            0x7C, 0x12, 0x80, 0x10, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
        ];
        let witness = inner_tlv(&response, 0x7C, 0x80).expect("the witness is there");
        assert_eq!(witness.len(), 16);
        assert_eq!(witness[0], 1);
        assert_eq!(inner_tlv(&response, 0x7C, 0x82), None);
    }

    #[test]
    fn a_truncated_tlv_does_not_panic() {
        assert!(tlvs(&[0x7C, 0x10, 0x01]).is_empty());
        assert!(inner_tlv(&[0x7C], 0x7C, 0x80).is_none());
        assert!(tlvs(&[0x7F]).is_empty());
        assert!(tlvs(&[0x7F, 0x49]).is_empty());
        assert!(tlvs(&[0x53, 0x82, 0x01]).is_empty());
    }

    #[test]
    fn a_two_byte_tag_is_read_as_one_tag() {
        // `7F 49` is how every generate response is framed. A parser that read
        // one-byte tags saw `7F` with a nonsense length and returned nothing.
        let response = [0x7F, 0x49, 0x04, 0x86, 0x02, 0xAA, 0xBB];
        assert_eq!(
            find_tlv(&response, 0x7F49).map(|v| v.to_vec()),
            Some(vec![0x86, 0x02, 0xAA, 0xBB])
        );
        assert_eq!(
            inner_tlv(&response, 0x7F49, 0x86).map(|v| v.to_vec()),
            Some(vec![0xAA, 0xBB])
        );
    }

    #[test]
    fn a_long_length_is_read_as_a_length() {
        // A certificate object is past 255 bytes, so `82 xx xx` is the normal
        // case rather than the exotic one.
        let mut data = vec![0x53, 0x82, 0x01, 0x00];
        data.extend(std::iter::repeat_n(0xEE, 256));
        assert_eq!(find_tlv(&data, 0x53).map(|v| v.len()), Some(256));

        let mut short = vec![0x70, 0x81, 0x80];
        short.extend(std::iter::repeat_n(0x11, 128));
        assert_eq!(find_tlv(&short, 0x70).map(|v| v.len()), Some(128));
    }

    #[test]
    fn lengths_round_trip_through_the_parser() {
        for len in [0usize, 1, 127, 128, 255, 256, 4096] {
            let mut encoded = vec![0x70];
            push_len(&mut encoded, len);
            encoded.extend(std::iter::repeat_n(0x5A, len));
            assert_eq!(
                find_tlv(&encoded, 0x70).map(|v| v.len()),
                Some(len),
                "length {len}"
            );
        }
    }

    #[test]
    fn chaining_splits_only_what_does_not_fit() {
        assert_eq!(chain(&[1, 2, 3], 255).len(), 1);
        assert_eq!(
            chain(&[], 255).len(),
            1,
            "an empty data field is still one APDU"
        );
        let long: Vec<u8> = vec![0; 600];
        let blocks = chain(&long, 255);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].len(), 255);
        assert_eq!(blocks[2].len(), 90);
        assert_eq!(
            blocks.iter().map(|b| b.len()).sum::<usize>(),
            600,
            "chaining must not lose a byte"
        );
    }

    #[test]
    fn card_statuses_map_to_the_error_the_operator_needs() {
        assert!(matches!(
            expect_ok(0x6982, "test", "x"),
            Err(WriteError::WrongSecret { .. })
        ));
        assert!(matches!(
            expect_ok(0x6D00, "test", "x"),
            Err(WriteError::Unsupported { .. })
        ));
        assert!(expect_ok(0x9000, "test", "x").is_ok());
    }

    #[test]
    fn a_pin_is_padded_to_eight_bytes_and_a_long_one_is_refused() {
        let padded = pad_pin("123456", "test").unwrap();
        assert_eq!(padded.len(), 8);
        assert_eq!(&padded[..6], b"123456");
        assert_eq!(&padded[6..], &[0xFF, 0xFF]);

        // Truncating would set a PIN nobody typed.
        assert!(pad_pin("123456789", "test").is_err());
        assert!(pad_pin("", "test").is_err());
    }

    #[test]
    fn each_slot_names_its_own_certificate_object() {
        assert_eq!(
            certificate_object_id(0x9C, "test").unwrap(),
            [0x5F, 0xC1, 0x0A]
        );
        assert_eq!(
            certificate_object_id(0x9A, "test").unwrap(),
            [0x5F, 0xC1, 0x05]
        );
        assert_eq!(
            certificate_object_id(0x9E, "test").unwrap(),
            [0x5F, 0xC1, 0x01]
        );
        // The management slot holds a key, not a certificate.
        assert!(certificate_object_id(0x9B, "test").is_err());
    }

    #[test]
    fn key_algorithm_names_are_accepted_in_the_forms_templates_use() {
        assert_eq!(
            KeyAlgorithm::from_name("eccp256", "test").unwrap(),
            KeyAlgorithm::EccP256
        );
        assert_eq!(
            KeyAlgorithm::from_name("ECDSA-P384", "test").unwrap(),
            KeyAlgorithm::EccP384
        );
        assert_eq!(KeyAlgorithm::EccP256.id(), 0x11);
        assert_eq!(KeyAlgorithm::Rsa2048.id(), 0x07);
        assert!(KeyAlgorithm::from_name("magic", "test").is_err());
    }

    #[test]
    fn policies_use_the_ids_the_applet_defines() {
        // A wrong id here is the difference between a key that asks for the PIN
        // on every signature and one that never does.
        assert_eq!(PinPolicy::Always.id(), 0x03);
        assert_eq!(PinPolicy::Once.id(), 0x02);
        assert_eq!(TouchPolicy::Cached.id(), 0x03);
        assert_eq!(TouchPolicy::Never.id(), 0x01);
    }

    #[test]
    fn an_ec_generate_response_becomes_a_parsable_public_key() {
        use x509_cert::der::Decode;

        // 0x04 then 64 bytes: an uncompressed P-256 point, as the card frames it.
        let mut point = vec![0x04];
        point.extend((1u8..=64).collect::<Vec<u8>>());
        let mut body = vec![0x86];
        push_len(&mut body, point.len());
        body.extend_from_slice(&point);
        let mut response = vec![0x7F, 0x49];
        push_len(&mut response, body.len());
        response.extend_from_slice(&body);

        let der = spki_from_generated(KeyAlgorithm::EccP256, &response, "test").unwrap();
        let spki = x509_cert::spki::SubjectPublicKeyInfoOwned::from_der(&der)
            .expect("the card's key has to come out as a SubjectPublicKeyInfo");
        assert_eq!(spki.algorithm.oid.to_string(), "1.2.840.10045.2.1");
        assert_eq!(
            spki.subject_public_key.raw_bytes(),
            point.as_slice(),
            "the point must survive the encoding unchanged"
        );
    }

    #[test]
    fn an_rsa_generate_response_becomes_a_parsable_public_key() {
        use x509_cert::der::Decode;

        // A modulus with the high bit set, which is the case that needs the
        // leading zero byte a DER INTEGER requires.
        let modulus = vec![0xFFu8; 256];
        let exponent = vec![0x01, 0x00, 0x01];
        let mut body = vec![0x81];
        push_len(&mut body, modulus.len());
        body.extend_from_slice(&modulus);
        body.push(0x82);
        push_len(&mut body, exponent.len());
        body.extend_from_slice(&exponent);
        let mut response = vec![0x7F, 0x49];
        push_len(&mut response, body.len());
        response.extend_from_slice(&body);

        let der = spki_from_generated(KeyAlgorithm::Rsa2048, &response, "test").unwrap();
        let spki = x509_cert::spki::SubjectPublicKeyInfoOwned::from_der(&der)
            .expect("an RSA key has to come out as a SubjectPublicKeyInfo too");
        assert_eq!(spki.algorithm.oid.to_string(), "1.2.840.113549.1.1.1");
        // SEQUENCE { INTEGER modulus, INTEGER publicExponent }
        let body = find_tlv(spki.subject_public_key.raw_bytes(), 0x30)
            .expect("the key bits are an RSAPublicKey SEQUENCE");
        let integers = tlvs(body);
        assert_eq!(integers.len(), 2, "modulus and exponent, nothing else");
        assert_eq!(
            integers[0].1.first(),
            Some(&0x00),
            "a modulus with the top bit set needs a leading zero, or it reads as negative"
        );
        assert_eq!(
            integers[0].1.len(),
            257,
            "256 bytes of modulus plus the pad byte"
        );
        assert_eq!(integers[1].1, &[0x01, 0x00, 0x01]);
    }

    #[test]
    fn a_generate_response_the_card_did_not_fill_in_is_an_error_not_a_key() {
        assert!(spki_from_generated(KeyAlgorithm::EccP256, &[], "test").is_err());
        // The right wrapper, the wrong contents.
        assert!(spki_from_generated(KeyAlgorithm::EccP256, &[0x7F, 0x49, 0x00], "test").is_err());
    }

    #[test]
    fn der_integers_are_minimal_and_never_negative() {
        assert_eq!(der_integer(&[0x01]), vec![0x02, 0x01, 0x01]);
        assert_eq!(
            der_integer(&[0x00, 0x00, 0x7F]),
            vec![0x02, 0x01, 0x7F],
            "leading zeros are not part of the number"
        );
        assert_eq!(
            der_integer(&[0x80]),
            vec![0x02, 0x02, 0x00, 0x80],
            "the top bit set needs a pad byte"
        );
        assert_eq!(der_integer(&[]), vec![0x02, 0x01, 0x00]);
    }
}
