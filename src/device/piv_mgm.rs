//! Setting the PIV management key on firmware that uses AES.
//!
//! This module exists because of one measured failure. The [`yubikey`] crate's
//! `MgmKey` is `[u8; 24]` with DES odd-parity weak-key checks — a **3DES** type —
//! and its `authenticate` sends a 3DES algorithm identifier. **Firmware 5.7
//! removed 3DES** and defaults the management key slot to **AES-192**, so no
//! byte value makes that authentication succeed: the crate cannot manage the
//! management key on any 5.7 key. Verified against a 5C NFC 5.7.4; the result is
//! in `features/step-piv-pin-puk-management-key.md`.
//!
//! The crate's `mod transaction` is private, so the fix cannot be a patch from
//! outside. Rather than vendor the whole crate for one exchange, this speaks to
//! the card directly over PC/SC — which is what the crate does too, and the
//! protocol involved is two APDUs.
//!
//! ## Scope, deliberately narrow
//!
//! **Only the management key.** `change_pin`, `change_puk` and the metadata read
//! were all verified working through the crate, so they stay there. This is not
//! a reimplementation of PIV; it is the one exchange the crate gets wrong,
//! replaced.
//!
//! ## The exchange
//!
//! Mutual authentication, as the card expects (SP 800-73-4, and what
//! `yubico-piv-tool` does):
//!
//! 1. `GENERAL AUTHENTICATE` asking for a **witness** — the card sends a block it
//!    has encrypted with the management key it currently holds.
//! 2. Decrypt it. Getting this right is what proves *we* know the key.
//! 3. Send the decrypted witness back with a **challenge** of our own.
//! 4. The card returns our challenge encrypted. Checking it is what proves the
//!    *card* knows the key — which is why mutual authentication is worth the
//!    extra round trip: a card that cannot prove itself is not one to write a new
//!    management key into.
//!
//! Then `SET MANAGEMENT KEY` carries the new key, tagged with its algorithm.
//!
//! ## What this does not do
//!
//! `protect = true` — storing the key in the PIN-protected data object so nothing
//! has to be retained — is [`ProtectedStore`], and is a separate object write.
//! It is implemented here too, because model B's whole argument is that nothing
//! is retained, and a management key that has to be kept somewhere would undo it.

use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};

use super::write::{Result, WriteError};

/// PIV application id.
const PIV_AID: [u8; 5] = [0xA0, 0x00, 0x00, 0x03, 0x08];

/// The management key slot.
const SLOT_MANAGEMENT: u8 = 0x9B;

/// PIV algorithm identifiers for the management key slot.
const ALG_3DES: u8 = 0x03;
const ALG_AES128: u8 = 0x08;
const ALG_AES192: u8 = 0x0A;
const ALG_AES256: u8 = 0x0C;

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

/// Where a newly set management key is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedStore {
    /// Written into the PIN-protected data object, so the PIN alone recovers it
    /// and nothing has to be handed over or escrowed. Model B's preferred form.
    OnCardUnderPin,
    /// Not stored anywhere. Whoever set it has to keep it.
    NotStored,
}

/// One card session, for the duration of this exchange.
///
/// Opened and dropped inside a single call rather than held: the `yubikey` crate
/// holds its own connection for PIN and PUK work, and two exclusive handles to
/// one reader is how a session gets refused.
struct Card {
    card: pcsc::Card,
}

impl Card {
    fn open(serial: u32, operation: &'static str) -> Result<Self> {
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

    /// Send an APDU, returning the data and the two status bytes.
    fn transmit(&mut self, apdu: &[u8], operation: &'static str) -> Result<(Vec<u8>, u16)> {
        let mut buf = vec![0u8; 4096];
        let response = self
            .card
            .transmit(apdu, &mut buf)
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
        Ok((response[..split].to_vec(), sw))
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

    /// Which cipher the management key slot currently uses.
    ///
    /// From `GET METADATA` (firmware 5.3+). Without it there is no way to know
    /// whether to speak 3DES or AES, and guessing wrong is indistinguishable
    /// from a wrong key — which is precisely the failure this module was written
    /// to fix, so it is read rather than assumed.
    fn management_algorithm(&mut self, operation: &'static str) -> Result<MgmAlgorithm> {
        let (data, sw) = self.transmit(&[0x00, 0xF7, 0x00, SLOT_MANAGEMENT], operation)?;
        if sw != 0x9000 {
            // Pre-5.3 firmware cannot say. Those keys are 3DES-only, which is
            // exactly what the metadata's absence implies.
            return Ok(MgmAlgorithm::Tdes);
        }
        // Tag 0x01 carries the algorithm.
        for (tag, value) in tlvs(&data) {
            if tag == 0x01 && !value.is_empty() {
                return MgmAlgorithm::from_id(value[0]).ok_or(WriteError::Unsupported {
                    operation,
                    reason: format!("the management key uses algorithm 0x{:02x}", value[0]),
                });
            }
        }
        Ok(MgmAlgorithm::Tdes)
    }
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

/// Walk a BER-TLV sequence, one level deep. Enough for the structures here,
/// which are short and flat.
fn tlvs(data: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < data.len() {
        let tag = data[i];
        let len = data[i + 1] as usize;
        let start = i + 2;
        if start + len > data.len() {
            break;
        }
        out.push((tag, &data[start..start + len]));
        i = start + len;
    }
    out
}

fn encrypt_block(algorithm: MgmAlgorithm, key: &[u8], block: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::Array;
    let mut out = block.to_vec();
    match algorithm {
        MgmAlgorithm::Aes192 => {
            let cipher = aes::Aes192::new_from_slice(key).map_err(|_| bad_key())?;
            let mut b = Array::try_from(&out[..]).map_err(|_| bad_block())?;
            cipher.encrypt_block(&mut b);
            out.copy_from_slice(&b);
        }
        MgmAlgorithm::Aes128 => {
            let cipher = aes::Aes128::new_from_slice(key).map_err(|_| bad_key())?;
            let mut b = Array::try_from(&out[..]).map_err(|_| bad_block())?;
            cipher.encrypt_block(&mut b);
            out.copy_from_slice(&b);
        }
        MgmAlgorithm::Aes256 => {
            let cipher = aes::Aes256::new_from_slice(key).map_err(|_| bad_key())?;
            let mut b = Array::try_from(&out[..]).map_err(|_| bad_block())?;
            cipher.encrypt_block(&mut b);
            out.copy_from_slice(&b);
        }
        MgmAlgorithm::Tdes => return Err(tdes_unsupported()),
    }
    Ok(out)
}

fn decrypt_block(algorithm: MgmAlgorithm, key: &[u8], block: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::Array;
    let mut out = block.to_vec();
    match algorithm {
        MgmAlgorithm::Aes192 => {
            let cipher = aes::Aes192::new_from_slice(key).map_err(|_| bad_key())?;
            let mut b = Array::try_from(&out[..]).map_err(|_| bad_block())?;
            cipher.decrypt_block(&mut b);
            out.copy_from_slice(&b);
        }
        MgmAlgorithm::Aes128 => {
            let cipher = aes::Aes128::new_from_slice(key).map_err(|_| bad_key())?;
            let mut b = Array::try_from(&out[..]).map_err(|_| bad_block())?;
            cipher.decrypt_block(&mut b);
            out.copy_from_slice(&b);
        }
        MgmAlgorithm::Aes256 => {
            let cipher = aes::Aes256::new_from_slice(key).map_err(|_| bad_key())?;
            let mut b = Array::try_from(&out[..]).map_err(|_| bad_block())?;
            cipher.decrypt_block(&mut b);
            out.copy_from_slice(&b);
        }
        MgmAlgorithm::Tdes => return Err(tdes_unsupported()),
    }
    Ok(out)
}

fn bad_key() -> WriteError {
    WriteError::Failed {
        operation: "piv.set_management_key",
        reason: "the management key is the wrong length for the slot's algorithm".into(),
    }
}

fn bad_block() -> WriteError {
    WriteError::Failed {
        operation: "piv.set_management_key",
        reason: "the card returned a block of the wrong size".into(),
    }
}

fn tdes_unsupported() -> WriteError {
    WriteError::Unsupported {
        operation: "piv.set_management_key",
        reason: "this key's management slot still uses 3DES; that path stays with the `yubikey` \
                 crate, which handles it"
            .into(),
    }
}

/// Authenticate to the card with the current management key, then set a new one.
///
/// `current` is `None` for a factory-fresh applet, which uses the published
/// default — supplied by the caller so no credential is written here.
pub fn set_management_key(
    serial: u32,
    current: Option<&[u8]>,
    new: &[u8],
    default_key: &[u8],
    store: ProtectedStore,
    require_touch: bool,
) -> Result<MgmAlgorithm> {
    const OP: &str = "piv.set_management_key";

    let mut card = Card::open(serial, OP)?;
    let algorithm = card.management_algorithm(OP)?;
    let current = current.unwrap_or(default_key);

    if current.len() != algorithm.key_len() {
        return Err(WriteError::Failed {
            operation: OP,
            reason: format!(
                "the current management key is {} bytes and this slot's {} needs {}",
                current.len(),
                algorithm.label(),
                algorithm.key_len()
            ),
        });
    }
    if new.len() != algorithm.key_len() {
        return Err(WriteError::Failed {
            operation: OP,
            reason: format!(
                "the new management key is {} bytes and this slot's {} needs {}",
                new.len(),
                algorithm.label(),
                algorithm.key_len()
            ),
        });
    }

    authenticate(&mut card, algorithm, current, OP)?;
    write_key(&mut card, algorithm, new, require_touch, OP)?;

    if store == ProtectedStore::OnCardUnderPin {
        // Deliberately after the key is in place: storing a key the card does
        // not have would be worse than not storing one, because the next session
        // would authenticate with something the card rejects.
        store_under_pin(&mut card, new, OP)?;
    }

    Ok(algorithm)
}

/// The mutual-authentication exchange.
fn authenticate(
    card: &mut Card,
    algorithm: MgmAlgorithm,
    key: &[u8],
    operation: &'static str,
) -> Result<()> {
    let block = algorithm.block_len();

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
    let (data, sw) = card.transmit(&apdu, operation)?;
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
    let (data, sw) = card.transmit(&apdu, operation)?;
    // A wrong management key surfaces here, and this is the one status worth
    // translating precisely: it is the difference between "you typed the wrong
    // key" and "something is broken".
    if sw == 0x6982 || sw == 0x6983 {
        return Err(WriteError::WrongSecret {
            applet: "PIV",
            retries_left: 0,
        });
    }
    expect_ok(sw, operation, "answering the authentication witness")?;

    // 4. The card returns our challenge encrypted. Checking it is what proves
    //    the card holds the key too — without this the exchange authenticates
    //    us to the card and not the card to us.
    let response = inner_tlv(&data, 0x7C, 0x82).ok_or(WriteError::Failed {
        operation,
        reason: "the card did not answer the challenge".into(),
    })?;
    let expected = encrypt_block(algorithm, key, &challenge)?;
    if response != expected.as_slice() {
        return Err(WriteError::Failed {
            operation,
            reason: "the card failed to prove it holds the management key — refusing to write a \
                     new one to it"
                .into(),
        });
    }
    Ok(())
}

/// `SET MANAGEMENT KEY`.
fn write_key(
    card: &mut Card,
    algorithm: MgmAlgorithm,
    new: &[u8],
    require_touch: bool,
    operation: &'static str,
) -> Result<()> {
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

    let (_, sw) = card.transmit(&apdu, operation)?;
    expect_ok(sw, operation, "writing the new management key")
}

/// Store the key in the PIN-protected data object (`0x5FC109`).
///
/// This is what `--protect` means: the key lives on the card, readable only
/// after the PIN, so under custody model B there is nothing to hand over and
/// nothing to retain.
fn store_under_pin(card: &mut Card, key: &[u8], operation: &'static str) -> Result<()> {
    // The protected object holds a small TLV structure; tag 0x89 is the
    // management key.
    let mut inner = vec![0x89, key.len() as u8];
    inner.extend_from_slice(key);
    let mut content = vec![0x88, inner.len() as u8];
    content.extend_from_slice(&inner);

    // PUT DATA: `5C 03 5F C1 09` selects the object, `53 <len>` carries it.
    let mut data = vec![0x5C, 0x03, 0x5F, 0xC1, 0x09, 0x53, content.len() as u8];
    data.extend_from_slice(&content);

    let mut apdu = vec![0x00, 0xDB, 0x3F, 0xFF, data.len() as u8];
    apdu.extend_from_slice(&data);

    let (_, sw) = card.transmit(&apdu, operation)?;
    expect_ok(sw, operation, "storing the management key under the PIN")
}

/// Find a tag nested one level inside another.
fn inner_tlv(data: &[u8], outer: u8, inner: u8) -> Option<&[u8]> {
    for (tag, value) in tlvs(data) {
        if tag == outer {
            for (t, v) in tlvs(value) {
                if t == inner {
                    return Some(v);
                }
            }
        }
    }
    None
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
}
