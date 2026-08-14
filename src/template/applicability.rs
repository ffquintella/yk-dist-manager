//! Which keys a procedure may be applied to
//! (`features/bootstrap-templates.md` phase 3).
//!
//! Until now the only per-key gates were inside individual steps: the minimum
//! PIN length needs firmware 5.7, the PIV steps need the PIV application. Those
//! are the right shape for a *step* — one of eleven skips and the rest run — and
//! the wrong shape for a **procedure**. A unit that keeps two templates, one for
//! its FIPS keys and one for the rest, cannot express "this one is not for that
//! key" as a step gate: the run would start, most of it would apply, and the
//! wrong procedure would be on record against a key it was never meant for.
//!
//! So the rule lives on the template, it is checked before the run, and a key it
//! does not fit is a **refusal** rather than a set of skips.
//!
//! ## Deliberately three fields
//!
//! Firmware floor, firmware ceiling, applications required. They are what
//! `features/bootstrap-templates.md` phase 3 names, and every one of them is
//! answerable from a read this tool already does. Anything else — a form factor, a
//! FIPS state, a model name — would be a rule the operator could write and the
//! tool could not always evaluate, and an unevaluable rule is worse than none: it
//! turns into a warning nobody can clear.
//!
//! ## Unknown is not "does not apply"
//!
//! A record built from a scanned serial has no firmware and no application list.
//! A rule that treated that as failure would refuse every key entered by barcode,
//! which is one of the three supported ways of entering one. So the verdict has
//! two lists: what it **refuses**, and what it could not **read** — the same
//! distinction [`crate::device::applets::Snapshot`] draws, for the same reason.

use serde::{Deserialize, Serialize};

use super::TemplateError;

/// The applications a rule may name, in the spelling both transports produce
/// (`device::mgmt` and `device::ykman::parse_info` agree on these).
///
/// A closed list because a typo in an open one produces a rule that silently
/// matches no key ever — a refusal with no cause the operator can see.
pub const APPLICATIONS: [&str; 7] = [
    "Yubico OTP",
    "FIDO U2F",
    "FIDO2",
    "OATH",
    "PIV",
    "OpenPGP",
    "YubiHSM Auth",
];

/// The keys a procedure is for.
///
/// [`Default`] is *unrestricted*, which is what every template shipped before this
/// existed means and what the encoding writes as nothing at all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applicability {
    /// Lowest firmware this procedure may be applied to, inclusive: `5.7.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_firmware: Option<String>,
    /// Highest firmware, inclusive — for a procedure that a later firmware
    /// changed out from under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_firmware: Option<String>,
    /// Applications that must be enabled on the key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_applications: Vec<String>,
}

/// What a rule says about one key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdict {
    /// Reasons this procedure must not be applied to this key.
    pub refusals: Vec<String>,
    /// Facts the rule needed and the tool could not read.
    pub unknowns: Vec<String>,
}

impl Verdict {
    /// Nothing refuses it. An unknown does **not** make it inapplicable — it makes
    /// it unverified, which the caller shows as a warning.
    pub fn allowed(&self) -> bool {
        self.refusals.is_empty()
    }

    /// Was everything the rule asked about actually answerable?
    pub fn complete(&self) -> bool {
        self.unknowns.is_empty()
    }
}

impl Applicability {
    /// No rule at all — the state of every template that does not carry one.
    ///
    /// Used by the serialiser and by [`crate::template::signing`]: an unrestricted
    /// template is encoded exactly as it was before this field existed, so its
    /// stored bytes, its fingerprint and any signature over it are unchanged.
    pub fn is_unrestricted(&self) -> bool {
        self == &Self::default()
    }

    /// One line for a screen, or `None` when there is no rule.
    pub fn describe(&self) -> Option<String> {
        if self.is_unrestricted() {
            return None;
        }
        let mut parts = Vec::new();
        match (&self.min_firmware, &self.max_firmware) {
            (Some(min), Some(max)) => parts.push(format!("firmware {min} to {max}")),
            (Some(min), None) => parts.push(format!("firmware {min} or newer")),
            (None, Some(max)) => parts.push(format!("firmware {max} or older")),
            (None, None) => {}
        }
        if !self.requires_applications.is_empty() {
            parts.push(format!(
                "needs {}",
                self.requires_applications.join(" and ")
            ));
        }
        Some(parts.join("; "))
    }

    /// Everything that must hold before a rule can be stored.
    ///
    /// Checked at the desk rather than in front of a key, like every other part of
    /// [`crate::template::BootstrapTemplate::check`]: a firmware bound that does
    /// not parse, or bounds the wrong way round, is a rule that refuses every key
    /// and says nothing about why.
    pub fn check(&self) -> Result<(), TemplateError> {
        let parsed = |field: &'static str, value: &Option<String>| match value {
            None => Ok(None),
            Some(text) => version(text)
                .map(Some)
                .ok_or(TemplateError::BadVersionBound {
                    field,
                    value: text.clone(),
                }),
        };
        let min = parsed("min_firmware", &self.min_firmware)?;
        let max = parsed("max_firmware", &self.max_firmware)?;
        if let (Some(min), Some(max)) = (min, max)
            && min > max
        {
            return Err(TemplateError::ImpossibleVersionRange);
        }

        for application in &self.requires_applications {
            if !APPLICATIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(application.trim()))
            {
                return Err(TemplateError::UnknownApplication(application.clone()));
            }
        }
        Ok(())
    }

    /// Does this procedure apply to a key with this firmware and these enabled
    /// applications?
    ///
    /// Takes plain values rather than a device type on purpose: the rule belongs to
    /// the template, which knows nothing about transports, and the caller is the
    /// one that decides *which* application list is authoritative — the management
    /// applet's read, or the record's.
    ///
    /// An empty `firmware` or an empty `applications` means "not read". See the
    /// module note: that is an unknown, never a refusal.
    pub fn verdict(&self, firmware: &str, applications: &[String]) -> Verdict {
        let mut verdict = Verdict::default();

        if self.min_firmware.is_some() || self.max_firmware.is_some() {
            match version(firmware) {
                None => verdict.unknowns.push(format!(
                    "this procedure is limited by firmware ({}), and this key's firmware is not \
                     on record — the limit could not be checked",
                    self.describe().unwrap_or_default()
                )),
                Some(actual) => {
                    if let Some(min) = self.min_firmware.as_deref().and_then(version)
                        && actual < min
                    {
                        verdict.refusals.push(format!(
                            "this procedure needs firmware {} or newer, and this key reports {}",
                            self.min_firmware.clone().unwrap_or_default(),
                            firmware.trim()
                        ));
                    }
                    if let Some(max) = self.max_firmware.as_deref().and_then(version)
                        && actual > max
                    {
                        verdict.refusals.push(format!(
                            "this procedure is only for firmware {} or older, and this key reports \
                             {}",
                            self.max_firmware.clone().unwrap_or_default(),
                            firmware.trim()
                        ));
                    }
                }
            }
        }

        if !self.requires_applications.is_empty() {
            if applications.is_empty() {
                verdict.unknowns.push(format!(
                    "this procedure needs {}, and which applications this key has enabled was \
                     never read",
                    self.requires_applications.join(" and ")
                ));
            } else {
                for required in &self.requires_applications {
                    if !applications
                        .iter()
                        .any(|have| have.eq_ignore_ascii_case(required.trim()))
                    {
                        verdict.refusals.push(format!(
                            "this procedure needs the {} application, which is not enabled on this \
                             key",
                            required.trim()
                        ));
                    }
                }
            }
        }

        verdict
    }
}

/// `5.7.4` as a comparable triple. Every component must be a number: a firmware
/// string this cannot read is reported as unknown rather than compared as zero,
/// because zero would silently satisfy every floor.
fn version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.trim().split('.');
    let mut next = || parts.next()?.trim().parse::<u32>().ok();
    let triple = (next()?, next()?, next()?);
    parts.next().is_none().then_some(triple)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apps(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn a_template_with_no_rule_applies_to_everything_and_encodes_as_nothing() {
        // The property the signing format depends on: an unrestricted rule has to
        // be indistinguishable from the field not existing, or every template
        // signed before this feature would stop verifying.
        let rule = Applicability::default();
        assert!(rule.is_unrestricted());
        assert_eq!(rule.describe(), None);
        assert_eq!(serde_json::to_string(&rule).unwrap(), "{}");

        let verdict = rule.verdict("", &[]);
        assert!(verdict.allowed());
        assert!(verdict.complete());
    }

    #[test]
    fn a_firmware_floor_refuses_an_older_key_and_names_both_numbers() {
        let rule = Applicability {
            min_firmware: Some("5.7.0".into()),
            ..Default::default()
        };
        let verdict = rule.verdict("5.4.3", &apps(&["PIV"]));
        assert!(!verdict.allowed());
        assert!(verdict.refusals[0].contains("5.7.0"), "{:?}", verdict);
        assert!(verdict.refusals[0].contains("5.4.3"), "{:?}", verdict);

        assert!(
            rule.verdict("5.7.0", &[]).allowed(),
            "the floor is inclusive"
        );
        assert!(rule.verdict("5.7.4", &[]).allowed());
    }

    #[test]
    fn a_firmware_ceiling_refuses_a_newer_key() {
        // The case a ceiling is for: a procedure a later firmware changed out from
        // under, kept on record for the keys it was written for.
        let rule = Applicability {
            max_firmware: Some("5.4.3".into()),
            ..Default::default()
        };
        assert!(!rule.verdict("5.7.4", &[]).allowed());
        assert!(rule.verdict("5.4.3", &[]).allowed());
        assert!(rule.verdict("5.2.7", &[]).allowed());
    }

    #[test]
    fn a_firmware_that_was_never_read_is_unknown_rather_than_refused() {
        // A key entered by barcode has no firmware, and refusing it would refuse
        // one of the three supported ways of entering a serial.
        let rule = Applicability {
            min_firmware: Some("5.7.0".into()),
            ..Default::default()
        };
        let verdict = rule.verdict("", &[]);
        assert!(verdict.allowed(), "{verdict:?}");
        assert!(!verdict.complete());
        assert!(verdict.unknowns[0].contains("not on record"), "{verdict:?}");

        // And so is a firmware string this cannot parse — comparing it as zero
        // would silently satisfy every floor.
        assert!(!rule.verdict("5.7", &[]).complete());
        assert!(!rule.verdict("unknown", &[]).complete());
    }

    #[test]
    fn a_required_application_that_is_switched_off_refuses_the_procedure() {
        let rule = Applicability {
            requires_applications: apps(&["FIDO2", "PIV"]),
            ..Default::default()
        };
        let verdict = rule.verdict("5.7.4", &apps(&["FIDO2", "Yubico OTP"]));
        assert_eq!(verdict.refusals.len(), 1, "{verdict:?}");
        assert!(verdict.refusals[0].contains("PIV"), "{verdict:?}");

        assert!(
            rule.verdict("5.7.4", &apps(&["PIV", "FIDO2", "OATH"]))
                .allowed()
        );
    }

    #[test]
    fn an_application_list_that_was_never_read_is_unknown_rather_than_refused() {
        // The same mistake the pre-flight made twice, in a third place: empty is
        // "never read", not "none enabled".
        let rule = Applicability {
            requires_applications: apps(&["PIV"]),
            ..Default::default()
        };
        let verdict = rule.verdict("5.7.4", &[]);
        assert!(verdict.allowed(), "{verdict:?}");
        assert!(verdict.unknowns[0].contains("never read"), "{verdict:?}");
    }

    #[test]
    fn a_rule_is_checked_before_it_can_be_stored() {
        assert!(Applicability::default().check().is_ok());
        assert!(
            Applicability {
                min_firmware: Some("5.7.0".into()),
                max_firmware: Some("5.7.4".into()),
                requires_applications: apps(&["fido2"]),
            }
            .check()
            .is_ok(),
            "application names are matched however they are cased"
        );

        // A bound that does not parse would refuse every key and say nothing about
        // why, so it is refused at the desk instead.
        assert!(matches!(
            Applicability {
                min_firmware: Some("5.7".into()),
                ..Default::default()
            }
            .check(),
            Err(TemplateError::BadVersionBound { .. })
        ));
        // Bounds the wrong way round can never be satisfied.
        assert!(matches!(
            Applicability {
                min_firmware: Some("5.7.4".into()),
                max_firmware: Some("5.4.3".into()),
                ..Default::default()
            }
            .check(),
            Err(TemplateError::ImpossibleVersionRange)
        ));
        // A misspelled application is the same failure with a friendlier cause.
        assert!(matches!(
            Applicability {
                requires_applications: apps(&["FIDO3"]),
                ..Default::default()
            }
            .check(),
            Err(TemplateError::UnknownApplication(_))
        ));
    }

    #[test]
    fn the_description_reads_as_a_sentence_in_every_combination() {
        let describe = |min: Option<&str>, max: Option<&str>, apps: &[&str]| {
            Applicability {
                min_firmware: min.map(str::to_owned),
                max_firmware: max.map(str::to_owned),
                requires_applications: apps.iter().map(|a| (*a).to_owned()).collect(),
            }
            .describe()
        };
        assert_eq!(
            describe(Some("5.7.0"), None, &[]).as_deref(),
            Some("firmware 5.7.0 or newer")
        );
        assert_eq!(
            describe(None, Some("5.4.3"), &[]).as_deref(),
            Some("firmware 5.4.3 or older")
        );
        assert_eq!(
            describe(Some("5.4.0"), Some("5.7.4"), &[]).as_deref(),
            Some("firmware 5.4.0 to 5.7.4")
        );
        assert_eq!(
            describe(None, None, &["PIV", "FIDO2"]).as_deref(),
            Some("needs PIV and FIDO2")
        );
        assert_eq!(describe(None, None, &[]), None);
    }

    #[test]
    fn versions_compare_by_component_and_not_as_text() {
        // `5.10.0` is newer than `5.9.0`, which string comparison gets wrong.
        let rule = Applicability {
            min_firmware: Some("5.9.0".into()),
            ..Default::default()
        };
        assert!(rule.verdict("5.10.0", &[]).allowed());
        assert!(!rule.verdict("5.8.9", &[]).allowed());
    }
}
