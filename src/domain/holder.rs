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
            registration: registration.trim().to_owned(),
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
