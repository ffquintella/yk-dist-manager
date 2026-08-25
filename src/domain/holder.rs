//! The person holding a key.
//!
//! This is the only place where personal data lives. Keep it minimal: name,
//! corporate e-mail (needed for the signing certificate), organisational unit
//! and an optional payroll/registration id. See
//! `docs/security-and-compliance.md` for the LGPD notes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::{ValidationError, optional_note, optional_text, require_text, validate_email};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Holder {
    pub id: Uuid,
    pub full_name: String,
    /// Corporate address; goes into the certificate `rfc822Name` SAN.
    pub email: String,
    /// Department / "lotação".
    pub unit: String,
    /// Optional registration id, when the unit needs it for asset control.
    pub registration: String,
    /// Optional national identification number (CPF in Brazil, or the local
    /// equivalent). Called an *identification number* rather than CPF because the
    /// field is not limited to one country's document — and it appears on the
    /// consignment term, which is why it is here at all.
    pub identification_number: String,
    /// Optional contact number.
    pub phone: String,
    /// Optional address, for a key sent by post.
    pub address: String,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

impl Holder {
    /// The required fields. Optional ones are added with [`Holder::with_optional`].
    pub fn new(
        full_name: &str,
        email: &str,
        unit: &str,
        registration: &str,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            id: Uuid::new_v4(),
            full_name: require_text("full_name", full_name)?,
            email: validate_email(email)?,
            unit: require_text("unit", unit)?,
            registration: optional_text("registration", registration)?,
            identification_number: String::new(),
            phone: String::new(),
            address: String::new(),
            active: true,
            created_at: Utc::now(),
        })
    }

    /// Attach the optional fields. Each is length-bounded like every other input,
    /// and an empty value means "not provided" — which is what makes the
    /// corresponding line disappear from a rendered term.
    pub fn with_optional(
        mut self,
        identification_number: &str,
        phone: &str,
        address: &str,
    ) -> Result<Self, ValidationError> {
        self.identification_number = optional_text("identification_number", identification_number)?;
        self.phone = optional_text("phone", phone)?;
        self.address = optional_note("address", address)?;
        Ok(self)
    }

    /// The same person, with the fields an operator may correct revalidated.
    ///
    /// `id`, `created_at` and `active` are kept, and that is the whole point: an
    /// edit is a **correction to a record**, not a new record. Every hand-over,
    /// bootstrap run and term points at this `id`, so a corrected spelling must
    /// not become a second person the register has to reconcile.
    ///
    /// The optional fields are left as they were — chain [`Holder::with_optional`]
    /// to set them, exactly as registration does. Unlike the re-registration
    /// upsert in [`crate::store::Store::insert_holder`], an edit that clears an
    /// optional field **clears it**: the operator is looking at the record and
    /// meant to empty it.
    pub fn with_details(
        &self,
        full_name: &str,
        email: &str,
        unit: &str,
        registration: &str,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            id: self.id,
            full_name: require_text("full_name", full_name)?,
            email: validate_email(email)?,
            unit: require_text("unit", unit)?,
            registration: optional_text("registration", registration)?,
            identification_number: self.identification_number.clone(),
            phone: self.phone.clone(),
            address: self.address.clone(),
            active: self.active,
            created_at: self.created_at,
        })
    }

    /// What an edit changed, for the audit detail. Empty when nothing did.
    ///
    /// Field *names* for everything except the e-mail, which is named old and
    /// new: it is the value that binds the signing certificate to the person
    /// (`features/step-piv-signing-certificate.md`), so "the address moved from
    /// here to there" is the line an auditor needs, and the trail is where it
    /// belongs. No optional value is spelled out — an identification number is a
    /// step up in sensitivity and the trail records *that it changed*, not what
    /// it changed to.
    pub fn describe_changes_from(&self, before: &Self) -> String {
        let mut changes = Vec::new();
        if self.full_name != before.full_name {
            changes.push("name".to_owned());
        }
        if self.email != before.email {
            changes.push(format!("e-mail {} -> {}", before.email, self.email));
        }
        if self.unit != before.unit {
            changes.push("unit".to_owned());
        }
        if self.registration != before.registration {
            changes.push("registration".to_owned());
        }
        if self.identification_number != before.identification_number {
            changes.push("identification number".to_owned());
        }
        if self.phone != before.phone {
            changes.push("phone".to_owned());
        }
        if self.address != before.address {
            changes.push("address".to_owned());
        }
        changes.join(", ")
    }

    /// True when the holder record carries everything a consignment term needs
    /// beyond the mandatory fields.
    pub fn has_identification(&self) -> bool {
        !self.identification_number.trim().is_empty()
    }

    /// `Ana Silva <ana.silva@example.org>`, for tables and receipts.
    pub fn display(&self) -> String {
        format!("{} <{}>", self.full_name, self.email)
    }

    /// RFC 4514 subject used when requesting the signing certificate.
    ///
    /// The e-mail is *not* placed in the DN — it belongs in the `rfc822Name`
    /// SAN, which `ykman piv certificates request` cannot emit. See
    /// `features/step-piv-signing-certificate.md`.
    pub fn certificate_subject(&self, org: &str, org_unit: &str) -> String {
        let mut rdns = vec![format!("CN={}", escape_rfc4514(&self.full_name))];
        if !org_unit.trim().is_empty() {
            rdns.push(format!("OU={}", escape_rfc4514(org_unit)));
        }
        if !org.trim().is_empty() {
            rdns.push(format!("O={}", escape_rfc4514(org)));
        }
        rdns.join(",")
    }
}

/// What an operator is told before they move a holder's address.
///
/// Here rather than in the screen because it is a *rule* being explained, not a
/// decoration: the address in a holder record is the `rfc822Name` that was put
/// into the certificate on the keys they already have
/// (`features/step-piv-signing-certificate.md`), and correcting the register
/// does not reissue any of them. The sentence names how many keys are affected,
/// because "some certificates may be stale" is not something anyone can act on.
///
/// `None` when there is nothing to warn about: the person has never been handed
/// a key, so no certificate carries the old address.
pub fn email_change_warning(from: &str, handed_over: usize, still_out: usize) -> Option<String> {
    if handed_over == 0 {
        return None;
    }
    let keys = if handed_over == 1 {
        "1 key".to_owned()
    } else {
        format!("{handed_over} keys")
    };
    Some(format!(
        "This moves the address away from {from}. {keys} already handed over to this person \
         ({still_out} still out): the signing certificate on those keys names the old address, \
         and correcting the record here does not reissue it."
    ))
}

/// Escape the characters RFC 4514 §2.4 requires escaping in an attribute value.
pub fn escape_rfc4514(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for (i, ch) in value.char_indices() {
        let last = i + ch.len_utf8() == value.len();
        match ch {
            '"' | '+' | ',' | ';' | '<' | '>' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            '#' if i == 0 => out.push_str("\\#"),
            ' ' if i == 0 || last => out.push_str("\\ "),
            _ => out.push(ch),
        }
    }
    out
}
