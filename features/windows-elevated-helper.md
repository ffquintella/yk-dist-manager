# Feature: Windows elevated FIDO2 helper

## Summary

On Windows the FIDO2 applet cannot be reached from the application at all. A
background **Windows service**, installed and started by the MSI under an account
that already has the right to install software, performs the FIDO2 operations on
behalf of the non-elevated GUI over a local named pipe. Windows only: nothing here
is compiled on macOS or Linux, and nothing about their transports changes.

## Motivation

Since Windows 10 1903 the `hidclass` driver denies read/write handles to HID
devices on the FIDO usage page (`0xF1D0`) to a process that is not elevated.
Enumeration still succeeds — the device is found — and the **open** is what fails,
with `ERROR_ACCESS_DENIED`.

Two call sites in this tool open exactly that device:

* [`device::ctaphid::open`](../src/device/ctaphid.rs) — `hidapi` directly, for
  `authenticatorReset`.
* [`device::native_fido`](../src/device/native_fido.rs) — `FidoKeyHidFactory::create`,
  which is `ctap-hid-fido2` over `hidapi`, for everything else on the applet.

So on a non-elevated Windows workstation every operation on
[`write::Fido2Writer`](../src/device/write.rs) fails, and the FIDO2 third of a
factory reset fails with them. That is the FIDO2 PIN step, the minimum-length
policy, `forcePINChange`, the initial discoverable credential, operator sign-in by
security key, and the reset the pre-flight names as the only way past an
already-configured key.

**PIV and OTP are not affected**, and the reason is worth recording because it was
not designed for this: PIV goes over PC/SC, and
[`device::native_otp`](../src/device/native_otp.rs) reaches the OTP applet over
**CCID rather than HID** — a decision taken in `native-device-transport.md` phase
4a for an unrelated reason (not hand-rolling the Yubico configuration frame) which
incidentally keeps the OTP applet clear of the interface Windows guards. `ykman
otp` needs Administrator on Windows precisely because it does use that interface.

Three things make this worse than a missing capability:

1. **The fallback is blocked too.** `ykman fido reset`
   ([`device::ykman`](../src/device/ykman.rs)) needs Administrator on Windows for
   the same reason, so demoting to the subprocess transport rescues nothing.
2. **The probe cannot see it.**
   [`select::native_reachable`](../src/device/select.rs) asks PC/SC, via
   `list_serials`. PC/SC answers perfectly well unelevated, so `Native` is chosen
   and the FIDO2 steps fail at *write* time — after the PIV steps have already
   written to the key. That is the same shape as the `otp.state` failure recorded
   in `native-device-transport.md` phase 4a: a run that dies partway leaves a
   half-provisioned key, and the decision of 2026-08-13 says the only way back is a
   factory reset.
3. **The error misdiagnoses itself.** `ctaphid::open` reports *"the security key
   was found but could not be opened — another process may hold it"*. On Windows
   nothing holds it; the operating system refuses the handle. An operator following
   that message closes their browser and tries again, forever.

Linux has this problem and it is solved: `packaging/linux/70-yk-dist-manager.rules`
hands the HID interfaces to whoever is logged in at the seat, via `uaccess`.
**Windows has no per-device equivalent.** The only lever is elevation.

### Why not the fix BastionVault used

BastionVault hit the identical denial and fixed it by routing its ceremonies
through the platform WebAuthn API in `webauthn.dll`
(`WebAuthNAuthenticatorMakeCredential` / `WebAuthNAuthenticatorGetAssertion`),
which owns the device and renders the OS prompt. That does not transfer here, and
the reason is the shape of this tool rather than an implementation detail:

* `webauthn.dll` has **no reset entry point**. `authenticatorReset` is unreachable
  through it — Windows performs its own resets by a private path in Settings.
* It has **no PIN-management entry point**. `set_pin`, `change_pin`,
  `set_min_pin_length` and `force_pin_change` have no platform equivalent.
* It **collects the PIN itself**, in the OS dialog. A provisioning tool that has
  just generated a transport PIN from a template cannot supply it.

BastionVault needed exactly the two-call subset the platform API exposes. This tool
needs seven operations, five of which are not in it.

The one genuinely portable piece is **`get_assertion` for operator sign-in**
(`operator-auth-and-roles.md` phase 3): that *is* a WebAuthn-shaped ceremony, and
the operator typing their own PIN into the Windows prompt is the correct behaviour
rather than a compromise. It is recorded as phase 7 below, and it is not a
substitute for the rest.

## Current state

**Phases 1–5 are built (2026-09-08). Phase 6 — the hardware verification and the
ESI gate — is not, and phase 7 is unscheduled.**

What that means concretely: the refusal, the protocol, the service, the client, the
selection and the installer all exist and are covered by tests. **Nothing has been
run against a Windows host or a key.** The failure this feature exists to fix is
confirmed by inspection of the two call sites and of `hidclass`'s documented
behaviour, not by observation, and the service has never accepted a connection.
That is the same statement [`device::piv_session`](../src/device/piv_session.rs),
[`device::mgmt`](../src/device/mgmt.rs) and [`device::ctaphid`](../src/device/ctaphid.rs)
carry, for the same reason, and it is why phase 6 exists rather than being implied
done by a green suite.

**The Windows-only code is not merely unrun, it was compiled somewhere.** `cargo
check --target x86_64-pc-windows-msvc` cannot be run on this crate from a Mac —
`libsqlite3-sys` (`bundled`) and `mozjpeg-sys` need an MSVC C toolchain — so
[`helper::client`](../src/device/helper/client.rs),
[`helper::service`](../src/device/helper/service.rs) and `elevation`'s token query
were checked by the scratch-crate technique already used for
`WNetAddConnection2W`: a crate outside the repository with only `windows-sys`,
stand-ins for the `super::` types, and `include!` of doc-stripped copies. It earned
its keep immediately, catching four things a clean local build could not see:

* `ReadFile`, `WriteFile` and `ConnectNamedPipe` are gated on windows-sys's
  **`Win32_System_IO`** feature — they each take an `*mut OVERLAPPED` — even though
  they are declared in the file and pipe modules. Grepping for the declaration
  finds the right module and the wrong answer.
* one `Secret` construction that no longer existed, in the `ChangePin` arm;
* two `clippy::unnecessary_mut_passed` warnings, on `SetServiceStatus` and
  `CreateNamedPipeW`, which take `*const`. Those are `-D warnings` on CI's Windows
  leg, and they are precisely how `releases/v0.17.0` and `v0.17.1` both died.

`cargo clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings` is
clean on the probe.

## Design

### The shape of the decision: a service, not a prompt

The obvious alternative is to elevate on demand — `ShellExecuteW` with the `runas`
verb, a UAC prompt per operation. It is rejected because of *when* it asks. A
`runas` prompt demands administrator credentials **from the operator, at run time,
once per key, in the middle of a run whose steps they have already confirmed** — and
an operator who does not have those credentials is stopped there, holding a key that
has had its PIV steps applied.

The MSI already runs elevated: it is `Scope="perMachine"`, so installing it is
already an act performed by whoever administers the image. Spending the elevation
**once, at install, by the account that exists for the purpose** is what lets every
operator afterwards run the tool as themselves and never see a prompt. That is the
whole argument for a service, and it is also what makes withdrawing the Windows
`.zip` coherent rather than merely restrictive: the privilege is required once per
*machine*, not once per operator and not once per key.

### What the helper is, and what it is emphatically not

| | |
|---|---|
| **Does** | The seven FIDO2 operations: `fido2_state`, `set_pin`, `change_pin`, `set_min_pin_length`, `force_pin_change`, `make_credential`, `get_assertion` — plus `authenticatorReset` |
| **Does not** | PIV. OTP. Any database, settings file or audit chain. Any network. Any filesystem path taken from a request. Any subprocess. Any raw-APDU or raw-frame passthrough |

That second row is the security design, not a scope note. A `LocalSystem` process
that will do whatever a caller asks to a security key is a privilege-escalation
surface, and the only thing that keeps it small is that **the request type is a
closed enum of the eight operations named above**. There is no generic endpoint. An
eighth operation is a new variant, a new test and a deliberate decision — which is
the point.

PIV and OTP keep going direct from the GUI, because they work unelevated today and
moving them behind the service would enlarge the privileged surface to buy nothing.

### One binary, two entry points

The service is `yk-dist-manager.exe --windows-service`, not a second executable.

`diagnostics::parse_args` already owns mode flags (`--diagnose`, `--version`), so
there is a place for it. The reasons for one file rather than two:

* **Version drift.** The MSI upgrades in place. Two files means the running service
  can be a different build from the GUI that just replaced it, and a wire protocol
  with a stale peer is a class of bug that does not exist when both ends are the
  same bytes.
* **One Authenticode signature.** `packaging-and-release.md` phase 4 is already
  waiting on the procurement of one certificate. An unsigned privileged service is
  a worse thing to ship than an unsigned GUI, and two artefacts would be two
  signing steps to get right.

The protocol still carries a version and the service still refuses a mismatch,
because the MSI must stop and restart the service on upgrade and **a failure to do
that is exactly what the handshake catches**.

### The channel

A message-mode named pipe, `\\.\pipe\yk-dist-manager-fido`.

**The security descriptor is load-bearing and easy to get wrong.** A named pipe is
reachable over SMB as `\\host\pipe\name`, so a pipe created with a careless
descriptor is a remotely reachable privileged endpoint. The DACL:

* grants `NT AUTHORITY\INTERACTIVE` — a locally logged-on session, which matches
  the physical premise that somebody is standing at the machine with a key in their
  hand;
* grants `BUILTIN\Administrators`;
* **explicitly denies `NT AUTHORITY\NETWORK`**;
* grants nothing else, and in particular not `Everyone` and not `ANONYMOUS`.

**One request in flight.** The service accepts a second connection only to refuse
it. There is one key, and two callers racing an `authenticatorReset` is not a state
worth being able to reason about.

### Secrets across the boundary

Six of the eight operations carry a PIN, so AGENTS.md §2 applies to the wire:

* **Never in argv and never in the environment.** The PIN travels in the request
  message. This is the same argument `store::smb::windows` makes for using
  `WNetAddConnection2W` instead of `net use`.
* **Never to a temporary file.** The pipe is memory to memory.
* **`Debug` is redacted.** The request type prints `<redacted>` for every secret
  field, the way [`secret::Secret`](../src/secret.rs) already does, and a test
  sweeps every variant for it — the same sweep `secrets-custody.md` already
  applies to records.
* **Zeroised on both sides** after the operation, including the receive buffer,
  which is the copy that is easy to forget.

### Audit stays in the GUI

The helper writes **no audit entries**. Every mutation is still recorded by
`YkDistApp::record` in the operator's own session, as §3 requires. Moving the trail
into a `LocalSystem` service would put the immutable record behind a process the
operator's session cannot inspect and whose identity is not theirs. The helper
writes to the log only, through the one logging entry point.

### What a caller who reaches the pipe can actually do

Worth stating plainly, because it is the argument that should decide the ESI
review: **every mutation the helper performs requires a touch**, and
`authenticatorReset` additionally requires the key to have been inserted within the
last few seconds. The worst a caller who defeats the DACL can achieve is to make a
key blink at the person standing next to it.

That is the real control; the descriptor is defence in depth. It also stops being
true the moment somebody adds an operation that does not require user presence —
which is therefore the standing review question for every future variant of the
request enum, and is written here so it survives the person who knew it.

### Selection, and the refusal that has to come first

| Platform | Service answers `Ping` | FIDO2 goes | 
|---|---|---|
| Windows | yes | through the helper |
| Windows | no | direct, and `ERROR_ACCESS_DENIED` becomes `WriteError::ElevationRequired` |
| macOS, Linux | — | direct, unchanged, not compiled |

`Transport` gains no variant: the helper is not a transport the operator chooses,
it is how the native transport reaches one applet on one platform.

Nor is the answer folded into `select::Choice::describe`, though an earlier draft of
this document said it would be. That string is **audited** as
`device.transport.selected`, and it answers a different question: the transport is
the operator's choice, and this is the operating system's. They are shown side by
side instead — *Settings → Device transport* carries a `FIDO2 applet:` line under
*Now using*, with a warning callout when the applet is unreachable, and
`--diagnose` reports it as `fido2 access:`. Two lines that can disagree, which is
the point: on Windows *native, and FIDO2 unavailable* is a real and confusing state,
and one string could not say it.

The new error is deterministic, so `is_worth_retrying` is `false` for it and
`is_fatal_to_the_run` is `false` — it is the shape of `Unsupported`, not of
`Detached`.

**Phase 1 is the refusal, and it ships before any service exists.** The pre-flight
must turn "there is no elevated path to the FIDO2 applet" into a blocking finding
*before* the run starts, next to the findings `device-detection.md` phase 5 already
produces. Without it, the first Windows deployment writes a PIV PIN and a
management key to a key and then fails, and the way back is a factory reset. The
service is the fix; the refusal is what stops the fix being urgent.

### Windows ships the MSI only

The portable `.zip` is **withdrawn on Windows** (decided 2026-09-08). A service can
only be installed by an installer, so the zip would be permanently unable to reach
the FIDO2 applet — a third of the standard procedure, plus the reset that is the
only way past an already-configured key.

That is not a trim, it is the consequence of the packaging spec's own argument.
[`packaging-and-release.md`](packaging-and-release.md) justifies shipping two
artefacts per platform on the grounds that they are *"two builds with identical
capabilities"*, and warns specifically against shipping two with different ones.
This feature breaks that premise on Windows, so the pair stops being defensible
there. macOS and Linux keep both artefacts: nothing on either needs privilege the
running operator does not already have.

**What it costs, stated plainly rather than left as a footnote.** The zip existed
for the operator who cannot install software on their own machine, and on Windows
that operator is now not served — they need somebody who can run the MSI once. That
is the trade the service is buying: one elevated act at install, by the account that
already administers the image, in exchange for no elevation ever being asked of an
operator. The alternative — a script beside the zip that registers the service when
run elevated — was rejected because it is a second way to install a privileged
service, with no uninstall bookkeeping, and the MSI's `ServiceControl` exists
precisely to be the one place that is tracked.

Until the service ships, the interim answer on Windows is to run the whole
application elevated, and `docs/operations.md` now says so — along with the
consequence that is not obvious: an elevated process is a **different logon
session**, so a register on a mapped drive or on a share connected as the signed-in
user may not be reachable from it.

### The installer

`packaging/windows/Package.wxs` currently states, as a recorded decision, *"No file
association, **no service**, no PATH entry, no registry beyond what the installer
needs."* This feature reverses the middle clause, and the comment must be updated
to say why rather than quietly losing the sentence.

The MSI:

* installs the service (`ServiceInstall`), auto-start, running as the principal
  settled in phase 6;
* starts it, and **stops and restarts it on upgrade** — the case the version
  handshake exists to catch;
* **removes it on uninstall** (`ServiceControl` with `Remove="uninstall"`). A stale
  `LocalSystem` service surviving an uninstall is the worst outcome available here,
  and `verify-msi.ps1` — which already installs, interrogates and uninstalls —
  gains an assertion that the service is gone.

## Phases

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | Typed refusal and blocking pre-flight | 2 | **Done** | [`WriteError::ElevationRequired`](../src/device/write.rs), asked from the process token in [`device::elevation`](../src/device/elevation.rs) **before** the open at both call sites rather than diagnosed from `hidapi`'s message afterwards, and a blocking pre-flight finding that names the steps that cannot run and what to do about it. Silent when the plan has no FIDO2 step — refusing a PIV-only procedure over a capability it never needed would be refusing work for no reason |
| 2 | The wire protocol | 2 | **Done** | [`helper::protocol`](../src/device/helper/protocol.rs) — pure, and it runs in the ordinary suite on every platform. A closed eight-variant enum, length-prefixed frames bounded before allocation, `WireSecret` redacting its `Debug` and zeroising on drop, and `needs_user_presence` asserted per variant so the security argument cannot be quietly outgrown |
| 3 | The service | 2 | **Done** | [`helper::service`](../src/device/helper/service.rs) — the dispatcher, the pipe, `PIPE_REJECT_REMOTE_CLIENTS`, [`PIPE_SDDL`](../src/device/helper/mod.rs) and one caller at a time. The descriptor lives in `helper/mod.rs` rather than here so that it is compiled and its test run on **every** platform: a constant that only exists in a `#[cfg(windows)]` module is one nobody checks until CI's Windows leg. The privileged side **re-validates the PIN** it is handed rather than trusting the application's check |
| 4 | The client and selection | 2 | **Done** | [`helper::client`](../src/device/helper/client.rs) implements `Fido2Writer` over the pipe, and [`composite::NativeBackend::fido`](../src/device/composite.rs) is the **one** place the choice is made — seven methods route through it, because phase 4a of `native-device-transport.md` records what happens when three call sites are converted and one is missed. `--diagnose` reports the access on a `fido2 access:` line; `device.helper.selected` and `device.helper.refused` reach the trail |
| 5 | MSI, verification and docs | 2 | **Done** | `ServiceInstall` / `ServiceControl` in [`Package.wxs`](../packaging/windows/Package.wxs), `Vital="no"` so a service that will not register does not cost the operator the whole application, `Stop="both"` + `Start="install"` so an upgrade restarts it, `Remove="uninstall"`. [`verify-msi.ps1`](../packaging/windows/verify-msi.ps1) asserts it is registered, auto-start, running the installed binary with the flag — and **gone after the uninstall**. [`tests/unit_packaging.rs`](../tests/unit_packaging.rs) pins the service name and argument against the Rust constants off Windows, because nothing else links the three files but a string |
| 6 | Hardware verification and the ESI gate | 2 | Todo | On a managed Windows image with a key. Settles the one question that cannot be settled from a Mac: whether a **virtual service account** (`NT SERVICE\yk-dist-manager`) can open the FIDO interface, or whether it has to be `LocalSystem` |
| 7 | Operator sign-in through `webauthn.dll` | 3 | Todo | The one piece BastionVault's fix does transfer to: `get_assertion` for sign-in, where the OS collecting the PIN is correct rather than a compromise. Independent of the service, and not a substitute for it |

## Audit events

| Event | When |
|---|---|
| `device.helper.selected` | At startup: whether FIDO2 goes through the helper, and why. Beside `device.transport.selected`, not folded into it — an operator reporting a fault needs to say which of the two answers they have |
| `device.helper.refused` | A FIDO2 operation was refused because no elevated path exists. A refusal rather than a state change, and on the trail for the same reason `bootstrap.incomplete` is: it is the entry that explains a key that was not finished |

The helper itself appends nothing. Every step outcome is recorded by the GUI as it
is today.

## Tests

* [`tests/unit_device_helper.rs`](../tests/unit_device_helper.rs) — 16 tests: a
  round trip for every variant, a `Debug` sweep asserting no PIN text appears in
  any of them, four refusals proving the vocabulary is closed (`exec`, `apdu`,
  `read-file`, and the right verb with a missing field), the frame bound on both
  sides, every branch of `decide`, and seven assertions on the security
  descriptor's text. **Headless and cross-platform**: it runs on the developer's
  Mac and in all three CI legs.
* [`tests/behaviour_windows_helper.rs`](../tests/behaviour_windows_helper.rs) — six
  scenarios on the refusal: that it blocks, that it names the steps and both ways
  out and the cost of ignoring it, that a **PIV-only procedure is not refused**, and
  that the helper makes the procedure runnable again.

  Named `behaviour_windows_helper` and not `behaviour_app_windows_helper`, and the
  file says why: `YkDistApp` reads its answer from `elevation::access()`, which is
  derived from the running process and cached, so on this developer's machine and
  two of the three CI legs the answer is permanently `Direct` and the interesting
  branch is unreachable through the application. That is exactly why the pre-flight
  takes the access as a *field* — the decision is made once at the edge and
  everything below it is pure. The file also deliberately builds no `YkDistApp`, so
  it cannot rewrite anybody's settings (`unit_settings`' guard).
* [`tests/unit_packaging.rs`](../tests/unit_packaging.rs) — the service name and
  argument in `Package.wxs` and `verify-msi.ps1` against the Rust constants, plus
  `Stop="both"` and `Remove="uninstall"`. Read as text on any platform: nothing
  else links those three files but a string, and both failure modes are quiet —
  a rename leaves the old service registered on every upgraded machine, and a lost
  argument starts a GUI with no window station.
* The pure half of selection goes in `src/device/select.rs`'s existing unit tests,
  which already cover branches only reachable on machines this developer does not
  have.
* The Windows FFI — the service dispatcher, the pipe, the token query — is checked
  from the Mac by the scratch-crate technique, under both `cargo check` and `cargo
  clippy -- -D warnings`. See *Current state* for what that caught. There is no
  need to prove the harness is live by typoing a symbol: it failed on four real
  things first time out.
* **What no test here covers, stated rather than implied.** That the descriptor has
  the *effect* its text describes; that `ConnectNamedPipe` is actually unblocked by
  the control handler's self-connect; that a `LocalSystem` service can open the FIDO
  interface at all. Those need a Windows host, and two of them need a key. Phase 6.
* CI's Windows leg compiles the real file, as it does `store::smb::windows` today.
* **No test writes to a real key**, and no test requires the service to be
  installed. Phase 6's verification is by hand, on an image, and is recorded as
  such.

## Open questions and gates

* **ESI (AGENTS.md §8), blocking for shipping, not for building.** A privileged
  always-on service that receives PINs over local IPC is an *architecture security
  premise*, and it is not the implementer's to grant. Phases 1–5 can be built and
  tested; phase 6 is where it is put in front of the ESI, with the request enum,
  the DACL and the user-presence argument above as the material.
* **Which principal.** `LocalSystem` is what certainly works and is more privilege
  than the job needs. A virtual service account is the right answer if it can open
  the FIDO interface, and that cannot be determined from here. Phase 6.
* **Whether the service is optional in the MSI.** An image whose procedure does not
  include FIDO2 does not need it. A feature the installer can decline is cheap to
  author and is one more thing to test; deferred until somebody asks.
* **Code signing.** `packaging-and-release.md` phase 4's Authenticode certificate is
  outstanding, and matters more for a service than for a GUI.
* **The `.zip` degradation** is a documentation change with no code behind it, and
  it should not wait for phase 5.

## References

* [`src/device/ctaphid.rs`](../src/device/ctaphid.rs),
  [`src/device/native_fido.rs`](../src/device/native_fido.rs),
  [`src/device/select.rs`](../src/device/select.rs),
  [`src/device/write.rs`](../src/device/write.rs)
* [`src/store/smb/windows.rs`](../src/store/smb/windows.rs) — the shape to copy for
  a Windows-only FFI file: thin, with the decisions written above the code and the
  pure half testable everywhere
* [`packaging/windows/Package.wxs`](../packaging/windows/Package.wxs),
  [`packaging/linux/70-yk-dist-manager.rules`](../packaging/linux/70-yk-dist-manager.rules)
* [`features/native-device-transport.md`](native-device-transport.md),
  [`features/device-detection.md`](device-detection.md),
  [`features/packaging-and-release.md`](packaging-and-release.md)
