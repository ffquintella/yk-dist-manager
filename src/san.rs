//! The certificate's Subject Alternative Name: what goes in it, and how to
//! check what came back.
//!
//! The signing certificate must carry the holder's e-mail as an **`rfc822Name`
//! SAN**. That is the whole point of the step
//! (`features/step-piv-signing-certificate.md`): the subject DN names a person,
//! but it is the SAN a mail client and a document signer actually match against,
//! so a certificate without it is issued, valid, and useless for the job.
//!
//! ## Why this is configurable rather than hard-coded
//!
//! Roadmap open question 1 — *which CA issues the certificate?* — is still open,
//! and the answer changes where the SAN comes from:
//!
//! * An **internal CA** takes our CSR, so the SAN has to be in the request we
//!   build.
//! * An **enterprise CA with a certificate profile** often ignores the SAN in the
//!   request and injects its own, from a directory lookup keyed on the subject.
//! * Some CAs take the SAN as a **separate request attribute** rather than an
//!   extension inside the CSR.
//!
//! A tool that hard-codes one of those cannot be pointed at the other two. So the
//! SAN is a *pattern* the deployment sets ([`SanPolicy`]), rendered from the same
//! variables the templates use, and the deployment also says whether the tool
//! supplies it or the CA is expected to
//! ([`SanSource`]). That way the CA decision, when it comes, is a settings change
//! rather than a release — and until it comes, the tool can say plainly which
//! assumption it is running under.
//!
//! ## And why the verification half matters as much
//!
//! Every one of those routes can silently produce a certificate with the wrong
//! SAN, or none. [`extraction_guide`] is the operator-facing instructions for
//! checking, with a worked example, because "we assumed the CA would add it" is
//! exactly how a batch of unusable keys gets handed out.

use serde::{Deserialize, Serialize};

/// Who is expected to put the SAN in the certificate.
///
/// Recorded rather than assumed: it decides what the tool does, and it is the
/// thing an operator most needs stated when a certificate comes back wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SanSource {
    /// This tool puts the SAN in the CSR it builds. Needs a CA that honours it.
    #[default]
    Request,
    /// The CA's certificate profile injects the SAN; whatever the request says
    /// is ignored. The tool still renders the expected value, so the issued
    /// certificate can be checked against it.
    CaProfile,
    /// The SAN travels beside the CSR — a form field, an API parameter, a ticket.
    /// The tool renders it for the operator to paste.
    OutOfBand,
}

impl SanSource {
    pub const ALL: [SanSource; 3] = [
        SanSource::Request,
        SanSource::CaProfile,
        SanSource::OutOfBand,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            SanSource::Request => "this tool puts it in the CSR",
            SanSource::CaProfile => "the CA's profile injects it",
            SanSource::OutOfBand => "supplied to the CA separately",
        }
    }

    /// The stored spelling, for settings and audit entries.
    pub fn slug(&self) -> &'static str {
        match self {
            SanSource::Request => "request",
            SanSource::CaProfile => "ca-profile",
            SanSource::OutOfBand => "out-of-band",
        }
    }

    /// What the operator has to do differently, in one line.
    pub fn operator_note(&self) -> &'static str {
        match self {
            SanSource::Request => {
                "The CSR carries the SAN. Check the issued certificate anyway — a CA that \
                 does not honour requested SANs will drop it without saying so."
            }
            SanSource::CaProfile => {
                "The CA is expected to add the SAN from its own records. The value below is \
                 what it should produce; if the issued certificate differs, the CA's profile \
                 is keyed on something other than this holder."
            }
            SanSource::OutOfBand => {
                "Copy the value below into the CA's request form or ticket. It is not in the \
                 CSR, so nothing else will carry it."
            }
        }
    }
}

/// How the SAN is produced for a given holder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SanPolicy {
    /// The pattern, rendered with the same variables as a bootstrap template.
    ///
    /// Defaults to the holder's e-mail, which is what
    /// `features/step-piv-signing-certificate.md` requires. A deployment whose CA
    /// wants a different form — a UPN, an alias domain — changes it here rather
    /// than editing a template step.
    pub pattern: String,
    /// Who puts it in the certificate.
    pub source: SanSource,
}

impl Default for SanPolicy {
    fn default() -> Self {
        Self {
            pattern: "{{holder.email}}".into(),
            source: SanSource::default(),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SanError {
    #[error(
        "the SAN pattern is empty — a signing certificate without an rfc822Name is not usable for signing"
    )]
    Empty,
    #[error("the SAN rendered to `{0}`, which is not an e-mail address")]
    NotAnEmail(String),
    #[error("the SAN pattern could not be rendered: {0}")]
    Render(String),
}

impl SanPolicy {
    /// Render the SAN for one holder, and check the result is usable.
    ///
    /// Validated at render time rather than at configuration time because the
    /// pattern is only wrong for *some* holders: `{{holder.email}}` is fine until
    /// it meets a holder record with no e-mail.
    pub fn render(&self, ctx: &crate::template::RenderContext) -> Result<String, SanError> {
        if self.pattern.trim().is_empty() {
            return Err(SanError::Empty);
        }
        let rendered = crate::template::render(&self.pattern, ctx)
            .map_err(|e| SanError::Render(e.to_string()))?;
        let rendered = rendered.trim().to_owned();

        if rendered.is_empty() {
            return Err(SanError::Empty);
        }
        // Deliberately shallow: an `rfc822Name` is an addr-spec, and this is not
        // the place to re-litigate e-mail validation — `domain::Holder` already
        // validated the address this usually renders from. What is checked is
        // the failure that actually happens: a pattern that rendered to a name,
        // a DN fragment or a leftover placeholder.
        if !looks_like_an_email(&rendered) {
            return Err(SanError::NotAnEmail(rendered));
        }
        Ok(rendered)
    }

    /// Does the tool put this in the CSR it builds?
    pub fn tool_supplies_it(&self) -> bool {
        self.source == SanSource::Request
    }
}

fn looks_like_an_email(value: &str) -> bool {
    let mut parts = value.splitn(2, '@');
    let (local, domain) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    !local.is_empty()
        && !domain.is_empty()
        && domain.contains('.')
        && !value.contains(char::is_whitespace)
        && !value.contains("{{")
}

/// Operator-facing instructions for finding and extracting the SAN from a
/// certificate, with a worked example.
///
/// Lives in the library rather than only in `docs/` so the Bootstrap and
/// Settings screens can show it at the moment it is needed. The failure it
/// guards against is the quiet one: a certificate that installs cleanly, looks
/// right in every UI, and cannot sign as the holder because the `rfc822Name` is
/// absent or wrong.
pub fn extraction_guide(expected: &str) -> String {
    format!(
        "How to check the SAN on an issued certificate\n\
         =============================================\n\
         \n\
         The certificate must carry the holder's e-mail as an rfc822Name in the\n\
         Subject Alternative Name extension. The subject DN is NOT enough: mail\n\
         clients and document signers match on the SAN, so a certificate without\n\
         it is valid, installable, and unusable for signing.\n\
         \n\
         Expected for this holder:\n\
         \n\
             {expected}\n\
         \n\
         From a certificate file\n\
         -----------------------\n\
         \n\
             openssl x509 -in holder.crt -noout -ext subjectAltName\n\
         \n\
         Expected output:\n\
         \n\
             X509v3 Subject Alternative Name:\n\
                 email:{expected}\n\
         \n\
         Read it as: the `email:` prefix is the rfc822Name. `DNS:`, `URI:` or\n\
         `otherName:` in its place means the CA issued a different kind of SAN,\n\
         and the certificate will not sign as this holder.\n\
         \n\
         Straight from the key, without a file\n\
         -------------------------------------\n\
         \n\
             ykman piv certificates export 9c - | \\\n\
                 openssl x509 -noout -ext subjectAltName\n\
         \n\
         From the CSR, before it goes to the CA\n\
         --------------------------------------\n\
         \n\
             openssl req -in holder.csr -noout -verify -text | grep -A1 'Alternative'\n\
         \n\
         What each failure looks like\n\
         ----------------------------\n\
         \n\
         * No output at all, or `No extensions`\n\
               -> there is no SAN. If the CA was expected to inject one, its\n\
                  profile is not keyed on this holder. If the CSR was expected to\n\
                  carry it, the request was built without it.\n\
         * `email:` present but a different address\n\
               -> the CA is filling the SAN from its own directory, and that\n\
                  directory disagrees with this register. Fix the directory or\n\
                  the holder record before issuing more.\n\
         * `DNS:` or `otherName:` instead of `email:`\n\
               -> the wrong SAN type. A UPN (otherName 1.3.6.1.4.1.311.20.2.3)\n\
                  is common on enterprise CAs and is not a substitute.\n\
         \n\
         Worked example\n\
         --------------\n\
         \n\
             $ openssl x509 -in ana.crt -noout -subject -ext subjectAltName\n\
             subject=CN=Ana Silva, OU=ESI, O=Example Org\n\
             X509v3 Subject Alternative Name:\n\
                 email:ana.silva@example.org\n\
         \n\
         Subject names the person; SAN is what signing matches on. Both should\n\
         agree, and the SAN is the one that has to be right."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Holder;
    use crate::template::RenderContext;

    fn ctx() -> RenderContext {
        let holder = Holder::new("Ana Silva", "ana.silva@example.org", "ESI", "").unwrap();
        RenderContext::for_holder(&holder, 20_423_633, "felipe", "Example Org")
    }

    #[test]
    fn the_default_is_the_holders_email() {
        let policy = SanPolicy::default();
        assert_eq!(policy.render(&ctx()).unwrap(), "ana.silva@example.org");
        assert!(
            policy.tool_supplies_it(),
            "the default assumes our CSR carries it"
        );
    }

    #[test]
    fn a_deployment_can_rewrite_the_san_for_its_own_ca() {
        // The case this exists for: a CA that wants an alias domain rather than
        // the address the register holds.
        let policy = SanPolicy {
            pattern: "{{holder.name}}@signing.example.org".into(),
            source: SanSource::CaProfile,
        };
        // The name has a space, so this particular pattern is refused — which is
        // the point of validating at render time.
        assert!(matches!(
            policy.render(&ctx()),
            Err(SanError::NotAnEmail(_))
        ));
    }

    #[test]
    fn an_empty_pattern_is_refused_because_the_certificate_would_be_useless() {
        let policy = SanPolicy {
            pattern: "   ".into(),
            ..Default::default()
        };
        assert_eq!(policy.render(&ctx()), Err(SanError::Empty));
    }

    #[test]
    fn a_pattern_that_renders_to_something_that_is_not_an_address_is_refused() {
        for pattern in ["{{holder.name}}", "{{org}}", "CN={{holder.name}}"] {
            let policy = SanPolicy {
                pattern: pattern.into(),
                ..Default::default()
            };
            assert!(
                matches!(policy.render(&ctx()), Err(SanError::NotAnEmail(_))),
                "`{pattern}` should not pass as an rfc822Name"
            );
        }
    }

    #[test]
    fn an_unknown_variable_is_reported_rather_than_left_in_the_san() {
        let policy = SanPolicy {
            pattern: "{{holder.nickname}}".into(),
            ..Default::default()
        };
        assert!(matches!(policy.render(&ctx()), Err(SanError::Render(_))));
    }

    #[test]
    fn each_source_tells_the_operator_something_different_to_do() {
        let notes: Vec<&str> = SanSource::ALL.iter().map(|s| s.operator_note()).collect();
        assert_eq!(
            notes.len(),
            notes.iter().collect::<std::collections::HashSet<_>>().len(),
            "three sources, three distinct instructions"
        );
        assert!(SanSource::OutOfBand.operator_note().contains("Copy"));
        assert!(
            SanSource::CaProfile
                .operator_note()
                .contains("its own records")
        );
    }

    #[test]
    fn the_guide_shows_the_expected_value_and_the_command_that_reveals_it() {
        let guide = extraction_guide("ana.silva@example.org");
        assert!(guide.contains("ana.silva@example.org"));
        assert!(guide.contains("openssl x509"));
        assert!(guide.contains("subjectAltName"));
        assert!(
            guide.contains("email:"),
            "the rfc822Name prefix is the thing to look for"
        );
        assert!(
            guide.contains("otherName"),
            "a UPN is the most common wrong answer and has to be named"
        );
        assert!(
            guide.contains("ykman piv certificates export"),
            "reading it off the key needs no file"
        );
    }
}
