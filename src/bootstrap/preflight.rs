//! What is checked *before* a run, so problems arrive as warnings on a screen
//! rather than as failures halfway through a key.
//!
//! `features/gui-bootstrap-wizard.md` phase 7 states the requirement precisely:
//! firmware gates, applications enabled and "this key is already configured"
//! should be **shown as skips and warnings before the run, not as failures
//! during it**. The difference matters because the run is not free to abandon —
//! a key that failed at step 7 of 11 is in a state somebody has to reason about,
//! and a key that was never started is not.
//!
//! Everything here is a pure function of the plan, the key record and the applet
//! state. It writes nothing and touches no hardware, so the wizard can call it as
//! the operator changes the template.

use crate::domain::{StepKind, YubiKeyRecord};
use crate::template::plan::{PlannedCommand, Transport};

/// How much a finding should interrupt the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// The run can proceed and this step will be skipped.
    Skip,
    /// The run can proceed, but the outcome will not be what the template says.
    Warning,
    /// The run should not start.
    Blocking,
}

impl Severity {
    /// A word, not a colour — `features/gui-shell.md` phase 10 requires that
    /// nothing carries meaning by colour alone.
    pub fn label(&self) -> &'static str {
        match self {
            Severity::Skip => "will skip",
            Severity::Warning => "warning",
            Severity::Blocking => "blocks the run",
        }
    }
}

/// One thing the operator should know before starting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    /// The step it concerns, or empty when it is about the run as a whole.
    pub step_id: String,
    pub message: String,
}

impl Finding {
    fn new(severity: Severity, step_id: &str, message: impl Into<String>) -> Self {
        Self {
            severity,
            step_id: step_id.to_owned(),
            message: message.into(),
        }
    }
}

/// What the applets currently hold, when it could be read.
///
/// One type, shared with the read side (`device::applets`), rather than a copy: the
/// pre-flight is the main consumer of that read, and two structs with the same three
/// fields would drift the moment one gained a fourth. All optional, because a key may
/// be attached with its FIDO2 applet disabled over USB, and a pre-flight that refused
/// to run without every answer would be a pre-flight nobody could use.
pub use crate::device::applets::Snapshot as AppletSnapshot;

/// Everything the checks look at.
pub struct Preflight<'a> {
    pub commands: &'a [PlannedCommand],
    pub key: Option<&'a YubiKeyRecord>,
    pub applets: &'a AppletSnapshot,
    /// True when this build has a transport that can actually write.
    pub can_write: bool,
}

impl Preflight<'_> {
    /// Every finding, most severe first.
    pub fn run(&self) -> Vec<Finding> {
        let mut findings = Vec::new();

        if !self.can_write {
            findings.push(Finding::new(
                Severity::Blocking,
                "",
                "this build has no transport that can write to a key — rebuild with \
                 `--features native-device`",
            ));
        }

        let Some(key) = self.key else {
            findings.push(Finding::new(
                Severity::Blocking,
                "",
                "no key is selected, so nothing can be checked or applied",
            ));
            return sorted(findings);
        };

        // Before anything about individual steps: has this key been through a
        // procedure already? A run that overwrites a configured key destroys the
        // identity somebody is currently relying on, so this is a refusal and not a
        // warning (`features/device-detection.md` phase 5).
        findings.extend(self.check_already_configured());

        for command in self.commands {
            findings.extend(self.check_step(command, key));
        }

        // A run that would write nothing is not a run. Counted over the steps
        // that *apply* something rather than over findings: one step can raise
        // several findings, and `verify` only reads, so it never skips and would
        // otherwise make this condition unreachable.
        let applying: Vec<&str> = self
            .commands
            .iter()
            .filter(|c| c.kind != StepKind::Verify)
            .map(|c| c.step_id.as_str())
            .collect();
        let skipped: std::collections::HashSet<&str> = findings
            .iter()
            .filter(|f| f.severity == Severity::Skip)
            .map(|f| f.step_id.as_str())
            .collect();
        if !applying.is_empty() && applying.iter().all(|id| skipped.contains(id)) {
            findings.push(Finding::new(
                Severity::Blocking,
                "",
                "every step that would write to this key is going to skip — the run would apply \
                 nothing, and recording it would claim a procedure was carried out",
            ));
        }

        sorted(findings)
    }

    /// Refuse a key that already carries a configuration
    /// (`features/device-detection.md` phase 5).
    ///
    /// **A refusal with no override, by decision (2026-08-13):** a configured key is
    /// only ever returned to factory default, and only by the system operator. There
    /// is no in-place re-bootstrap. So this does not offer a "continue anyway" —
    /// offering one would make the reset optional, and the reason the reset exists is
    /// that overwriting a live signing identity is not recoverable from the tool.
    ///
    /// The message therefore has to name the way forward, or a refusal with no exit is
    /// just an operator stuck at a screen.
    ///
    /// Silence when nothing was read is deliberate, and it is why the message says
    /// which applets *were* consulted: an unread applet is not a clean applet, and a
    /// refusal that pretended otherwise would be worse than no refusal at all.
    fn check_already_configured(&self) -> Vec<Finding> {
        let evidence = self.applets.already_configured();
        if evidence.is_empty() {
            return Vec::new();
        }
        vec![Finding::new(
            Severity::Blocking,
            "",
            format!(
                "this key has already been through a procedure — {}. A configured key is only                  ever returned to factory default, and only by the system operator: there is no                  in-place re-bootstrap, because overwriting the credential a holder is currently                  relying on cannot be undone from here. Reset the key to factory default first,                  then start the run against it.",
                evidence.join("; ")
            ),
        )]
    }

    fn check_step(&self, command: &PlannedCommand, key: &YubiKeyRecord) -> Vec<Finding> {
        let mut findings = Vec::new();
        let id = command.step_id.as_str();

        // A step nothing can perform is a manual instruction, not an automatic
        // one, and the operator has to know that before they start.
        if command.transport() == Transport::Manual {
            findings.push(Finding::new(
                Severity::Warning,
                id,
                "no transport performs this step — it has to be done by hand",
            ));
        }

        // Firmware gates. `supports_fido_min_pin_length` is the 5.7 floor the
        // domain already models, so the wizard and the executor agree.
        if command.kind == StepKind::Fido2MinPinLength && !key.supports_fido_min_pin_length() {
            findings.push(Finding::new(
                Severity::Skip,
                id,
                format!(
                    "firmware {} predates the minimum-PIN-length policy (needs 5.7)",
                    key.firmware
                ),
            ));
        }
        if command.kind == StepKind::Fido2ForcePinChange && !key.supports_fido_min_pin_length() {
            // Not a skip: the step still records the custody model, and the
            // consignment term carries the instruction instead. But the operator
            // must know the key will not enforce it.
            findings.push(Finding::new(
                Severity::Warning,
                id,
                format!(
                    "firmware {} cannot enforce the PIN change — the hand-over term is the only \
                     mechanism, so it must be signed",
                    key.firmware
                ),
            ));
        }

        // Applications the key does not have enabled.
        let applet = match command.kind {
            StepKind::Fido2Pin
            | StepKind::Fido2MinPinLength
            | StepKind::Fido2ForcePinChange
            | StepKind::Fido2Credential => Some("FIDO2"),
            StepKind::OtpAccessCode | StepKind::OtpSlotConfig => Some("OTP"),
            StepKind::PivPinPuk
            | StepKind::PivManagementKey
            | StepKind::PivKeygen
            | StepKind::PivCsr
            | StepKind::PivCertImport => Some("PIV"),
            StepKind::Verify => None,
        };
        if let Some(applet) = applet
            && !key.applications.is_empty()
            && !key
                .applications
                .iter()
                .any(|a| a.to_uppercase().contains(applet))
        {
            findings.push(Finding::new(
                Severity::Skip,
                id,
                format!("the {applet} application is not enabled on this key"),
            ));
        }

        // Already configured. These are the same questions the executor's
        // idempotency checks ask, asked early so the answer is on screen rather
        // than discovered mid-run.
        findings.extend(self.already_applied(command, id));
        findings
    }

    fn already_applied(&self, command: &PlannedCommand, id: &str) -> Vec<Finding> {
        let mut findings = Vec::new();
        match command.kind {
            StepKind::Fido2Pin => {
                if self.applets.fido2.as_ref().is_some_and(|s| s.pin_set) {
                    findings.push(Finding::new(
                        Severity::Skip,
                        id,
                        "a FIDO2 PIN is already set — it will be left alone, and the holder's \
                         existing PIN still applies",
                    ));
                }
            }
            StepKind::Fido2Credential => {
                if let Some(count) = self
                    .applets
                    .fido2
                    .as_ref()
                    .map(|s| s.resident_credentials)
                    .filter(|c| *c > 0)
                {
                    findings.push(Finding::new(
                        Severity::Skip,
                        id,
                        format!("the key already holds {count} resident credential(s)"),
                    ));
                }
            }
            StepKind::PivKeygen => {
                if self
                    .applets
                    .piv
                    .as_ref()
                    .is_some_and(|s| s.slot_occupied("9c"))
                {
                    findings.push(Finding::new(
                        Severity::Warning,
                        id,
                        "PIV slot 9c already holds a certificate — it will be left alone, so this \
                         key keeps the signing identity it already has",
                    ));
                }
            }
            StepKind::PivPinPuk => {
                if self
                    .applets
                    .piv
                    .as_ref()
                    .is_some_and(|s| s.pin_changed_from_default)
                {
                    findings.push(Finding::new(
                        Severity::Skip,
                        id,
                        "the PIV PIN is no longer the factory default — it will be left alone",
                    ));
                }
            }
            StepKind::OtpAccessCode
                if self.applets.otp.as_ref().is_some_and(|s| s.access_code_set) =>
            {
                findings.push(Finding::new(
                    Severity::Skip,
                    id,
                    "an OTP access code is already set — the slot stays as it is",
                ));
            }
            _ => {}
        }
        findings
    }
}

/// Most severe first, then by step, so the screen reads top-down in the order
/// the operator should care.
fn sorted(mut findings: Vec<Finding>) -> Vec<Finding> {
    findings.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(a.step_id.cmp(&b.step_id))
            .then(a.message.cmp(&b.message))
    });
    findings
}

/// Is there anything here that should stop the run starting?
pub fn blocks(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.severity == Severity::Blocking)
}

/// One line for the wizard's header.
pub fn summarise(findings: &[Finding]) -> String {
    if findings.is_empty() {
        return "pre-flight: nothing to flag".into();
    }
    let count = |s: Severity| findings.iter().filter(|f| f.severity == s).count();
    let (blocking, warnings, skips) = (
        count(Severity::Blocking),
        count(Severity::Warning),
        count(Severity::Skip),
    );
    let mut parts = Vec::new();
    if blocking > 0 {
        parts.push(format!("{blocking} blocking"));
    }
    if warnings > 0 {
        parts.push(format!("{warnings} warning(s)"));
    }
    if skips > 0 {
        parts.push(format!("{skips} step(s) will skip"));
    }
    format!("pre-flight: {}", parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::DeviceInfo;
    use crate::device::write::{Fido2State, OtpState, PivState};
    use crate::template::plan::plan;
    use crate::template::{BootstrapTemplate, RenderContext};

    fn key_with(firmware: &str, applications: &[&str]) -> YubiKeyRecord {
        YubiKeyRecord::from_device(&DeviceInfo {
            serial: 20_423_633,
            model: "YubiKey 5 NFC".into(),
            firmware: firmware.into(),
            form_factor: "Keychain (USB-A)".into(),
            nfc: true,
            usb_applications: applications.iter().map(|a| a.to_string()).collect(),
        })
    }

    fn commands() -> Vec<PlannedCommand> {
        let template = BootstrapTemplate::org_standard();
        plan(&template, &RenderContext::sample()).unwrap()
    }

    fn check(key: &YubiKeyRecord, applets: &AppletSnapshot) -> Vec<Finding> {
        let commands = commands();
        Preflight {
            commands: &commands,
            key: Some(key),
            applets,
            can_write: true,
        }
        .run()
    }

    #[test]
    fn a_build_with_no_write_transport_blocks_before_anything_else() {
        let commands = commands();
        let key = key_with("5.7.4", &["FIDO2", "PIV", "OTP"]);
        let findings = Preflight {
            commands: &commands,
            key: Some(&key),
            applets: &AppletSnapshot::default(),
            can_write: false,
        }
        .run();
        assert!(blocks(&findings));
        assert!(
            findings[0].message.contains("native-device"),
            "{findings:?}"
        );
    }

    #[test]
    fn no_key_selected_blocks_and_says_so() {
        let commands = commands();
        let findings = Preflight {
            commands: &commands,
            key: None,
            applets: &AppletSnapshot::default(),
            can_write: true,
        }
        .run();
        assert!(blocks(&findings));
    }

    #[test]
    fn an_old_firmware_turns_the_pin_policy_into_a_skip_not_a_failure() {
        // The point of the whole module: this arrives as a line on a screen
        // before the run, not as a failed step halfway through a key.
        let key = key_with("5.4.3", &["FIDO2", "PIV", "OTP"]);
        let findings = check(&key, &AppletSnapshot::default());

        let policy = findings
            .iter()
            .find(|f| f.step_id == "fido2-min-pin-length")
            .expect("the firmware gate is flagged");
        assert_eq!(policy.severity, Severity::Skip);
        assert!(policy.message.contains("5.7"), "{}", policy.message);
        assert!(!blocks(&findings), "an old key is still usable");
    }

    #[test]
    fn a_key_that_cannot_enforce_the_pin_change_is_a_warning_because_the_term_becomes_the_mechanism()
     {
        // Custody model B leans on the firmware where it exists and on the
        // signed term where it does not. An operator posting such a key needs to
        // know which of those they are relying on.
        let key = key_with("5.4.3", &["FIDO2", "PIV", "OTP"]);
        let findings = check(&key, &AppletSnapshot::default());
        let forced = findings
            .iter()
            .find(|f| f.step_id == "fido2-force-pin-change")
            .expect("flagged");
        assert_eq!(forced.severity, Severity::Warning);
        assert!(forced.message.contains("term"), "{}", forced.message);
    }

    #[test]
    fn a_modern_key_with_everything_enabled_has_nothing_to_flag_about_firmware() {
        let key = key_with("5.7.4", &["FIDO2", "PIV", "OTP"]);
        let findings = check(&key, &AppletSnapshot::default());
        assert!(
            !findings
                .iter()
                .any(|f| f.step_id == "fido2-min-pin-length" && f.severity == Severity::Skip),
            "{findings:?}"
        );
    }

    #[test]
    fn a_disabled_application_skips_its_steps() {
        let key = key_with("5.7.4", &["FIDO2"]);
        let findings = check(&key, &AppletSnapshot::default());
        assert!(
            findings
                .iter()
                .any(|f| f.step_id == "piv-keygen" && f.message.contains("PIV")),
            "{findings:?}"
        );
    }

    #[test]
    fn an_already_configured_key_is_reported_before_the_run_rather_than_mid_run() {
        let key = key_with("5.7.4", &["FIDO2", "PIV", "OTP"]);
        let applets = AppletSnapshot {
            fido2: Some(Fido2State {
                pin_set: true,
                resident_credentials: 1,
                ..Default::default()
            }),
            piv: Some(PivState {
                occupied_slots: vec!["9c".into()],
                pin_changed_from_default: true,
                ..Default::default()
            }),
            otp: Some(OtpState {
                access_code_set: true,
                ..Default::default()
            }),
            unread: Vec::new(),
        };
        let findings = check(&key, &applets);

        // Since phase 5, such a key does not merely produce per-step findings — the
        // run is refused outright, and the refusal names the way forward.
        let refusal = findings
            .iter()
            .find(|f| {
                f.severity == Severity::Blocking && f.message.contains("already been through")
            })
            .expect("a configured key blocks the run");
        assert!(
            refusal.message.contains("factory default"),
            "a refusal with no way forward leaves the operator stuck: {}",
            refusal.message
        );
        assert!(refusal.message.contains("9c"), "{}", refusal.message);

        for step in [
            "fido2-pin",
            "fido2-credential",
            "piv-pin-puk",
            "otp-access-code",
        ] {
            assert!(
                findings.iter().any(|f| f.step_id == step),
                "{step} should be flagged as already applied: {findings:?}"
            );
        }

        // The occupied 9c slot is a *warning*, not a skip: the key keeps a
        // signing identity the operator may not have expected it to have.
        let slot = findings
            .iter()
            .find(|f| f.step_id == "piv-keygen")
            .expect("flagged");
        assert_eq!(slot.severity, Severity::Warning);
        assert!(slot.message.contains("9c"), "{}", slot.message);
    }

    #[test]
    fn findings_are_ordered_most_severe_first() {
        let key = key_with("5.4.3", &["FIDO2"]);
        let applets = AppletSnapshot {
            fido2: Some(Fido2State {
                pin_set: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let findings = check(&key, &applets);
        let severities: Vec<Severity> = findings.iter().map(|f| f.severity).collect();
        let mut sorted_severities = severities.clone();
        sorted_severities.sort_by(|a, b| b.cmp(a));
        assert_eq!(severities, sorted_severities, "{findings:?}");
    }

    #[test]
    fn a_run_where_everything_would_skip_is_blocked_rather_than_started() {
        // Agreeing to a confirmation that applies nothing wastes a hand-over
        // slot and produces a run record claiming a procedure was attempted.
        let key = key_with("5.7.4", &["SomethingElse"]);
        let findings = check(&key, &AppletSnapshot::default());
        assert!(
            blocks(&findings),
            "every applet is disabled, so nothing can apply: {findings:?}"
        );
    }

    #[test]
    fn the_summary_counts_what_the_operator_is_about_to_agree_to() {
        let key = key_with("5.4.3", &["FIDO2", "PIV", "OTP"]);
        let findings = check(&key, &AppletSnapshot::default());
        let summary = summarise(&findings);
        assert!(summary.starts_with("pre-flight:"), "{summary}");
        assert!(
            summary.contains("skip") || summary.contains("warning"),
            "{summary}"
        );
        assert_eq!(summarise(&[]), "pre-flight: nothing to flag");
    }

    #[test]
    fn severity_reads_as_words_so_colour_is_never_the_only_signal() {
        let labels: Vec<&str> = [Severity::Skip, Severity::Warning, Severity::Blocking]
            .iter()
            .map(|s| s.label())
            .collect();
        assert_eq!(
            labels.len(),
            labels
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
    }
}
