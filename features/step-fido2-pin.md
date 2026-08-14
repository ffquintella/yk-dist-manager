# Feature: Step — FIDO2 PIN

## Summary

Set (or change) the PIN that guards FIDO2 on the key, mark it so the holder must replace it
before first use, and where the firmware allows it, raise the minimum PIN length.

> **Custody model B (decided 2026-08-10):** the PIN the operator sets is a *transport* PIN.
> `forcePINChange` makes the key refuse to be used until the holder replaces it — on
> firmware 5.7+. Below that, the change is instructed on the hand-over term, and the run
> records which of the two applied.

## Motivation

A YubiKey out of the box has **no** FIDO2 PIN. Without one, user verification is
unavailable: the key can only assert user *presence* (a touch), so any service
requiring UV either refuses the key or silently downgrades. Setting the PIN during
bootstrap is what makes the key usable as a second factor with verification, and it
is the one step that must never be skipped.

## Current state

**Planned, not executed.** `native_op(StepKind::Fido2Pin)` points at
`ctap-hid-fido2::FidoKeyHid::set_new_pin` and is marked available; the `ykman`
fallback (`ykman fido access change-pin --new-pin <PIN>`) is in the plan as the
reference command. Nothing is applied yet — the executor is Wave 1.

## Design

### CTAP2 details that matter

- **Length**: CTAP2 requires ≥ 4 UTF-8 bytes; the key enforces up to 63. Yubico
  recommends numeric PINs for cross-platform keypad compatibility.
- **Retries**: 8 attempts. After 8 consecutive failures the FIDO2 applet is
  **blocked and can only be recovered by resetting it**, which destroys every
  credential on it. There is no PUK for FIDO2. This is why the tool must show the
  remaining retry count before and after the step, and must never "try" a PIN.
- **Setting vs changing**: `authenticatorClientPIN` has distinct
  `setPIN` and `changePIN` subcommands. Setting requires no current PIN; changing
  requires the current one. The step must read `get_info` first
  (`options.clientPin`) to know which it is, instead of guessing and burning a retry.
- **`forcePINChange`** (CTAP 2.1, firmware 5.7+): the key is marked so the *user* must
  change the PIN before first use. This is the mechanism custody model B is built on: set a
  transport PIN, mark it, hand the key over. On a pre-5.7 key the flag does not exist, so
  the same procedure runs with `ChangeEnforcement::ByProcedure` and the term carries the
  instruction — the tool must not imply an enforcement the firmware cannot provide.
- **`setMinPINLength`** (CTAP 2.1, firmware 5.7+): raises the floor. Once raised it
  **cannot be lowered** except by resetting FIDO2. Optional and firmware-gated for
  exactly that reason.
- **`alwaysUv`**: forces user verification for every operation. Out of scope for the
  default template — it breaks some services — but worth a template parameter.

### Template parameters

| Parameter | Meaning | Default |
|---|---|---|
| `min_length` | Minimum PIN length to enforce (needs 5.7+) | `6` |
| `source` | `operator-entered` \| `holder-entered` \| `generated` | `operator-entered` |
| `enforcement` (on the `fido2-force-pin-change` step) | `firmware-if-available` — use `forcePINChange` where the firmware has it, otherwise fall back to the instruction on the term | `firmware-if-available` |

### Why the native path is not optional here

`ykman fido access change-pin --new-pin 123456` puts the PIN in an argv vector.
On a shared workstation that is visible to `ps`. The native call passes it as a
function parameter, and the value is zeroised after use.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Plan entry with a secret placeholder | Done | |
| 2 | `fido2-force-pin-change` step in both built-in templates, with the firmware gate on the step | Done | custody model B made visible in the plan |
| 3 | Read `get_info`: is a PIN already set, how many retries remain | **Done** | hardware-verified; the retry count is read rather than burned |
| 4 | Set PIN over CTAP2 (`setPIN`) | **Done** | hardware-verified — see below |
| 5 | Change PIN (`changePIN`) for an already-configured key | Todo | implemented, not yet verified: needs a key whose current PIN is known |
| 6 | `forcePINChange` executed, with the pre-5.7 procedural fallback recorded | **Done** | hardware-verified on 5.7.4; **must be the last FIDO2 step** |
| 7 | `setMinPINLength` gated on firmware 5.7+ | **Done** | hardware-verified; irreversible, so the confirmation gate covers it |
| 8 | Retry-count display before and after, in the wizard | **Done** | `Fido2State::pin_retries`, read through `get_pin_retries` which spends no attempt. The pre-flight warns at two or fewer and **blocks at zero** — a locked applet fails every step that authenticates, so a confirmed run could only fail — and the warning names what recovery costs, which under model B is a reset and a new certificate. The `Verify` step reads both counters back, so a run that walked one down is on the record |

## Audit events

| Event | Detail (never the PIN) |
|---|---|
| `bootstrap.step.done` | `step=fido2-pin action=set|change retries_after=8` |
| `bootstrap.step.done` | `step=fido2-force-pin-change enforcement=enforced-by-firmware|instructed-on-handover` |
| `bootstrap.step.failed` | `step=fido2-pin reason=<ctap status> retries_after=<n>` |
| `bootstrap.step.skipped` | `step=fido2-min-pin-length reason=firmware<5.7` |
| `fido.pin.force_change_set` | Phase 6 |

## Tests

- Plan: `pin_carrying_steps_use_a_secret_placeholder`,
  `no_plan_output_can_leak_a_secret`.
- Firmware gate: `min_pin_length_is_gated_on_firmware_5_7`,
  `refresh_keeps_lifecycle_and_notes` (5.7 detection).
- Phase 2+: behaviour tests against a mock CTAP transport — PIN already set path,
  PIN not set path, wrong current PIN, retries exhausted (must refuse to continue).
- **No test writes a PIN to real hardware.** Manual verification against a dedicated
  test key is recorded in the phase notes.

## Open questions and gates

- ~~Custody~~ **answered 2026-08-10: model B**, transport PIN plus forced change, nothing
  retained. See `features/secrets-custody.md`.
- On a pre-5.7 key the enforcement is procedural. Is that acceptable for the current
  inventory, or should pre-5.7 keys be handed over with the holder present (model A) instead?
  The tool supports both; the fleet's firmware mix decides.
- Should `alwaysUv` be offered at all? It is a compatibility risk.

## References

- `src/template/plan.rs`, `src/domain/bootstrap.rs`
- `docs/yubikey-reference.md`
- [CTAP 2.1 `authenticatorClientPIN`](https://fidoalliance.org/specs/fido-v2.1-ps-20210615/fido-client-to-authenticator-protocol-v2.1-ps-20210615.html#authenticatorClientPIN)

### Hardware verification (2026-08-11)

Run against a **YubiKey 5C NFC, firmware 5.7.4, serial 36668917**, a dedicated
test key, with `examples/verify_fido2_write.rs` — the manual procedure
`features/testing-strategy.md` requires, since no *test* may write to a key. The
applet was reset to factory state before and after.

| Operation | Result |
|---|---|
| `set_pin` | `pin_set` -> true |
| `set_min_pin_length(8)` | `min_pin_length` -> `Some(8)` |
| `make_credential` (rk=true, UV) | credential `9acee661...f665` created for `example.org`, ES256 |
| `force_pin_change` | `force_pin_change_set` -> true |

Two things this settled that no mock could:

1. **`forcePINChange` really is enforced by the firmware on 5.7+.** Custody model
   B's enforcement is `enforced-by-firmware` on such a key, not merely
   `instructed-on-handover`. The specs had assumed a 5.4.3 reference key, where
   only the procedural path exists.
2. **The forced change has to be the *last* FIDO2 step.** A key marked that way
   refuses its PIN for everything except changing it, so the credential step
   failed with "PIN not accepted" until the order was corrected. The shipped
   standard procedure had the wrong order and could never have completed. See
   `features/bootstrap-engine.md` ordering rule 5.
