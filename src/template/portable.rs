//! A template as a **file**, for sharing a procedure between units
//! (`features/bootstrap-templates.md` phase 4).
//!
//! Today a procedure crosses between two installations by somebody retyping it,
//! which is the one method guaranteed to introduce a difference nobody notices —
//! and this feature already knows what a one-step difference costs: a FIDO2
//! ordering mistake made `org-standard` v1 unable to complete on hardware. A file
//! makes the transfer exact, and (with phase 5) makes it verifiable.
//!
//! ## The file is a wrapper, not the template
//!
//! ```json
//! {
//!   "format": "yk-dist-manager/template/1",
//!   "exported_by": "yk-dist-manager 0.8.0",
//!   "exported_at": "2026-08-12T14:05:00Z",
//!   "template": { "id": "org-standard", "version": "2", ... }
//! }
//! ```
//!
//! The wrapper exists so the reader can refuse a file it does not understand
//! *before* deserialising a template out of it, and so the provenance fields have
//! somewhere to live that is **outside** what a signature covers — a signature
//! over the export timestamp would be a signature that changes every time
//! somebody re-exports the same procedure.
//!
//! ## What import does and does not preserve
//!
//! It preserves the procedure: id, name, description and every step in order with
//! its parameters — and the signature, if the file carries one, because
//! [`super::signing::canonical_bytes`] deliberately excludes the version.
//!
//! It does **not** preserve the version number. The receiving database assigns
//! the next one for that id ([`crate::versioning::next_version`]), which is the
//! rule this feature already lives by: a version is a local bookkeeping fact, and
//! two units that both call their procedure "version 2" would otherwise import
//! each other's and silently disagree about what "v2" means.
//!
//! Nor does it preserve retirement or the run count. Those are facts about *this*
//! register — who ran what, and what somebody here decided to withdraw — and they
//! belong to the database, not to the procedure.

use serde::{Deserialize, Serialize};

use crate::template::{BootstrapTemplate, TemplateError};

/// The format tag. Bumped only for a change a previous reader could not survive.
pub const FILE_FORMAT: &str = "yk-dist-manager/template/1";

/// Extension offered by the save dialog and expected by the open dialog.
pub const FILE_EXTENSION: &str = "json";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PortableError {
    #[error("this file is not a bootstrap template export: {0}")]
    NotAnExport(String),
    #[error(
        "this file says it is format `{found}`, and this build reads `{expected}`. It was written \
         by a newer version of the application — upgrade this workstation rather than editing the \
         file"
    )]
    WrongFormat { found: String, expected: String },
    #[error("the template in this file was refused: {0}")]
    Refused(#[from] TemplateError),
}

/// The wrapper written to and read from a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateFile {
    pub format: String,
    /// Which build wrote it, for a bug report years later.
    #[serde(default)]
    pub exported_by: String,
    /// When, RFC 3339. Provenance only — deliberately outside the signature.
    #[serde(default)]
    pub exported_at: String,
    /// The version the exporting database happened to hold, kept for the reader's
    /// information and **not** used on import. Recorded because "we sent you our
    /// v4" is how two units talk about a procedure on the phone.
    #[serde(default)]
    pub exported_version: String,
    pub template: BootstrapTemplate,
}

impl TemplateFile {
    /// Wrap a template for export.
    pub fn of(template: &BootstrapTemplate, now: chrono::DateTime<chrono::Utc>) -> Self {
        Self {
            format: FILE_FORMAT.to_owned(),
            exported_by: format!("yk-dist-manager {}", crate::VERSION),
            exported_at: now.to_rfc3339(),
            exported_version: template.version.clone(),
            template: template.clone(),
        }
    }

    /// The file's bytes: JSON, pretty-printed and newline-terminated.
    ///
    /// Pretty rather than compact because this file is meant to be *read* — by a
    /// reviewer deciding whether to trust a procedure, and by whoever has to sign
    /// it. It is also what makes the file diffable in a version-control system,
    /// which is how a unit that keeps its procedures under review will actually
    /// hold them.
    pub fn to_json(&self) -> String {
        let mut json = serde_json::to_string_pretty(self).expect("a template export serialises");
        json.push('\n');
        json
    }

    /// Read an export, refusing anything this build cannot honour.
    ///
    /// The template is put through [`BootstrapTemplate::check`] here, at the file
    /// boundary: a file is untrusted input, and the gate that plans a template
    /// against sample data is exactly the check that keeps an unplannable
    /// procedure out of the database. A malformed file must not be able to reach
    /// the store at all.
    pub fn from_json(raw: &str) -> Result<Self, PortableError> {
        let file: Self =
            serde_json::from_str(raw).map_err(|e| PortableError::NotAnExport(e.to_string()))?;
        if file.format != FILE_FORMAT {
            return Err(PortableError::WrongFormat {
                found: file.format.clone(),
                expected: FILE_FORMAT.to_owned(),
            });
        }
        file.template.check()?;
        Ok(file)
    }

    /// A file name that says what is inside it: `org-standard-v2.json`.
    ///
    /// The id is already restricted to lower-case letters, digits and hyphens
    /// ([`crate::template::check_id`]), so it is safe in a file name by
    /// construction — no separator, no traversal, nothing to sanitise. The version
    /// is numeric for the same reason.
    pub fn suggested_name(template: &BootstrapTemplate) -> String {
        format!(
            "{}-v{}.{FILE_EXTENSION}",
            template.id.trim(),
            template.version.trim()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::signing::{TemplateSignature, canonical_bytes};

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-08-12T14:05:00Z")
            .unwrap()
            .into()
    }

    fn standard() -> BootstrapTemplate {
        BootstrapTemplate::builtin()
            .into_iter()
            .next()
            .expect("a built-in template")
    }

    #[test]
    fn a_template_round_trips_through_a_file_byte_for_byte() {
        // The whole point of the feature: what arrives is what left. A procedure
        // that changed in transit is the failure mode retyping already has.
        let original = standard();
        let json = TemplateFile::of(&original, now()).to_json();
        let read = TemplateFile::from_json(&json).expect("our own export reads back");
        assert_eq!(read.template, original);
        assert_eq!(
            canonical_bytes(&read.template),
            canonical_bytes(&original),
            "the canonical bytes must survive, or a signature could not"
        );
    }

    #[test]
    fn a_signature_survives_the_file() {
        let mut signed = standard();
        signed.signature = Some(TemplateSignature {
            key_id: "esi-2026".into(),
            algorithm: crate::template::signing::ALGORITHM.into(),
            signature: "ab".repeat(64),
        });
        let read = TemplateFile::from_json(&TemplateFile::of(&signed, now()).to_json()).unwrap();
        assert_eq!(read.template.signature, signed.signature);
    }

    #[test]
    fn the_export_records_its_provenance_outside_the_template() {
        let file = TemplateFile::of(&standard(), now());
        assert_eq!(file.exported_at, "2026-08-12T14:05:00+00:00");
        assert!(file.exported_by.contains("yk-dist-manager"));
        assert_eq!(file.exported_version, standard().version);
        // None of it is inside what a signature covers.
        let json = file.to_json();
        assert!(json.contains("\"exported_at\""));
        assert!(
            !String::from_utf8(canonical_bytes(&file.template))
                .unwrap()
                .contains("2026-08-12")
        );
    }

    #[test]
    fn a_file_from_a_newer_build_is_refused_by_name() {
        let mut file = TemplateFile::of(&standard(), now());
        file.format = "yk-dist-manager/template/2".into();
        let raw = serde_json::to_string(&file).unwrap();
        match TemplateFile::from_json(&raw) {
            Err(PortableError::WrongFormat { found, expected }) => {
                assert_eq!(found, "yk-dist-manager/template/2");
                assert_eq!(expected, FILE_FORMAT);
            }
            other => panic!("expected WrongFormat, got {other:?}"),
        }
    }

    #[test]
    fn something_that_is_not_an_export_is_refused_rather_than_guessed_at() {
        for raw in [
            "",
            "not json at all",
            "{}",
            r#"{"format":"yk-dist-manager/template/1"}"#,
            "[1, 2, 3]",
        ] {
            assert!(
                matches!(
                    TemplateFile::from_json(raw),
                    Err(PortableError::NotAnExport(_))
                ),
                "{raw:?} should not parse as an export"
            );
        }
    }

    #[test]
    fn an_unplannable_template_is_refused_at_the_file_boundary() {
        // A file is untrusted input. The gate that keeps a broken procedure out of
        // the database is `check()`, and it has to run before the store is asked to
        // do anything — otherwise the only thing standing between a hand-edited
        // file and a run is the store remembering to check.
        let mut broken = standard();
        // A parameter the step's kind actually *renders* — the certificate subject.
        // A value nothing reads is not an error, which is itself deliberate: a
        // template may carry a parameter a future step kind will read.
        broken
            .steps
            .iter_mut()
            .find(|s| s.id == "piv-csr")
            .expect("the standard procedure requests a certificate")
            .params
            .insert("subject".into(), "CN={{holder.nickname}}".into());
        let raw = TemplateFile::of(&broken, now()).to_json();
        match TemplateFile::from_json(&raw) {
            Err(PortableError::Refused(TemplateError::UnknownVariable(name))) => {
                assert_eq!(name, "holder.nickname")
            }
            other => panic!("expected an unknown-variable refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_suggested_name_says_what_is_inside_and_cannot_escape_a_directory() {
        let name = TemplateFile::suggested_name(&standard().as_version("4"));
        assert!(name.ends_with("-v4.json"), "{name}");
        assert!(!name.contains('/'), "{name}");
        assert!(!name.contains(".."), "{name}");
    }

    #[test]
    fn the_file_is_readable_by_a_person_and_ends_with_a_newline() {
        // It is reviewed before it is trusted, and signed by somebody who has to
        // read it first. A single compact line would be neither.
        let json = TemplateFile::of(&standard(), now()).to_json();
        assert!(json.ends_with('\n'));
        assert!(json.lines().count() > 20, "pretty-printed");
    }
}
