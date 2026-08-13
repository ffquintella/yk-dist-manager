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
| 2 | Executor on the `ykman` path using the prompt form (`--access-code -`) | Todo | no code in argv |
| 3 | Native read path over `hidapi` (slot status, sequence) | Todo — **the `ykman` read landed instead** | `device::ykman::parse_otp_info` answers "which slots are programmed" today, unit-tested against recorded output, and feeds the pre-flight refusal. The *native* frame is still unwritten. Note what neither path can answer: **whether a slot carries an access code** — nothing reports it, so no read ever claims one |
| 4 | Native write path (access code) | Todo — **blocked on hardware, deliberately** | frame + CRC + status confirmation, hand-rolled because no crate exposes it. Not written without a key to verify against: the failure mode of a wrong frame is a slot write-protected by a code nobody holds, which is unrecoverable from this tool |
| 5 | Optional slot programming (challenge-response) | Todo | for local unlock use cases |
| 6 | Warn that a protected slot blocks interface mode switching | Todo | in the wizard, before the run |
| 7 | Record custody of the access code | Todo | `features/secrets-custody.md` |

## Audit events

| Event | Detail (never the code) |
|---|---|
| `bootstrap.step.done` | `step=otp-access-code slot=1 protected=true` |
| `bootstrap.step.failed` | `step=otp-access-code slot=1 reason=<status>` |
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
