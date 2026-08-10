//! Documents attached to a distribution — above all, the **signed term**.
//!
//! The bytes live in the database rather than as a path on somebody's desktop.
//! That follows from the deployment model: the database is one file that can sit
//! on a share, so a path reference would break the moment the file moved, and the
//! signed term is the evidence that makes the distribution record worth keeping.
//!
//! Consequences taken deliberately:
//!
//! * A signed term contains personal data (a name, an identification number, a
//!   signature), which strengthens the case for the optional database password —
//!   see `docs/security-and-compliance.md`.
//! * Uploads are size-capped, because a database on a share is copied and backed
//!   up whole.
//! * Every document carries a SHA-256, so a later reader can tell that the bytes
//!   are the ones that were filed.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Largest accepted upload. A scanned, signed A4 page is well under this; a
/// 40-page photo album is not, and does not belong here.
pub const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;

/// What an attached document is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocumentKind {
    /// The consignment term, signed by the holder.
    SignedTerm,
    /// The term as generated, before signature.
    GeneratedTerm,
    /// The receipt for a returned key.
    ReturnReceipt,
    /// Anything else the operator needs to file with the hand-over.
    Other,
}

impl DocumentKind {
    pub const ALL: [DocumentKind; 4] = [
        DocumentKind::SignedTerm,
        DocumentKind::GeneratedTerm,
        DocumentKind::ReturnReceipt,
        DocumentKind::Other,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            DocumentKind::SignedTerm => "Signed term",
            DocumentKind::GeneratedTerm => "Generated term",
            DocumentKind::ReturnReceipt => "Return receipt",
            DocumentKind::Other => "Other document",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocumentError {
    #[error("the file is empty")]
    Empty,
    #[error("the file is {size} bytes; the limit is {limit}")]
    TooLarge { size: usize, limit: usize },
    #[error("`{0}` is not an accepted document type — use PDF, PNG, JPEG or TIFF")]
    UnsupportedType(String),
}

/// A document filed against a distribution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachedDocument {
    pub id: Uuid,
    pub distribution_id: Uuid,
    pub kind: DocumentKind,
    /// Original file name, for re-export.
    pub filename: String,
    /// Media type, inferred from the extension.
    pub media_type: String,
    pub size_bytes: usize,
    /// Lowercase hex SHA-256 of the content.
    pub sha256: String,
    pub uploaded_at: DateTime<Utc>,
    pub uploaded_by: String,
    /// The content. Loaded on demand: listings do not carry it.
    #[serde(skip)]
    pub content: Option<Vec<u8>>,
}

impl AttachedDocument {
    /// Validate an upload and build the record.
    pub fn new(
        distribution_id: Uuid,
        kind: DocumentKind,
        filename: &str,
        content: Vec<u8>,
        uploaded_by: &str,
    ) -> Result<Self, DocumentError> {
        if content.is_empty() {
            return Err(DocumentError::Empty);
        }
        if content.len() > MAX_DOCUMENT_BYTES {
            return Err(DocumentError::TooLarge {
                size: content.len(),
                limit: MAX_DOCUMENT_BYTES,
            });
        }

        let filename = sanitise_filename(filename);
        let media_type = media_type_for(&filename)?;

        let mut hasher = Sha256::new();
        hasher.update(&content);
        let sha256 = hex::encode(hasher.finalize());

        Ok(Self {
            id: Uuid::new_v4(),
            distribution_id,
            kind,
            filename,
            media_type,
            size_bytes: content.len(),
            sha256,
            uploaded_at: Utc::now(),
            uploaded_by: uploaded_by.to_owned(),
            content: Some(content),
        })
    }

    /// Confirm the stored bytes still hash to what was filed.
    pub fn verify(&self) -> Option<bool> {
        let content = self.content.as_ref()?;
        let mut hasher = Sha256::new();
        hasher.update(content);
        Some(hex::encode(hasher.finalize()) == self.sha256)
    }

    /// Human-readable size for the UI.
    pub fn size_label(&self) -> String {
        let kib = self.size_bytes as f64 / 1024.0;
        if kib < 1024.0 {
            format!("{kib:.0} KiB")
        } else {
            format!("{:.1} MiB", kib / 1024.0)
        }
    }

    /// First 12 hex characters of the digest, enough to quote in a ticket.
    pub fn short_digest(&self) -> String {
        self.sha256.chars().take(12).collect()
    }
}

/// Strip any directory component: an uploaded name is data, not a path.
pub fn sanitise_filename(raw: &str) -> String {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        .trim()
        .trim_start_matches('.');
    let cleaned: String = base.chars().filter(|c| !c.is_control()).take(120).collect();
    if cleaned.is_empty() {
        "document".to_owned()
    } else {
        cleaned
    }
}

/// Media type from the extension. Only the formats a scanner produces.
pub fn media_type_for(filename: &str) -> Result<String, DocumentError> {
    let extension = filename
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();

    Ok(match extension.as_str() {
        "pdf" => "application/pdf".into(),
        "png" => "image/png".into(),
        "jpg" | "jpeg" => "image/jpeg".into(),
        "tif" | "tiff" => "image/tiff".into(),
        "txt" | "md" => "text/plain".into(),
        other => return Err(DocumentError::UnsupportedType(other.to_owned())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(filename: &str, content: Vec<u8>) -> Result<AttachedDocument, DocumentError> {
        AttachedDocument::new(
            Uuid::new_v4(),
            DocumentKind::SignedTerm,
            filename,
            content,
            "felipe",
        )
    }

    #[test]
    fn a_signed_term_records_its_digest_and_size() {
        let document = doc("termo.pdf", b"%PDF-1.7 signed".to_vec()).unwrap();
        assert_eq!(document.media_type, "application/pdf");
        assert_eq!(document.size_bytes, 15);
        assert_eq!(document.sha256.len(), 64);
        assert_eq!(document.verify(), Some(true));
        assert_eq!(document.short_digest().len(), 12);
    }

    #[test]
    fn tampering_with_the_content_fails_verification() {
        let mut document = doc("termo.pdf", b"original".to_vec()).unwrap();
        document.content = Some(b"replaced".to_vec());
        assert_eq!(document.verify(), Some(false));
    }

    #[test]
    fn an_empty_file_is_refused() {
        assert_eq!(
            doc("termo.pdf", Vec::new()).unwrap_err(),
            DocumentError::Empty
        );
    }

    #[test]
    fn an_oversized_file_is_refused_with_both_numbers() {
        let big = vec![0u8; MAX_DOCUMENT_BYTES + 1];
        match doc("termo.pdf", big).unwrap_err() {
            DocumentError::TooLarge { size, limit } => {
                assert_eq!(size, MAX_DOCUMENT_BYTES + 1);
                assert_eq!(limit, MAX_DOCUMENT_BYTES);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn only_scanner_formats_are_accepted() {
        for good in ["a.pdf", "a.PNG", "a.jpeg", "a.tiff", "a.txt"] {
            assert!(media_type_for(good).is_ok(), "{good} should be accepted");
        }
        for bad in ["a.exe", "a.docx", "a.zip", "noextension"] {
            assert!(
                matches!(media_type_for(bad), Err(DocumentError::UnsupportedType(_))),
                "{bad} should be refused"
            );
        }
    }

    #[test]
    fn a_filename_cannot_carry_a_path() {
        assert_eq!(sanitise_filename("/etc/passwd.pdf"), "passwd.pdf");
        assert_eq!(sanitise_filename("..\\..\\windows\\term.pdf"), "term.pdf");
        assert_eq!(sanitise_filename("   "), "document");
        assert_eq!(sanitise_filename("../term.pdf"), "term.pdf");
    }

    #[test]
    fn size_labels_are_readable() {
        let small = doc("a.pdf", vec![0; 2048]).unwrap();
        assert_eq!(small.size_label(), "2 KiB");
        let large = doc("a.pdf", vec![0; 3 * 1024 * 1024]).unwrap();
        assert_eq!(large.size_label(), "3.0 MiB");
    }
}
