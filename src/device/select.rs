//! Which transport this session reads the hardware through
//! (`features/native-device-transport.md` phase 6).
//!
//! Until this existed, the answer was "`ykman`, always" — a line in
//! `YkDistApp::new` that no build configuration could change. So a build compiled
//! with `--features native-device`, whose FIDO2 transport is hardware-verified and
//! whose PIV read agrees with `ykman` on a real key, still shelled out to a Python
//! subprocess for every enumeration. The code was shipped and unreachable.
//!
//! ## The order, and why it is that order
//!
//! 1. **The operator's choice**, if they made one. A Settings override is the escape
//!    hatch for the case no heuristic can see: a workstation where PC/SC is being
//!    held by something else, or where the two transports disagree and somebody is
//!    diagnosing it.
//! 2. **Native**, if this build has it *and a reader answers*. Compiled-in is not
//!    enough: `native-piv` links PC/SC, and PC/SC exists on a machine with no reader
//!    driver running. Asking is one enumeration at startup.
//! 3. **`ykman`**, if it is on `PATH`.
//! 4. **Neither** — and this is a state, not a panic. The application opens, the
//!    register works, keys can be recorded by serial from a barcode or by hand, and
//!    the screens say what is missing. A tool that refused to start because no reader
//!    was attached would be useless for the half of the job that is paperwork.
//!
//! ## Why the *probe* is what decides, not the feature flag
//!
//! A feature flag says what was compiled. It cannot say whether `pcscd` is running,
//! whether the Smart Card service was disabled by policy, or whether another process
//! holds the reader. The probe answers the question actually being asked — "can this
//! transport talk to hardware on this machine, right now" — and it answers it once,
//! at startup, rather than on every read.
//!
//! The cost of being wrong in each direction is asymmetric, which is why the probe
//! is allowed to *demote* but nothing silently promotes: choosing `ykman` when native
//! would have worked costs a subprocess per read; choosing native when PC/SC is dead
//! costs every read failing until somebody restarts the application.

use super::{YkmanBackend, YubiKeyBackend};

/// Alias so `device::TransportChoice` reads as what it is at the call site.
pub use Choice as TransportChoice;

/// A transport this build could use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    /// Decide at startup, by probing. The default, and what a fresh install gets.
    #[default]
    Automatic,
    /// In-process, over PC/SC and USB HID.
    Native,
    /// The `ykman` subprocess.
    Ykman,
}

impl Transport {
    pub const ALL: [Transport; 3] = [Transport::Automatic, Transport::Native, Transport::Ykman];

    /// Words, distinct per variant, because the status bar shows this and a
    /// transport nobody can name is one nobody can report in a ticket.
    pub fn label(&self) -> &'static str {
        match self {
            Transport::Automatic => "automatic",
            Transport::Native => "native",
            Transport::Ykman => "ykman",
        }
    }

    /// The stable name used in the settings file and the audit entry.
    pub fn slug(&self) -> &'static str {
        self.label()
    }
}

/// What this build could do, and what this machine can do — the inputs to the
/// decision, separated from the decision so it can be tested without a reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Availability {
    /// Was a native transport compiled in at all?
    pub native_compiled: bool,
    /// Did a native probe reach the hardware layer? `None` when it was not asked —
    /// because the build has no native transport, or the operator chose `ykman`.
    pub native_probe: Option<bool>,
    /// Is `ykman` on `PATH`?
    pub ykman_present: bool,
}

/// Which transport a session ended up with, and why.
///
/// The reason travels with the choice because it is what the status bar and the
/// audit entry say. "native" alone does not tell an operator whether their override
/// was honoured or whether the probe happened to pick it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub transport: Transport,
    /// True when nothing can reach hardware. The register still works.
    pub disabled: bool,
    pub reason: String,
}

impl Choice {
    /// One line for the status bar and for `device.transport.selected`.
    pub fn describe(&self) -> String {
        if self.disabled {
            return format!("no device transport — {}", self.reason);
        }
        format!("{} — {}", self.transport.label(), self.reason)
    }
}

/// Decide, from what is available and what the operator asked for.
///
/// Pure: no probing, no filesystem, no subprocess. The caller does the asking and
/// hands the answers in, which is what makes every branch below a unit test.
pub fn decide(requested: Transport, available: Availability) -> Choice {
    match requested {
        // An explicit choice is honoured even when the probe disagrees — that is
        // what an override is for, and a diagnosis is impossible if the application
        // quietly overrules the person doing it. What it will not do is pretend: a
        // forced transport that cannot work is reported as forced *and* failing.
        Transport::Native => {
            if !available.native_compiled {
                return Choice {
                    transport: Transport::Ykman,
                    disabled: !available.ykman_present,
                    reason: "native was chosen in Settings, but this build has no native \
                             transport — rebuild with `--features native-device`"
                        .into(),
                };
            }
            Choice {
                transport: Transport::Native,
                disabled: false,
                reason: match available.native_probe {
                    Some(true) => "chosen in Settings".into(),
                    // Honoured, and said out loud. The operator asked for this.
                    _ => "chosen in Settings, and no reader answered the startup probe — reads \
                          will fail until one does"
                        .into(),
                },
            }
        }
        Transport::Ykman => Choice {
            transport: Transport::Ykman,
            disabled: !available.ykman_present,
            reason: if available.ykman_present {
                "chosen in Settings".into()
            } else {
                "chosen in Settings, but `ykman` is not on PATH".into()
            },
        },
        Transport::Automatic => automatic(available),
    }
}

fn automatic(available: Availability) -> Choice {
    if available.native_compiled && available.native_probe == Some(true) {
        return Choice {
            transport: Transport::Native,
            disabled: false,
            reason: "a reader answered, and this build talks to it in process".into(),
        };
    }
    if available.ykman_present {
        return Choice {
            transport: Transport::Ykman,
            disabled: false,
            reason: match (available.native_compiled, available.native_probe) {
                // The interesting case: the build *can* go native and the machine
                // cannot. Naming it is what stops somebody spending an afternoon
                // wondering why their native build spawns subprocesses.
                (true, Some(false)) => {
                    "no reader answered the native probe — is pcscd or the Smart Card service \
                     running?"
                        .into()
                }
                _ => "this build has no native transport compiled in".into(),
            },
        };
    }
    Choice {
        transport: Transport::Ykman,
        disabled: true,
        reason: match (available.native_compiled, available.native_probe) {
            (true, Some(false)) => "no reader answered, and `ykman` is not on PATH either. Keys \
                                    can still be recorded by serial; nothing can be read from \
                                    hardware"
                .into(),
            _ => "no native transport in this build and no `ykman` on PATH. Keys can still be \
                  recorded by serial; nothing can be read from hardware"
                .into(),
        },
    }
}

/// Ask the machine the two questions [`decide`] needs.
///
/// The probe is skipped when the answer cannot matter — no native transport
/// compiled, or the operator chose `ykman` — because it opens a PC/SC context, and
/// doing that on a machine where the service is stopped is a wait nobody asked for.
pub fn probe(requested: Transport) -> Availability {
    let native_compiled = cfg!(feature = "native-piv");
    let want_probe = native_compiled && requested != Transport::Ykman;

    Availability {
        native_compiled,
        native_probe: want_probe.then(native_reachable),
        // On `PATH`, without running it. The same check `--diagnose` reports, so the
        // two cannot disagree about whether the fallback exists.
        ykman_present: ykman_on_path(),
    }
}

/// Is `ykman` on `PATH`? Read-only: it looks for the file and does not run it.
fn ykman_on_path() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join("ykman").is_file())
}

/// Can the native transport reach the hardware layer at all?
///
/// **An empty reader list counts as reachable.** The question is whether PC/SC
/// answers, not whether a key is plugged in — an operator who opens the application
/// before reaching for a key would otherwise be demoted to the subprocess transport
/// for the rest of the session.
#[cfg(feature = "native-piv")]
fn native_reachable() -> bool {
    match super::NativeBackend::new().list_serials() {
        Ok(_) => true,
        Err(e) => {
            tracing::info!(
                event = "device.transport.probe_failed",
                reason = %e,
                detail = "falling back to the subprocess transport"
            );
            false
        }
    }
}

#[cfg(not(feature = "native-piv"))]
fn native_reachable() -> bool {
    false
}

/// Build the backend for a decided choice.
///
/// A disabled choice still gets a backend: `ykman`'s, which will report that the
/// binary is missing on every call. That is deliberate — the alternative is an
/// `Option<Box<dyn …>>` threaded through every caller for a state the screens
/// already describe, and a refusal that names the missing tool is more useful than
/// a `None` that names nothing.
pub fn backend_for(choice: &Choice) -> Box<dyn YubiKeyBackend> {
    match choice.transport {
        #[cfg(feature = "native-piv")]
        Transport::Native => Box::new(super::NativeBackend::new()),
        #[cfg(not(feature = "native-piv"))]
        Transport::Native => Box::new(YkmanBackend::default()),
        Transport::Ykman | Transport::Automatic => Box::new(YkmanBackend::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available(native_compiled: bool, probe: Option<bool>, ykman: bool) -> Availability {
        Availability {
            native_compiled,
            native_probe: probe,
            ykman_present: ykman,
        }
    }

    #[test]
    fn a_build_that_can_go_native_and_a_reader_that_answers_goes_native() {
        let choice = decide(Transport::Automatic, available(true, Some(true), true));
        assert_eq!(choice.transport, Transport::Native);
        assert!(!choice.disabled);
        assert!(choice.reason.contains("in process"), "{}", choice.reason);
    }

    #[test]
    fn a_native_build_whose_reader_does_not_answer_falls_back_and_says_why() {
        // The case worth naming: somebody built with `--features native-device`,
        // sees subprocesses, and needs to know it is their PC/SC service and not the
        // build. A silent fallback here is an afternoon lost.
        let choice = decide(Transport::Automatic, available(true, Some(false), true));
        assert_eq!(choice.transport, Transport::Ykman);
        assert!(!choice.disabled);
        assert!(
            choice.reason.contains("pcscd") || choice.reason.contains("Smart Card"),
            "the message has to name the service to check: {}",
            choice.reason
        );
    }

    #[test]
    fn the_default_build_uses_ykman_without_complaining_about_it() {
        // No native transport compiled in is the *default*, not a fault, and the
        // reason should not read like one.
        let choice = decide(Transport::Automatic, available(false, None, true));
        assert_eq!(choice.transport, Transport::Ykman);
        assert!(!choice.disabled);
        assert!(
            !choice.reason.contains("pcscd"),
            "a build with no native transport was never probed: {}",
            choice.reason
        );
    }

    #[test]
    fn nothing_available_is_a_state_and_not_a_failure_to_start() {
        // The application still has to open: half of this tool's job is paperwork,
        // and a register that refuses to open because no reader is attached is
        // useless for it.
        let choice = decide(Transport::Automatic, available(false, None, false));
        assert!(choice.disabled);
        assert!(
            choice.reason.contains("recorded by serial"),
            "the message has to say what still works: {}",
            choice.reason
        );
        assert!(choice.describe().starts_with("no device transport"));
    }

    #[test]
    fn an_operator_override_is_honoured_even_when_the_probe_disagrees() {
        // An override exists for the case no heuristic can see. Overruling it
        // quietly would make a diagnosis impossible — but it is reported as forced
        // *and* failing, rather than as working.
        let choice = decide(Transport::Native, available(true, Some(false), true));
        assert_eq!(choice.transport, Transport::Native);
        assert!(choice.reason.contains("Settings"), "{}", choice.reason);
        assert!(
            choice.reason.contains("will fail"),
            "an override that cannot work must not read as working: {}",
            choice.reason
        );
    }

    #[test]
    fn choosing_native_in_a_build_without_it_says_which_flag_is_missing() {
        // The refusal has to be actionable: the operator cannot fix this in
        // Settings, only by installing a different build.
        let choice = decide(Transport::Native, available(false, None, true));
        assert_eq!(choice.transport, Transport::Ykman);
        assert!(
            choice.reason.contains("native-device"),
            "name the feature to rebuild with: {}",
            choice.reason
        );
    }

    #[test]
    fn choosing_ykman_without_ykman_is_disabled_rather_than_pretending() {
        let choice = decide(Transport::Ykman, available(true, None, false));
        assert_eq!(choice.transport, Transport::Ykman);
        assert!(choice.disabled);
        assert!(choice.reason.contains("PATH"), "{}", choice.reason);
    }

    #[test]
    fn the_probe_is_not_run_when_its_answer_cannot_matter() {
        // Opening a PC/SC context on a machine whose service is stopped is a wait,
        // and it buys nothing when the operator has already chosen the subprocess.
        assert_eq!(probe(Transport::Ykman).native_probe, None);

        // And in a build with no native transport, there is nothing to probe.
        if !cfg!(feature = "native-piv") {
            assert_eq!(probe(Transport::Automatic).native_probe, None);
            assert!(!probe(Transport::Automatic).native_compiled);
        }
    }

    #[test]
    fn every_transport_has_its_own_words() {
        let labels: std::collections::BTreeSet<&str> =
            Transport::ALL.iter().map(|t| t.label()).collect();
        assert_eq!(labels.len(), Transport::ALL.len(), "{labels:?}");
        assert_eq!(Transport::default(), Transport::Automatic);
    }

    #[test]
    fn the_description_names_the_transport_and_the_reason() {
        // The status bar shows this, and a ticket quotes it: "native" alone does not
        // say whether an override was honoured or a probe happened to pick it.
        let choice = decide(Transport::Automatic, available(true, Some(true), true));
        let said = choice.describe();
        assert!(said.starts_with("native — "), "{said}");
        assert!(said.len() > "native — ".len());
    }
}
