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
//! outside. Rather than vendor the whole crate for one exchange,
//! [`super::piv_session`] speaks to the card directly over PC/SC — which is what
//! the crate does too — and this module is the management-key operation built on
//! top of it.
//!
//! ## Scope
//!
//! `change_pin`, `change_puk` and the metadata read were all verified working
//! through the crate, so they stay there. What had to move here is the set of
//! operations that need **management-key authentication**, because that
//! authentication belongs to a card session: the management key itself, and — via
//! [`super::piv_session::Session`] — on-device key generation and certificate
//! import.

use zeroize::Zeroizing;

use super::piv_session::{MgmAlgorithm, Session};
use super::write::{Result, WriteError};

/// Where a newly set management key is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedStore {
    /// Written into the PIN-protected data object, so the PIN alone recovers it
    /// and nothing has to be handed over or escrowed. Model B's preferred form.
    OnCardUnderPin,
    /// Not stored anywhere. Whoever set it has to keep it.
    NotStored,
}

/// Authenticate to the card with the current management key, then set a new one.
///
/// `current` is `None` for a factory-fresh applet, which uses the published
/// default — supplied by the caller so no credential is written here.
///
/// The returned algorithm is the slot's, read from the card rather than assumed:
/// it is what the audit entry records, and on current firmware it is AES-192.
pub fn set_management_key(
    serial: u32,
    current: Option<&[u8]>,
    new: &[u8],
    default_key: &[u8],
    store: ProtectedStore,
    require_touch: bool,
) -> Result<MgmAlgorithm> {
    const OP: &str = "piv.set_management_key";

    let mut session = Session::open(serial, OP)?;
    let algorithm = session.management_algorithm(OP)?;
    let current = Zeroizing::new(current.unwrap_or(default_key).to_vec());

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

    session.authenticate(algorithm, &current, OP)?;
    session.set_management_key(algorithm, new, require_touch, OP)?;

    if store == ProtectedStore::OnCardUnderPin {
        // Deliberately after the key is in place: storing a key the card does
        // not have would be worse than not storing one, because the next session
        // would authenticate with something the card rejects.
        session.store_management_key_under_pin(new, OP)?;
    }

    Ok(algorithm)
}
