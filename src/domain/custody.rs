//! Custody of the secrets a bootstrap sets.
//!
//! **Decided 2026-08-10: model B — transport PIN plus forced change.** The
//! operator sets a temporary PIN, the key is marked so the holder must change it
//! before first use, and nothing is retained by this tool.
//!
//! This type exists to fix the vocabulary *before* the executor is written: a
//! run records which model applied, and the record is the only place custody is
//! ever described. The secret value itself is never stored — see
//! `features/secrets-custody.md`.

use serde::{Deserialize, Serialize};

/// How the secrets set by a bootstrap run reach (or do not reach) the holder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CustodyModel {
    /// **A** — the holder types their own secret at the desk; the operator never
    /// learns it. No recovery: a forgotten PIN means a reset.
    HolderSet,
    /// **B (default)** — the operator sets a transport secret, the key is marked
    /// for mandatory change, and the holder changes it on first use. Nothing is
    /// retained here.
    TransportPinForcedChange,
    /// **C** — generated here and escrowed in an *external* secret store. This
    /// tool records only the reference, never the value.
    Escrowed,
    /// No secret was set (a dry run, or a template with no secret-setting step).
    NoSecretSet,
}

impl CustodyModel {
    /// The decided default for new runs.
    pub const DEFAULT: CustodyModel = CustodyModel::TransportPinForcedChange;

    pub const ALL: [CustodyModel; 4] = [
        CustodyModel::HolderSet,
        CustodyModel::TransportPinForcedChange,
        CustodyModel::Escrowed,
        CustodyModel::NoSecretSet,
    ];

    /// Stored form, written into `bootstrap_runs.custody`.
    pub fn as_str(&self) -> &'static str {
        match self {
            CustodyModel::HolderSet => "holder-set",
            CustodyModel::TransportPinForcedChange => "transport-pin+forced-change",
            CustodyModel::Escrowed => "escrowed",
            CustodyModel::NoSecretSet => "no-secret-set",
        }
    }

    /// Operator-facing description.
    pub fn label(&self) -> &'static str {
        match self {
            CustodyModel::HolderSet => "Holder sets their own secret at hand-over",
            CustodyModel::TransportPinForcedChange => {
                "Transport secret, holder must change it on first use"
            }
            CustodyModel::Escrowed => "Generated here, escrowed in an external store",
            CustodyModel::NoSecretSet => "No secret was set",
        }
    }

    /// Read back a stored custody note. Tolerates a trailing reference
    /// (`escrowed:bastionvault:kv/...`) and unknown values.
    pub fn parse(raw: &str) -> Option<Self> {
        let head = raw.trim().split(':').next()?.trim();
        Self::ALL.into_iter().find(|model| model.as_str() == head)
    }

    /// True when a secret has to travel from the operator to the holder, and so
    /// needs an out-of-band channel (sealed envelope, in person).
    pub fn hands_a_secret_to_the_holder(&self) -> bool {
        matches!(self, CustodyModel::TransportPinForcedChange)
    }

    /// True when something is retained outside this tool, so the run must carry
    /// a reference to it.
    pub fn requires_reference(&self) -> bool {
        matches!(self, CustodyModel::Escrowed)
    }

    /// Build the string stored in `bootstrap_runs.custody`.
    ///
    /// `reference` is only meaningful for [`CustodyModel::Escrowed`]; it is a
    /// *pointer* to an external store, never a secret.
    pub fn note(&self, reference: Option<&str>) -> String {
        match (self, reference) {
            (CustodyModel::Escrowed, Some(reference)) if !reference.trim().is_empty() => {
                format!("{}:{}", self.as_str(), reference.trim())
            }
            _ => self.as_str().to_owned(),
        }
    }
}

impl Default for CustodyModel {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Whether the key can enforce the change itself, or the holder has to be
/// *instructed* to change the secret.
///
/// FIDO2 gained `forcePINChange` with CTAP 2.1 (firmware 5.7). PIV has no
/// equivalent at any firmware level: a PIV transport PIN is always procedural.
/// Both facts belong in the record, so a later audit can tell an enforced change
/// from an instructed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeEnforcement {
    /// The key refuses to be used until the secret is changed.
    ByFirmware,
    /// The hand-over term instructs the holder to change it.
    ByProcedure,
}

impl ChangeEnforcement {
    /// For FIDO2, given the key's firmware.
    pub fn for_fido2(firmware: &str) -> Self {
        if crate::device::ykman::supports_ctap21_config(firmware) {
            ChangeEnforcement::ByFirmware
        } else {
            ChangeEnforcement::ByProcedure
        }
    }

    /// For PIV, always. There is no force-change flag in PIV.
    pub fn for_piv() -> Self {
        ChangeEnforcement::ByProcedure
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeEnforcement::ByFirmware => "enforced-by-firmware",
            ChangeEnforcement::ByProcedure => "instructed-on-handover",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_model_b() {
        assert_eq!(
            CustodyModel::default(),
            CustodyModel::TransportPinForcedChange
        );
        assert!(CustodyModel::default().hands_a_secret_to_the_holder());
        assert!(!CustodyModel::default().requires_reference());
    }

    #[test]
    fn only_escrow_carries_a_reference() {
        assert_eq!(
            CustodyModel::Escrowed.note(Some("bastionvault:kv/yubikeys/20423633")),
            "escrowed:bastionvault:kv/yubikeys/20423633"
        );
        // A reference on any other model is meaningless and dropped.
        assert_eq!(
            CustodyModel::TransportPinForcedChange.note(Some("ignored")),
            "transport-pin+forced-change"
        );
    }

    #[test]
    fn a_stored_note_reads_back_including_its_reference() {
        assert_eq!(
            CustodyModel::parse("escrowed:bastionvault:kv/yubikeys/1"),
            Some(CustodyModel::Escrowed)
        );
        assert_eq!(
            CustodyModel::parse("transport-pin+forced-change"),
            Some(CustodyModel::TransportPinForcedChange)
        );
        assert_eq!(CustodyModel::parse("something-else"), None);
    }

    #[test]
    fn fido2_enforcement_depends_on_firmware_but_piv_never_does() {
        assert_eq!(
            ChangeEnforcement::for_fido2("5.7.1"),
            ChangeEnforcement::ByFirmware
        );
        assert_eq!(
            ChangeEnforcement::for_fido2("5.4.3"),
            ChangeEnforcement::ByProcedure,
            "forcePINChange needs CTAP 2.1 (firmware 5.7)"
        );
        assert_eq!(ChangeEnforcement::for_piv(), ChangeEnforcement::ByProcedure);
    }
}
