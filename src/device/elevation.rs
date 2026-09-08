//! Whether this process may open the FIDO2 interface at all
//! (`features/windows-elevated-helper.md` phase 1).
//!
//! Since Windows 10 1903 the `hidclass` driver refuses read/write handles to USB
//! HID devices on the FIDO usage page (`0xF1D0`) to a process that is not
//! elevated: the operating system opens those interfaces for its own WebAuthn
//! stack and hands them to nobody else. Every other platform this tool ships on
//! either has no such rule (macOS) or answers it with a permission that can be
//! granted per device (Linux's `uaccess` udev rule,
//! `packaging/linux/70-yk-dist-manager.rules`). **Windows has no per-device
//! permission to grant**, so the only lever is elevation.
//!
//! ## Why this is a check and not an error mapping
//!
//! The denial does not present as an absent device. Enumeration *succeeds* — the
//! key is found and listed — and the **open** is what fails, with
//! `ERROR_ACCESS_DENIED`. Deciding that from the error text afterwards means
//! parsing a string `hidapi` composes, and getting it wrong means telling an
//! operator that another process holds a device nothing holds. So the question is
//! asked **before** the open, from the process token, and the answer is a typed
//! refusal ([`WriteError::ElevationRequired`](super::write::WriteError)) rather
//! than a guess about somebody else's message.
//!
//! ## The three answers
//!
//! [`Fido2Access::Helper`] is the one the operator should normally get: the MSI
//! installs a service that holds the privilege, so the application itself never
//! needs it. [`Fido2Access::Direct`] is every non-Windows platform, and a Windows
//! process that happens to be elevated. [`Fido2Access::NeedsElevation`] is a
//! refusal with somewhere to go, and it exists so that the pre-flight can say so
//! **before** a run writes a PIV PIN to a key it will then fail to finish.
//!
//! ## Asked once per process
//!
//! [`access`] caches. The same property [`super::select`] already has and states:
//! a service started after the application will not be noticed until the
//! application is restarted. That is the accepted trade there — one probe at
//! startup rather than one per read — and this is the same trade for the same
//! reason. It also keeps a pipe connection out of the inner loop of a run.

use std::sync::OnceLock;

/// How this process can reach the FIDO2 applet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fido2Access {
    /// Open the HID device from this process.
    Direct,
    /// Ask the elevated helper service to do it.
    Helper,
    /// Neither is possible: Windows, not elevated, no helper answering.
    NeedsElevation,
}

impl Fido2Access {
    /// Can a FIDO2 operation be attempted at all?
    pub fn is_usable(self) -> bool {
        !matches!(self, Fido2Access::NeedsElevation)
    }

    /// One clause for the status bar, `--diagnose` and the audit entry. Distinct
    /// per variant, because an operator reporting a fault has to be able to say
    /// which of the three they have.
    pub fn describe(self) -> &'static str {
        match self {
            Fido2Access::Direct => "FIDO2 direct",
            Fido2Access::Helper => "FIDO2 via the elevated helper",
            Fido2Access::NeedsElevation => {
                "FIDO2 unavailable — Windows refuses this process a security-key handle"
            }
        }
    }
}

/// The three facts [`decide`] needs, separated from the asking so every branch is
/// a unit test on any platform — including the two branches that are only
/// reachable on a machine this developer does not have. Same split, for the same
/// reason, as [`super::select::Availability`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Availability {
    /// Is this a platform that guards the FIDO HID interface? Windows, today.
    pub guarded_platform: bool,
    /// Does this process hold an elevated token?
    pub elevated: bool,
    /// Did the helper service answer a ping?
    pub helper_answering: bool,
}

/// Decide, from what was asked. Pure: no FFI, no pipe, no cache.
pub fn decide(available: Availability) -> Fido2Access {
    if !available.guarded_platform {
        return Fido2Access::Direct;
    }
    // Elevation first, deliberately. An operator who ran the application as
    // administrator is usually the person diagnosing this machine, and routing
    // them through a service they may be about to stop would make the diagnosis
    // harder — the same argument `select::decide` makes for honouring an override.
    if available.elevated {
        return Fido2Access::Direct;
    }
    if available.helper_answering {
        return Fido2Access::Helper;
    }
    Fido2Access::NeedsElevation
}

/// Ask the machine the three questions.
pub fn probe() -> Availability {
    Availability {
        guarded_platform: cfg!(windows),
        elevated: is_elevated(),
        helper_answering: helper_answering(),
    }
}

/// The decision for this process, made once.
pub fn access() -> Fido2Access {
    static ACCESS: OnceLock<Fido2Access> = OnceLock::new();
    *ACCESS.get_or_init(|| {
        let decided = decide(probe());
        tracing::info!(
            event = "device.helper.selected",
            access = decided.describe(),
            detail = "how this process reaches the FIDO2 applet"
        );
        decided
    })
}

/// May this process open the HID device **itself**?
///
/// Not the same question as [`Fido2Access::is_usable`], and the difference is the
/// one worth having a function for: on an ordinary Windows session with the helper
/// running, the applet *is* reachable — but not from here. A caller that opened the
/// device directly in that state has bypassed
/// [`super::composite::NativeBackend`]'s one routing decision, and would fail with
/// `ERROR_ACCESS_DENIED` reported as something about another process holding the
/// device.
///
/// So that case is refused *and* logged as what it is — a routing bug rather than
/// an operator's problem. AGENTS.md §2 asks for an error to reach the log as well
/// as somewhere visible, and a refusal whose message names the wrong cause is worse
/// than a loud one.
pub fn direct_open_permitted() -> bool {
    match access() {
        Fido2Access::Direct => true,
        Fido2Access::Helper => {
            tracing::error!(
                event = "device.helper.bypassed",
                detail = "a FIDO2 operation tried to open the device directly while the elevated \
                          helper is this session's transport — the call did not go through \
                          composite::NativeBackend::fido"
            );
            false
        }
        Fido2Access::NeedsElevation => false,
    }
}

/// Does this process hold an elevated token?
///
/// `TokenElevation` rather than a group check: it answers *is this token elevated
/// right now*, which is the question `hidclass` is about to be asked. Membership
/// of `BUILTIN\Administrators` is a different question, and on a machine with UAC
/// on, the wrong one — a filtered token belongs to an administrator and is still
/// refused the handle.
#[cfg(windows)]
fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::TokenElevation;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: `token` is written only when the call reports success, is used only
    // for the one query below, and is closed on every path out.
    unsafe {
        let mut token: HANDLE = INVALID_HANDLE_VALUE;
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            // No token to ask means no elevation to claim. Reported as
            // not-elevated rather than as an error: the caller's question is
            // whether to attempt the open, and the answer on a token we cannot
            // read is the conservative one.
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

#[cfg(not(windows))]
fn is_elevated() -> bool {
    // Not "am I root": on macOS and Linux the FIDO interface is reachable by an
    // ordinary user, so elevation is not a question anything here asks. Answering
    // `false` keeps `decide` reading the same way on every platform, and
    // `guarded_platform` is what actually switches the behaviour off.
    false
}

#[cfg(windows)]
fn helper_answering() -> bool {
    super::helper::client::ping()
}

#[cfg(not(windows))]
fn helper_answering() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available(guarded: bool, elevated: bool, helper: bool) -> Availability {
        Availability {
            guarded_platform: guarded,
            elevated,
            helper_answering: helper,
        }
    }

    #[test]
    fn an_unguarded_platform_always_goes_direct() {
        // macOS and Linux, with and without a helper that could not exist there.
        assert_eq!(decide(available(false, false, false)), Fido2Access::Direct);
        assert_eq!(decide(available(false, false, true)), Fido2Access::Direct);
    }

    #[test]
    fn an_elevated_windows_process_goes_direct_even_when_the_helper_is_up() {
        assert_eq!(decide(available(true, true, true)), Fido2Access::Direct);
    }

    #[test]
    fn an_ordinary_windows_process_goes_through_the_helper() {
        assert_eq!(decide(available(true, false, true)), Fido2Access::Helper);
    }

    #[test]
    fn windows_with_neither_is_a_refusal_rather_than_an_attempt() {
        // The branch the pre-flight exists to catch: attempting the open here is
        // what leaves a key half-configured.
        let decided = decide(available(true, false, false));
        assert_eq!(decided, Fido2Access::NeedsElevation);
        assert!(!decided.is_usable());
    }

    #[test]
    fn every_variant_describes_itself_differently() {
        let described = [
            Fido2Access::Direct.describe(),
            Fido2Access::Helper.describe(),
            Fido2Access::NeedsElevation.describe(),
        ];
        for (i, a) in described.iter().enumerate() {
            for b in described.iter().skip(i + 1) {
                assert_ne!(a, b, "two accesses describe themselves the same way");
            }
        }
    }

    #[test]
    fn a_direct_open_is_only_permitted_when_this_process_holds_the_handle() {
        // The distinction the function exists for: `Helper` means the applet is
        // usable and a direct open from *here* is still a routing bug.
        assert_eq!(direct_open_permitted(), access() == Fido2Access::Direct);
        assert!(Fido2Access::Helper.is_usable());
    }

    #[test]
    fn the_probe_agrees_with_the_platform_it_is_running_on() {
        assert_eq!(probe().guarded_platform, cfg!(windows));
        // And the cached answer is the same one `decide` gives for that probe.
        assert_eq!(access(), decide(probe()));
    }
}
