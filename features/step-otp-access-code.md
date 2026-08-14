# Feature: Step — OTP slot access code (and slot configuration)

## Summary

Write the 6-byte access code that write-protects a Yubico OTP slot, so the slot's
configuration cannot be overwritten by anyone who plugs the key in. Optionally
program the slot itself.

## Motivation

The OTP application has two keyboard slots (short touch, long touch). By default
**anyone with physical access can reprogram them** — no PIN, no confirmation. A key
left on a desk can have slot 1 replaced with a static password that types whatever
the attacker chose, and the holder will not notice until it misbehaves.

The access code is the "PIN for OTP" in the request: a 6-byte value required to
change a protected slot's configuration. It is the only protection the OTP applet
has.

## Current state

**Planned, on the `ykman` fallback.** `native_op(StepKind::OtpAccessCode)` names
`hidapi` with `available: false`, because **no Rust crate implements the Yubico OTP
configuration protocol**. The plan therefore shows this step as `ykman (fallback)`:

```
ykman --device <serial> otp settings 1 --new-access-code <OTP-ACCESS-CODE> --force
```

(flags verified against ykman 5.9.2).

## Design

### Access-code facts that shape the code

- Exactly **6 bytes**, supplied as 12 hex characters.
- Set per slot. Slot 1 and slot 2 have independent codes.
- Required to change *or delete* a protected slot's configuration. Lose it and the
  slot is frozen — the only recovery is a full OTP applet reset via the
  configuration protocol, which also clears both slots.
- **A protected slot blocks mode switching**: you cannot change the enabled USB
  interfaces while a slot has an access code set. That surprises people later, so the
  wizard must say it.
- `ykman otp --access-code -` prompts instead of taking the value on the command
  line; the executor must use that form on the fallback path so the code never
  appears in argv.

### Native implementation (Phase 3)

The OTP applet is configured with HID feature reports: a 7-byte frame protocol
writing a fixed-size configuration structure (`YKP_CONFIG`) with a CRC, using
sequence-numbered writes and a status read to confirm. It is well documented by
Yubico's `yubikey-personalization` and reimplementable over `hidapi`, but it is
genuinely fiddly: a malformed frame can leave a slot in an unexpected state.

Sequencing: implement the **read** path first (slot status, configured/empty,
sequence number), verify it against `ykman otp info` on real keys, and only then
implement writes.

### Template parameters

| Parameter | Meaning | Default |
|---|---|---|
| `slot` | `1` or `2` | `1` |
| `source` | `generated` \| `operator-entered` | `generated` |
| `program` | Optional slot content: `chalresp` \| `yubiotp` \| `static` \| `none` | `none` in the default template |

The default template protects slot 1 but programs nothing: a challenge-response
credential is only useful if something consumes it (for example unlocking this
tool's own database — see `features/db-password-and-encryption.md` Phase 7).

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Plan entry with a secret placeholder and the fallback command | Done | |
| 1b | Custody default under model B: generate and discard | Done (default taken) | the holder never needs this code, so the slot is deliberately frozen; confirmation pending — `roadmap.md` #6 |
| 2 | Executor on the `ykman` path using the prompt form (`--access-code -`) | **Done** | [`ykman::set_otp_access_code`](../src/device/ykman.rs) runs `otp settings <slot> --force --new-access-code -` with the code on **stdin**, never in argv, where every process on the workstation can read it (`run_with_stdin`, which logs the arguments and never the input). Three facts read out of `ykman` 5.9.2's own source rather than guessed, each of which decides whether it works at all: the prompt **confirms**, so the code goes in twice; `settings` refuses an **empty** slot, so the step checks that first and explains it; and the write **resets the slot's other settings to their defaults**, which the preview says. The plan already labelled these steps `ykman (fallback)`, so what the operator confirms is what runs — **the code is complete and the protocol conversation is unverified against a key**; the pure half is covered by tests |
| 3 | Native read path over `hidapi` (slot status, sequence) | Todo — **the `ykman` read landed instead** | `device::ykman::parse_otp_info` answers "which slots are programmed" today, unit-tested against recorded output, and feeds the pre-flight refusal. The *native* frame is still unwritten. Note what neither path can answer: **whether a slot carries an access code** — nothing reports it, so no read ever claims one |
| 4 | Native write path (access code) | Todo — **blocked on hardware, deliberately** | frame + CRC + status confirmation, hand-rolled because no crate exposes it. Not written without a key to verify against: the failure mode of a wrong frame is a slot write-protected by a code nobody holds, which is unrecoverable from this tool |
| 5 | Optional slot programming (challenge-response) | Todo | for local unlock use cases |
| 6 | Warn that a protected slot blocks interface mode switching | **Done** | a pre-flight warning on the step, alongside the settings reset the write performs, and repeated in the step's own detail. Under model B it ends with the mitigation rather than the problem: the code travels to the holder on the sealed slip (2026-08-11), so the slot can still be reprogrammed later |
| 7 | Record custody of the access code | **Done** | `secret.custody` per write — step, slot, `custody=sealed-envelope`, `retained=no`. Where it went, never the value (AGENTS.md §2) |

## Open: as specified, this step can never run on a real key (2026-08-14)

Writing the access-code path made a contradiction visible that was invisible while the
write was unimplemented, and it is **not the implementer's to resolve** — it turns on
the owner's answer of 2026-08-13 about what counts as an already-configured key.

An access code write-protects a **configuration**. `ykman otp settings` refuses an
empty slot, and the applet has nothing to protect on one. So the step needs OTP slot 1
to hold something. But a programmed OTP slot is one of the three signals
[`device-detection.md`](device-detection.md) phase 5 treats as evidence that a key has
already been through a procedure, and that refusal is **blocking with no override**.
The two rules meet like this:

| The key's slot 1 | Pre-flight | This step |
|---|---|---|
| empty | run proceeds | **skips** — nothing to protect |
| programmed | **run refused** | never reached |

Either way the step does not apply, so `org-standard`'s `otp-access-code` is a step
that cannot complete on any key. Nothing here is wrong in isolation: the skip is
honest, and the refusal is the owner's decision working as intended.

Three ways out, and the choice is a **policy** one:

1. **Drop the step from the standard procedure.** The unit does not use the OTP slots,
   which is the likeliest truth — and it is the smallest change: an edit on the
   Templates screen, no code.
2. **Program the slot first, then protect it** — phase 5 (challenge-response, a static
   password, a Yubico OTP registered with somebody). That needs somebody to say *what
   the slot is for*, which nobody has, and it makes the procedure write a credential
   the holder was not promised.
3. **Narrow the phase-5 refusal** so a programmed OTP slot alone is not evidence of a
   previous bootstrap — the same carve-out already made for the PIV attestation slot
   `f9` and for a changed management key. Defensible (a factory Yubico OTP credential
   is not somebody's signing identity) and **the owner's call**, because it weakens the
   control that stops a live credential being overwritten.

Recorded here rather than decided, per `AGENTS.md` §8, and listed in
[`roadmap.md`](../roadmap.md) under Open questions.

## Audit events

| Event | Detail (never the code) |
|---|---|
| `bootstrap.step.done` | `step=otp-access-code slot=1 protected=true` |
| `bootstrap.step.failed` | `step=otp-access-code slot=1 reason=<status>` |
| `secret.custody` | `step=otp-access-code slot=1 custody=sealed-envelope retained=no` — where the code went, never the value (phase 7) |
| `otp.slot.programmed` | Phase 5: `slot=2 type=chalresp` |

## Tests

- Plan: the access-code step carries `Arg::Secret("OTP-ACCESS-CODE")` and renders as
  `<OTP-ACCESS-CODE>`; `otp_steps_still_fall_back_to_ykman` asserts the transport is
  honestly reported.
- Phase 3: unit tests for frame construction and CRC against vectors captured from a
  real exchange.
- Phase 4+: behaviour tests against a mock HID transport. **No test writes to a real
  key**; manual verification uses a dedicated test key and is recorded here.

## Open questions and gates

- **Is OTP even wanted?** For a FIDO2 + PIV deployment, the safest choice may be to
  *disable* the OTP application over USB rather than protect it. That is a policy
  decision; the template supports both once Phase 6 lands.
- Custody of the access code under model B: the default taken is **generate and discard**.
  Nobody records it, so the slot is permanently frozen — arguably the desired outcome
  ("nobody reprograms this"), at the cost of needing an OTP applet reset to change it later.
  It is written down here and in `roadmap.md` #6 rather than left implicit, and the
  alternative (put it in the sealed envelope) is one template parameter away.

## References

- `src/template/plan.rs`
- `docs/yubikey-reference.md`
- [`yubikey-personalization`](https://github.com/Yubico/yubikey-personalization) — the reference implementation of the configuration protocol
