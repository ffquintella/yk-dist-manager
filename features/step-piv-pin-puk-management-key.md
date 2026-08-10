# Feature: Step — PIV PIN, PUK and management key

## Summary

Leave no factory default on the PIV applet: change the PIN, change the PUK, and
replace the default management key — preferably with a random one stored on the key
itself and guarded by the PIN, so there is nothing to hold in custody.

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

**Planned, not executed.** Two plan entries:

- `StepKind::PivPinPuk` → `yubikey::YubiKey::change_pin / change_puk`
  (fallback: `ykman piv access change-pin --pin <old> --new-pin <new>`, then
  `change-puk`).
- `StepKind::PivManagementKey` → `yubikey::MgmKey::set_protected`
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

Algorithm: AES-256 where the firmware supports it (5.4+), TDES otherwise. The
step reads the firmware and picks, rather than failing on older keys.

Consequence of option 1 worth stating: if the PIN is blocked *and* the PUK is
blocked, the protected management key is unrecoverable and the applet must be reset.
That is acceptable — a key in that state has no usable credentials anyway — but the
operator should know it.

### Template parameters

| Parameter | Meaning | Default |
|---|---|---|
| `algorithm` | `aes256` \| `aes192` \| `aes128` \| `tdes` | `aes256` |
| `protect` | Store a random management key on the key, PIN-guarded | `true` |
| `source` (PIN/PUK) | `operator-entered` \| `holder-entered` \| `generated` | `operator-entered` |
| `set_retries` | Optional PIN/PUK retry counts | unset |

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Plan entries for PIN/PUK and management key | Done | |
| 2 | Read PIV state: retries remaining, management-key algorithm, whether protected | Todo | never probe by trying |
| 3 | Change PIN and PUK natively | Todo | ordering: retries → PIN → PUK |
| 4 | Set a protected random management key (AES-256, TDES fallback) | Todo | default path |
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

- Who chooses the PIV PIN — the operator (then it must be handed over and changed) or
  the holder at the desk (nothing to escrow, but the holder must be present)? Same
  decision as the FIDO2 PIN; see `features/secrets-custody.md`.
- Whether to change the retry counts from the default 3 at all.

## References

- `src/template/plan.rs`
- `docs/yubikey-reference.md`
- [NIST SP 800-73-4 (PIV card interface)](https://csrc.nist.gov/publications/detail/sp/800-73/4/final)
