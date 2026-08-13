# Feature: Step — PIV PIN, PUK and management key

## Summary

Leave no factory default on the PIV applet: change the PIN, change the PUK, and replace the
default management key — with a random one stored on the key itself and guarded by the PIN,
so there is nothing to hold in custody.

> **Custody model B (decided 2026-08-10):** the PIV PIN the operator sets is a transport
> PIN. **PIV has no force-change flag at any firmware level**, so the change is always
> instructed on the hand-over term and the run records
> `ChangeEnforcement::ByProcedure`. The PUK goes to the holder in the same sealed envelope
> and nothing is retained (a sub-decision still open — see below).

## Motivation

A YubiKey ships with three published PIV secrets:

| Secret | Factory default |
|---|---|
| PIN | `123456` |
| PUK | `12345678` |
| Management key (TDES) | `010203040506070801020304050607080102030405060708` |

`ykman piv info` prints `WARNING: Using default Management key!` for a reason:
anyone who can plug the key in can generate keys, import certificates and overwrite
slots. A signing certificate on a key with a default management key is not evidence
of anything.

Changing the PIN alone is not enough — the PUK resets the PIN, so an unchanged PUK
leaves the PIN's protection nominal.

## Current state

**Phases 1–4 done; 2, 3 and the AES management-key write hardware-verified on
2026-08-11.** The PIN and the PUK are changed through the [`yubikey`] crate; the
management key is set by [`device::piv_mgm`](../src/device/piv_mgm.rs) on
[`device::piv_session`](../src/device/piv_session.rs), which reads the slot's real
algorithm and speaks AES to it — the crate cannot, and the measurement that proved
that is below. Since 2026-08-13 the same session also carries on-device key
generation and certificate import, which had been failing on 5.7 for exactly the same
reason. Phases 6 and 7 (the inventory badge, and unblocking a returned key) are open.

The two plan entries:

- `StepKind::PivPinPuk` → `yubikey::YubiKey::change_pin / change_puk`
  (fallback: `ykman piv access change-pin --pin <old> --new-pin <new>`, then
  `change-puk`).
- `StepKind::PivManagementKey` → `device::piv_mgm::set_management_key`, **not**
  `yubikey::MgmKey::set_protected`, for the measured reason below
  (fallback: `ykman piv access change-management-key --algorithm aes256 --protect
  --generate --force`).

Both native operations are marked available; flags verified against ykman 5.9.2.

## Design

### PIN and PUK

- PIN: 6–8 bytes, any alphanumeric; numeric recommended for cross-platform keypads.
- Retries: 3 by default for both. Exhausting the PIN retries blocks the PIN, which
  the PUK unblocks; exhausting the PUK retries blocks the PUK, and then the applet
  can only be reset — destroying keys and certificates.
- The step must read `piv info`-equivalent state first to know the remaining retry
  counts and never spend one probing.
- Retry counts are configurable (`ykman piv access set-retries`), which also resets
  PIN and PUK to defaults — so if it is used, it must run **before** the PIN change,
  not after. That ordering trap belongs in the executor, not in the operator's head.

### Management key: prefer "protected"

Three options, in descending order of preference:

1. **`--protect --generate` (recommended)** — a random AES-256 management key is
   generated and stored on the key, encrypted under the PIN. Administrative
   operations then need only the PIN. **Nothing to escrow**, and the key is unique
   per device. This is what the default template does.
2. **Derived from a passphrase** — reproducible, but a single compromise affects
   every key derived from it.
3. **Random and escrowed** — maximum flexibility, but creates a secret store with a
   per-device value, which is the custody problem this tool would rather not have.

Algorithm: **read from the slot, not chosen.** This paragraph used to say "AES-256
where the firmware supports it (5.4+), TDES otherwise", and the hardware result below
overtook it twice: the slot takes 24 bytes, so AES-256 is not reachable at all, and
firmware 5.7 defaults the slot to AES-192 while removing 3DES. So the implementation
asks the card (`GET METADATA` on `9B`) and speaks whatever it answers — guessing the
cipher fails in a way indistinguishable from a wrong key, which is how an hour was
spent proving a good key was good. The `algorithm` template parameter below is
therefore **not** honoured today; the slot's own algorithm wins, and the run records
which it was.

Consequence of option 1 worth stating: if the PIN is blocked *and* the PUK is
blocked, the protected management key is unrecoverable and the applet must be reset.
That is acceptable — a key in that state has no usable credentials anyway — but the
operator should know it.

### Template parameters

| Parameter | Meaning | Default |
|---|---|---|
| `algorithm` | `aes256` \| `aes192` \| `aes128` \| `tdes` | `aes256` |
| `protect` | Store a random management key on the key, PIN-guarded | `true` |
| `source` (PIN/PUK) | `operator-entered` \| `holder-entered` \| `generated` | `operator-entered` (a transport PIN, under model B) |
| `set_retries` | Optional PIN/PUK retry counts | unset |

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Plan entries for PIN/PUK and management key | Done | |
| 2 | Read PIV state: retries remaining, management-key algorithm, whether protected | **Done** | `GET METADATA`, so idempotency never costs a retry. Retries and the PIN default flag through the crate; the **management** slot's algorithm and default flag through [`device::piv_session`](../src/device/piv_session.rs), because the crate's read of that slot disagreed with `ykman` after a successful change — the write was right and the read was wrong |
| 3 | Change PIN and PUK natively | **Done** | hardware-verified 2026-08-11 on a 5C NFC 5.7.4: both no longer default, counters back to 3/3. Ordering as specified — retries read, then PIN, then PUK |
| 4 | Set a protected random management key (AES-192 on current firmware) | **Done** | hardware-verified 2026-08-11, and **moved on 2026-08-13** into the shared session with no behavioural change intended — the APDUs are the same ones; the TLV parser under them now also handles multi-byte tags and long lengths, and has not been re-run against a key. [`device::piv_mgm`](../src/device/piv_mgm.rs) on [`device::piv_session`](../src/device/piv_session.rs): the slot's algorithm is read rather than assumed, the authentication is mutual, and the new key is written into the PIN-protected object so nothing is retained. **AES-256 is not reachable** and the note below says why — the slot takes 24 bytes |
| 5 | Optional escrowed management key | Todo | only with `features/secrets-custody.md` |
| 6 | Warn clearly when a default is still present | Todo | inventory badge, not just the wizard |
| 7 | PIN/PUK unblock flow for a returned key | Todo | `features/key-lifecycle-and-revocation.md` |

## Audit events

| Event | Detail (never the values) |
|---|---|
| `bootstrap.step.done` | `step=piv-pin-puk pin=changed puk=changed retries=3/3` |
| `bootstrap.step.done` | `step=piv-management-key algorithm=aes256 protected=true generated=true` |
| `bootstrap.step.failed` | `step=piv-pin-puk reason=<status> retries_after=<n>` |
| `piv.default_detected` | Phase 6: a factory default was found on a key |

## Tests

- Plan: both steps carry secret placeholders; `no_plan_output_can_leak_a_secret`
  asserts no default value (`123456`, `12345678`, `010203040506`) appears anywhere in
  a rendered plan — which also guards against someone "helpfully" pre-filling the
  factory defaults into the template.
- Phase 2+: mock PIV transport — wrong current PIN, blocked PIN, blocked PUK,
  firmware without AES support, already-protected management key.
- No test writes to real hardware.

## Open questions and gates

- ~~Who chooses the PIV PIN?~~ **Answered 2026-08-10: model B** — the operator sets a
  transport PIN and the holder changes it. Because PIV cannot enforce that, the instruction
  on the hand-over term is the only mechanism, so the term's wording is load-bearing here
  (`features/receipts-and-terms.md`).
- **The PUK** — handed to the holder (default, nothing retained) or retained for support
  (escrow, with a store to protect)? `roadmap.md` open question #5.
- Whether to change the retry counts from the default 3 at all.

## References

- `src/template/plan.rs`
- `docs/yubikey-reference.md`
- [NIST SP 800-73-4 (PIV card interface)](https://csrc.nist.gov/publications/detail/sp/800-73/4/final)

## Blocker found 2026-08-11: the crate's PIV writes are marked `untested`

Implementing this step against [`yubikey`](https://crates.io/crates/yubikey) 0.8
stopped on a dependency-quality question that is not the implementer's to settle.

**Every mutating PIV operation this step needs is behind the crate's `untested`
Cargo feature** — upstream's own name for it:

```rust
#[cfg(feature = "untested")] pub fn change_pin(...)
#[cfg(feature = "untested")] pub fn change_puk(...)
#[cfg(feature = "untested")] pub fn set_protected(...)   // MgmKey
#[cfg(feature = "untested")] pub fn set_manual(...)      // MgmKey
#[cfg(feature = "untested")] pub fn get_protected(...)   // MgmKey
```

The read paths (`piv::metadata`, `piv::Key::list`, `piv::generate`,
`Certificate::write`) are not gated, so identification, slot inspection,
on-device key generation and certificate import are all reachable from the
supported surface. It is exactly the PIN, PUK and management-key operations —
the ones whose failure mode is worst — that upstream declines to vouch for.

Why that matters more here than it would elsewhere: a management key set to a
value nobody holds leaves the PIV applet **administratively dead**, with no
recovery short of a reset that destroys the signing certificate and the key
behind it. That is the single most expensive failure this tool can cause, and
`AGENTS.md` §2 asks for the native crate specifically because it is the safer
transport. A crate feature named `untested` inverts that argument for these
calls.

### The options, and what each costs

| Option | Cost |
|---|---|
| **Enable `yubikey/untested`** behind a separate opt-in feature (e.g. `native-piv-write`), so the risk is a deliberate build-time act and appears in `--diagnose` | The code is upstream's, unexercised by its authors; we would be the ones exercising it, against real keys |
| **Drive PIV writes through `ykman`**, as the documented fallback | `ykman` is well exercised and is already a labelled transport in the plan. Costs the two things `features/native-device-transport.md` wanted to avoid: a PIN on a command line, and a Python dependency on every workstation |
| **Contribute tests upstream** and get the gate lifted | Slowest, best for everyone, and needs hardware time |

### Recommendation

Take the **`ykman` fallback** for PIN/PUK/management-key writes in the first
production release, and keep the native path behind an opt-in feature until
either upstream lifts the gate or the operations have been exercised here
against dedicated test keys. The plan the operator sees already labels a step's
transport, so a `ykman`-driven PIV step is honest rather than hidden — and the
secret-on-a-command-line objection is real but bounded, where a bricked applet
is not.

**This is an architecture premise, so it is the ESI's call** (`AGENTS.md` §8),
not the implementer's. Recorded here rather than decided.

The rest of the step — reading whether the PIN, PUK and management key are still
factory default via `GET METADATA` (firmware 5.2.3+), so idempotency never costs
a retry — is unaffected and can land either way.

## Hardware result 2026-08-11: the crate cannot do 5.7 management keys at all

`untested` was enabled deliberately and the operations were run against the test
key (5C NFC, firmware 5.7.4, serial 36668917). The result is sharper than
"untested":

| Operation | Result |
|---|---|
| `change_pin` + `change_puk` from the factory defaults | **Works.** `ykman piv info` confirms both no longer default, counters reset to 3/3 |
| `GET METADATA` idempotency read | **Works.** Correctly reported `pin_changed_from_default` flipping, with no retry burned |
| `set_management_key(protect = true)` | **Fails.** Authentication with the current management key is refused |

The cause is not a bug in the gated code. `yubikey` 0.8's `MgmKey` is
`[u8; 24]` with DES odd-parity weak-key checks — it is a **3DES** type, and its
`authenticate` sends a 3DES algorithm identifier. The attached key reports
`Management key algorithm: AES192`, because **firmware 5.7 removed 3DES and
defaults the management key to AES-192**. There is no byte value that makes a
3DES authentication succeed against an AES-192 slot.

So the crate cannot manage the management key on *any* 5.7 key, and this is a
version incompatibility rather than something a careful caller can work around.

Two further consequences worth recording:

* **AES-256 is not reachable either.** This spec and
  `features/secrets-custody.md` said "random AES-256"; the slot takes 24 bytes
  (AES-192 on current firmware) and the crate's type is 24 bytes. `MANAGEMENT_KEY_BYTES`
  is now 24, documented. Not a weakening in practice: the key is random, PIN-protected
  onto the card, never handed over and never retained.
* **A fork means vendoring.** `mod transaction` is private in the crate, so
  AES-192 `GENERAL AUTHENTICATE` cannot be added from outside — patching it means
  taking a copy of the whole crate and maintaining it.

### Options now, narrower than before

| Option | Assessment |
|---|---|
| **`ykman` for the management key step only** | Works today — `ykman piv reset` handled the AES-192 key without complaint. PIN and PUK can stay native, since those are verified working. Smallest change, and the plan already labels transports per step |
| **`yubikey` 0.9.0-pre.0** | A prerelease exists; whether it supports AES management keys is unverified. Worth ten minutes before any fork |
| **Vendor and patch 0.8** | Adds AES-192 `GENERAL AUTHENTICATE` behind the private transaction layer. Real crypto plumbing on a card protocol, and a permanent maintenance obligation |

**Recommendation: check 0.9.0-pre.0 first, and fall back to `ykman` for this one
step.** A mixed-transport step is honest — the plan shows it — and the native PIN
and PUK writes, which are the ones carrying secrets on a command line, stay
native.

## Re-implemented, and it works (2026-08-11)

Rather than vendor the crate or fall back to `ykman`, the **one broken function**
was re-implemented: [`src/device/piv_mgm.rs`](../src/device/piv_mgm.rs) speaks
`GENERAL AUTHENTICATE` to the card over its own PC/SC connection, reads the
management slot's **actual** algorithm from `GET METADATA` instead of assuming
3DES, and does the mutual-authentication exchange with AES.

Verified against the test key (5C NFC, 5.7.4, serial 36668917). After the run
`ykman piv info` no longer reports *"Using default Management key"* — nor default
PIN or PUK. The AES-192 path authenticates and writes.

Three decisions in it worth keeping:

* **The algorithm is read, never assumed.** Guessing the cipher makes every
  exchange fail in a way indistinguishable from a wrong key — which is exactly
  the trap the crate fell into, and how an hour was spent proving the key was
  fine.
* **Mutual authentication, not one-way.** The card returns our challenge
  encrypted and it is checked. A card that cannot prove it holds the current
  management key is not one to write a new key into.
* **The scope is one function.** `change_pin`, `change_puk` and the metadata
  read were measured working through the crate and stay there. This is not a
  reimplementation of PIV.

### Both remaining gaps closed 2026-08-13, and the decision that closed them

The two items recorded here as still open were the same bug seen twice, and
fixing them settled the transport question this file had been holding for the ESI
(`roadmap.md` decision log, 2026-08-13): **the exchange is written here**, rather
than choosing between a crate that cannot perform it and a CLI that takes a PIN
on its command line.

* **`generate_key` and `import_certificate` authenticated through the crate's
  `MgmKey::get_protected`**, which reads the right object and then sends a 3DES
  algorithm identifier — so both failed on any 5.7 key for the original reason,
  not for a reason of their own.

  The fix follows the protocol fact that made the first one necessary:
  **management-key authentication belongs to the card session, not to the
  process.** Authenticating correctly on one connection and then calling the crate
  on another authenticates nothing. So [`device::piv_session`](../src/device/piv_session.rs)
  is a *session* — it authenticates and then carries the two writes that need that
  authentication, `GENERATE ASYMMETRIC KEY PAIR` and `PUT DATA` of a certificate
  (the latter with command chaining, since a certificate does not fit in a short
  APDU).

* **`piv_state` reported `management_key_changed: false` after a successful
  change.** Moved to the same session's `GET METADATA` read of slot `9B`, tag
  `0x05`. This is the read the pre-flight uses to decide whether a key still
  carries a factory management key, so one that under-reports is the one that
  matters.

The scope is still deliberately narrow. `change_pin`, `change_puk`,
`piv::sign_data` and `piv::attest` were measured working through the crate and
stay there; what moved is the exchange the crate gets wrong plus the two
operations that are unreachable without it.

**Not hardware-verified.** The AES authentication and `SET MANAGEMENT KEY` were
verified on 2026-08-11 against the test key. The `GENERATE` and `PUT DATA` paths
were written with **no key attached** — stated plainly because this whole section
is about a failure that looked like success.

### Still open

* Phase 6: a key still holding a factory default should say so on the inventory
  row, not only in the wizard.
* Phase 7: the PIN/PUK unblock flow for a returned key
  (`features/key-lifecycle-and-revocation.md`).
