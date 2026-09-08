//! The elevated FIDO2 helper: what it says, who says it, and who listens
//! (`features/windows-elevated-helper.md`).
//!
//! Windows refuses a process that is not elevated any handle to the FIDO2
//! interface — see [`super::elevation`] for the mechanism and for why this tool
//! cannot go round it. The answer is a small **Windows service**, installed and
//! started by the MSI under an account that already has the right to install
//! software, which performs the FIDO2 operations on behalf of the ordinary,
//! non-elevated application.
//!
//! ```text
//!   yk-dist-manager.exe            \\.\pipe\yk-dist-manager-fido       yk-dist-manager.exe
//!   (the operator's session)  ───────────── Request ─────────────►     --windows-service
//!                             ◄──────────── Response ────────────      (LocalSystem)
//!                                                                             │
//!                                                                      native_fido / ctaphid
//!                                                                             │
//!                                                                        the USB HID device
//! ```
//!
//! ## One binary, two entry points
//!
//! The service is this same executable started with `--windows-service`, not a
//! second file. Two reasons, both of them about failure rather than tidiness:
//!
//! * **Version drift.** The MSI upgrades in place. Two files means the running
//!   service can be a different build from the application that just replaced it,
//!   and a wire protocol with a stale peer is a class of bug that does not exist
//!   when both ends are the same bytes. [`protocol::PROTOCOL_VERSION`] still
//!   exists, for the one case that survives this: an upgrade that replaced the
//!   files and failed to restart the service.
//! * **One Authenticode signature.** `features/packaging-and-release.md` phase 4
//!   is already waiting on the procurement of one certificate, and an unsigned
//!   privileged service is a worse thing to ship than an unsigned GUI.
//!
//! ## What keeps a `LocalSystem` service from being a way in
//!
//! Four things, and they are worth naming together because each is load-bearing:
//!
//! 1. **A closed request enum.** [`protocol::Request`] is the entire vocabulary:
//!    eight operations, no passthrough, no path, no command line. The helper
//!    cannot be asked to touch the database, the settings, the audit chain, the
//!    network or the PIV and OTP applets.
//! 2. **A security descriptor that excludes the network.** A named pipe is
//!    reachable as `\\host\pipe\name` over SMB, so [`service::PIPE_SDDL`] denies
//!    `NETWORK` and `ANONYMOUS` outright and admits only an interactive logon
//!    session and administrators.
//! 3. **One request in flight.** There is one key; two callers racing an
//!    `authenticatorReset` is not a state worth being able to reason about.
//! 4. **User presence.** Every mutating operation needs the operator to touch the
//!    key, and a reset needs it re-inserted within seconds of power-up. The worst
//!    a caller who defeats the first three achieves is making a key blink at
//!    whoever is standing next to it. That is the real control, and
//!    [`protocol::Request::needs_user_presence`] is where it is asserted so that
//!    it cannot be quietly outgrown.
//!
//! ## The audit trail does not move
//!
//! The helper appends nothing. Every mutation is still recorded by
//! `YkDistApp::record` in the operator's own session, as AGENTS.md §3 requires:
//! putting the immutable trail behind a `LocalSystem` process would make it a
//! record the operator's session cannot inspect, written under an identity that is
//! not theirs. The helper writes to the log, through the one logging entry point,
//! and nothing else.

pub mod protocol;

#[cfg(windows)]
pub mod client;
#[cfg(windows)]
pub mod service;

/// The pipe both ends name.
///
/// A constant rather than a setting: a configurable path would be one more thing
/// an attacker could point at, and one more thing to get wrong on an upgrade.
pub const PIPE_NAME: &str = r"\\.\pipe\yk-dist-manager-fido";

/// The name the service is registered under, and what `sc query` answers to.
pub const SERVICE_NAME: &str = "YkDistManagerFido";

/// The argument that turns this executable into the service.
pub const SERVICE_ARG: &str = "--windows-service";

/// Who may open the pipe.
///
/// Here rather than in [`service`] so that it is compiled — and its test run — on
/// every platform. It is the one line in this feature where a mistake is remotely
/// exploitable, and a constant that only exists in a `#[cfg(windows)]` module is a
/// constant nobody checks until CI's Windows leg.
///
/// * `O:SYG:SY` — owned by SYSTEM, so an ordinary user cannot rewrite the DACL.
/// * `D:P` — protected: nothing is inherited into it.
/// * `(D;;GA;;;AN)` and `(D;;GA;;;NU)` — deny `ANONYMOUS` and `NETWORK`. Deny
///   entries first, which is both the convention and the order Windows evaluates.
///   The `NETWORK` denial is what stops the pipe being reachable as
///   `\\host\pipe\name` over SMB; `PIPE_REJECT_REMOTE_CLIENTS` on the pipe itself
///   is the second, independent defence against the same mistake.
/// * `(A;;GRGW;;;IU)` — an **interactive** logon session may read and write. Not
///   `Everyone`, and not `Users`: the premise of this whole feature is that
///   somebody is standing at the machine with a key in their hand, and a service
///   account logged on over the network is not that person.
/// * `(A;;GA;;;BA)` — administrators, who could grant themselves this anyway.
pub const PIPE_SDDL: &str = "O:SYG:SYD:P(D;;GA;;;AN)(D;;GA;;;NU)(A;;GRGW;;;IU)(A;;GA;;;BA)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pipe_is_local_only_by_name() {
        // `\\.\pipe\…` is this machine. A name beginning `\\somehost\` would be a
        // pipe on another machine, which is not a thing this tool should ever open
        // and not a thing the service should ever create.
        assert!(PIPE_NAME.starts_with(r"\\.\pipe\"));
    }

    #[test]
    fn the_descriptor_denies_the_network_before_it_allows_anybody() {
        // The mistake this feature could make that is exploitable from another
        // machine: a privileged pipe reachable as \\host\pipe\name. Deny entries
        // must come first, and `Everyone` (WD) must appear nowhere.
        let deny_anonymous = PIPE_SDDL.find("(D;;GA;;;AN)").expect("denies ANONYMOUS");
        let deny_network = PIPE_SDDL.find("(D;;GA;;;NU)").expect("denies NETWORK");
        let allow_interactive = PIPE_SDDL
            .find("(A;;GRGW;;;IU)")
            .expect("allows INTERACTIVE");
        let allow_admins = PIPE_SDDL
            .find("(A;;GA;;;BA)")
            .expect("allows Administrators");

        assert!(deny_anonymous < allow_interactive);
        assert!(deny_network < allow_interactive);
        assert!(deny_network < allow_admins);
        assert!(
            !PIPE_SDDL.contains(";WD)"),
            "the descriptor admits Everyone"
        );
        assert!(
            PIPE_SDDL.starts_with("O:SYG:SYD:P"),
            "not owned by SYSTEM, or not a protected DACL"
        );
    }

    #[test]
    fn the_descriptor_grants_the_interactive_user_no_more_than_read_and_write() {
        // `GA` (all access) for an interactive user would include WRITE_DAC, which
        // is the right to rewrite this descriptor.
        assert!(!PIPE_SDDL.contains("(A;;GA;;;IU)"));
    }

    #[test]
    fn the_service_argument_looks_like_a_flag() {
        // `diagnostics::parse_args` refuses an unknown leading-dash argument, so a
        // service arg that did not start with one would be silently treated as
        // nothing and the service would start a GUI with no window station.
        assert!(SERVICE_ARG.starts_with("--"));
    }
}
