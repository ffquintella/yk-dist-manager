//! Domain records tracked by the tool.
//!
//! Every struct here is plain data: `serde`-serialisable, no I/O, no GUI types.
//! See `docs/data-model.md` for the field-by-field reference and
//! `features/key-inventory.md` / `features/distribution-records.md` for the
//! specifications.

pub mod bootstrap;
pub mod custody;
pub mod distribution;
pub mod document;
pub mod holder;
pub mod key;
pub mod lifecycle;

pub use bootstrap::{BootstrapRun, RunStatus, StepKind, StepOutcome, StepStatus};
pub use custody::{ChangeEnforcement, CustodyModel};
pub use distribution::{DeliveryMethod, DistributionRecord};
pub use document::{AttachedDocument, DocumentError, DocumentKind};
pub use lifecycle::{
    Dependency, DependencyKind, IncidentKind, KeyIncident, Remediation, RemediationKind,
    RevocationReason, RmaCase, RmaState, Sanitisation,
};
// `escape_rfc4514` is re-exported for the property test that pins its
// reversibility (`features/testing-strategy.md` phase 9): the escaper is what
// stands between a person's name as they spell it and the `CN` of a certificate
// issued to them, so it is worth testing from outside the module that owns it.
pub use holder::{Holder, escape_rfc4514};
pub use key::{KeyStatus, SerialSource, YubiKeyRecord};

/// Maximum accepted length for any free-text field arriving from the UI.
///
/// NRM §5.3.5 requires a maximum size on every input.
pub const MAX_TEXT: usize = 256;

/// Maximum accepted length for note / justification fields.
pub const MAX_NOTE: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("required field missing: {0}")]
    Missing(&'static str),
    #[error("field `{field}` exceeds {max} characters")]
    TooLong { field: &'static str, max: usize },
    #[error("invalid e-mail address: {0}")]
    Email(String),
}

/// Trim, reject empty, and enforce [`MAX_TEXT`].
pub fn require_text(field: &'static str, value: &str) -> Result<String, ValidationError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ValidationError::Missing(field));
    }
    if trimmed.chars().count() > MAX_TEXT {
        return Err(ValidationError::TooLong {
            field,
            max: MAX_TEXT,
        });
    }
    Ok(trimmed.to_owned())
}

/// Trim and enforce [`MAX_TEXT`]; empty is allowed.
pub fn optional_text(field: &'static str, value: &str) -> Result<String, ValidationError> {
    let trimmed = value.trim();
    if trimmed.chars().count() > MAX_TEXT {
        return Err(ValidationError::TooLong {
            field,
            max: MAX_TEXT,
        });
    }
    Ok(trimmed.to_owned())
}

/// Trim and enforce [`MAX_NOTE`]; empty is allowed.
pub fn optional_note(field: &'static str, value: &str) -> Result<String, ValidationError> {
    let trimmed = value.trim();
    if trimmed.chars().count() > MAX_NOTE {
        return Err(ValidationError::TooLong {
            field,
            max: MAX_NOTE,
        });
    }
    Ok(trimmed.to_owned())
}

/// Deliberately conservative e-mail check.
///
/// The address ends up in an X.509 `rfc822Name` SAN (see
/// `features/step-piv-signing-certificate.md`), so anything ambiguous is
/// rejected at entry rather than at certificate-issuance time.
pub fn validate_email(value: &str) -> Result<String, ValidationError> {
    let candidate = require_text("email", value)?.to_ascii_lowercase();

    let mut parts = candidate.split('@');
    let (local, domain) = match (parts.next(), parts.next(), parts.next()) {
        (Some(local), Some(domain), None) => (local, domain),
        _ => return Err(ValidationError::Email(candidate)),
    };

    let local_ok = !local.is_empty()
        && local.len() <= 64
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._%+-".contains(c));
    let domain_ok = domain.len() >= 3
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains("..")
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');

    if local_ok && domain_ok {
        Ok(candidate)
    } else {
        Err(ValidationError::Email(candidate))
    }
}

/// Cut a live text field down to `max` characters, in place.
///
/// NRM §5.3.5 wants the bound applied at the input, not only at validation.
/// egui's own `TextEdit::char_limit` did that for the stock widget; the
/// elegance inputs have no equivalent, so the GUI applies the cap here right
/// after the field is painted. Counting is by `char`, not by byte, so a name
/// in Cyrillic is bounded the same way a name in ASCII is.
pub fn clamp_text(value: &mut String, max: usize) {
    // `char_indices().nth(max)` is `None` exactly when the string is already
    // short enough, which makes the common case a single pass and no write.
    if let Some((cut, _)) = value.char_indices().nth(max) {
        value.truncate(cut);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_trimmed_and_bounded() {
        assert_eq!(require_text("name", "  Ana  ").unwrap(), "Ana");
        assert_eq!(
            require_text("name", "   ").unwrap_err(),
            ValidationError::Missing("name")
        );
        let long = "a".repeat(MAX_TEXT + 1);
        assert!(matches!(
            require_text("name", &long).unwrap_err(),
            ValidationError::TooLong { .. }
        ));
    }

    #[test]
    fn clamping_cuts_at_a_character_boundary_not_a_byte_one() {
        // Five characters, ten bytes: a byte-wise truncation to 3 would split
        // the third character and panic.
        let mut value = "áéíóú".to_owned();
        clamp_text(&mut value, 3);
        assert_eq!(value, "áéí");
    }

    #[test]
    fn clamping_leaves_a_short_enough_field_alone() {
        let mut value = "Ana".to_owned();
        clamp_text(&mut value, MAX_TEXT);
        assert_eq!(value, "Ana");

        // Exactly at the limit is within it.
        let mut exact = "abc".to_owned();
        clamp_text(&mut exact, 3);
        assert_eq!(exact, "abc");
    }

    #[test]
    fn email_is_normalised_to_lowercase() {
        assert_eq!(
            validate_email("Ana.Silva@Example.ORG").unwrap(),
            "ana.silva@example.org"
        );
    }
}
