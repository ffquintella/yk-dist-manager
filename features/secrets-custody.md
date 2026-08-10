# Feature: Secrets custody

## Summary

Decide and implement what happens to the PINs, PUKs, access codes and management keys
the bootstrap sets: how they are produced, how they reach the holder, and what — if
anything — is kept. The database records **where custody went**, never the value.

## Motivation

The bootstrap sets four or five secrets per key. Every one of them is a decision:

- Nobody keeps it → the key is self-contained, but a forgotten PIN means a reset and a
  new certificate.
- Somebody keeps it → resets get easier, and the tool has created a secret store that
  it must then protect, back up, and eventually purge.

Getting this wrong in the obvious direction (writing PINs into the database "for
support") turns a distribution register into a credential store, and turns a copied
file from an inconvenience into an incident. `AGENTS.md` forbids it; this feature is
where the alternative is designed.

## Current state

**Design only.** The infrastructure that keeps secrets out is in place: secrets exist
in a plan as `Arg::Secret` placeholders, `BootstrapRun.custody` holds a free-text note
about *where* custody went (currently `"dry run — no secret was set"`), and
`StepKind::sets_secret()` marks which steps produce one. No secret is generated,
prompted for, or stored anywhere yet.

## Design

### The three models

| Model | How it works | Cost |
|---|---|---|
| **A. Holder-set at the desk** | The holder types their own PIN during hand-over; the operator never learns it | Requires the holder present; no recovery — a forgotten PIN means a reset |
| **B. Transport PIN + forced change** | The operator sets a temporary PIN, marks it for mandatory change (CTAP 2.1 `forcePINChange`), and the holder changes it on first use | Works for posted keys; the transport PIN must reach the holder out of band |
| **C. Generated + escrowed** | The tool generates the secret, shows it once, and records it in an external secret store (never in this database) | Recovery possible; creates a secret store with per-device values |

The default recommendation is **B for FIDO2** (the mechanism exists in the
firmware and is designed for exactly this) and **A for the PIV PIN** where the holder
is present, falling back to B semantics — a transport PIN plus an instruction to
change it — when the key is posted.

**C is opt-in and never uses this database.** If escrow is wanted, the target is an
external store (BastionVault KV under a per-serial path), and what this tool records
is the *reference*: `bastionvault:kv/yubikeys/20423633`. That way the custody trail is
complete and the secret is where a secret store belongs.

### Generation rules (when the tool generates)

- Use the OS CSPRNG (`getrandom`), never a seeded PRNG.
- PINs: numeric by default (keypad compatibility), length from the template, minimum 6.
- OTP access code: 6 bytes of randomness, rendered as 12 hex characters.
- Management key: random AES-256, and preferably `--protect`ed onto the key so it never
  leaves it (`features/step-piv-pin-puk-management-key.md`).
- Show once, in a panel the operator must dismiss deliberately. Never in a log, never
  in a screenshot-friendly persistent view, never copied to the clipboard silently.
- Zeroise the buffer after use (`zeroize` crate), and keep the value out of any
  `Debug` output — the secret types get a manual `Debug` that prints `<redacted>`.

### What is recorded

| Recorded | Never recorded |
|---|---|
| Which secrets a run set (by step kind) | Any secret value |
| Custody destination (`holder-set`, `forced-change`, `envelope:2026-08-10-014`, `bastionvault:kv/…`) | A hint, a partial value, or a reversible transform |
| Who performed the run, and when | A "temporary" copy in a note field |
| Whether a forced change was set | |

`BootstrapRun.custody` is free text today; Phase 2 makes it a typed enum plus an
optional reference, so a report can answer "which keys have escrowed PINs?".

### Hand-over channel

If a transport PIN must reach the holder, the channel matters: in person, or a sealed
printed envelope, or an out-of-band message — never the same e-mail that the key's
own certificate protects, and never a chat message that persists. Phase 5 renders a
sealed-envelope slip so the physical channel is the default.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Decide the model (A / B / C per secret) | **Blocked** | needs an owner's decision; see gates |
| 2 | Typed custody field on the run + report | Todo | replaces free text |
| 3 | Secret input: prompt, generate, show-once, zeroise, redacted `Debug` | Todo | with `zeroize` |
| 4 | `forcePINChange` support in the FIDO2 step | Todo | makes model B real |
| 5 | Sealed-envelope slip rendering | Todo | printable, one per key, no PIN in the database |
| 6 | Optional external escrow (BastionVault KV), reference-only in the database | Todo | never the value here |
| 7 | Custody report: which keys hold which custody model | Todo | `features/reports-and-export.md` |

## Audit events

| Event | Detail |
|---|---|
| `secret.generated` | `step=<id> kind=fido2-pin length=6` — never the value |
| `secret.custody.recorded` | `run=<id> custody=forced-change` (or the escrow reference) |
| `secret.shown` | The show-once panel was displayed and dismissed |
| `secret.escrowed` | `reference=bastionvault:kv/yubikeys/20423633` |

## Tests

- Existing: `no_plan_output_can_leak_a_secret`,
  `pin_carrying_steps_use_a_secret_placeholder`,
  `scenario_the_plan_never_shows_a_pin`.
- Phase 3 adds: a generated secret's `Debug` output contains no digits of the value; a
  secret's buffer is zeroised after use; nothing containing the value reaches the
  audit or log sinks (assert by capturing both sinks during a mock run).
- A test that greps every persisted record of a completed mock run for the generated
  values and fails if any appears — the blunt instrument that catches accidental
  leakage through a field nobody thought about.

## Open questions and gates

1. **Which model, per secret?** This is the decision that blocks Wave 1's executor
   (`roadmap.md` open question #4). It is an operational and risk decision, not an
   implementation choice.
2. **If escrow is chosen**, the store, its access control and its retention are ESI
   decisions, and the DPO should be aware that a credential store now exists.
3. **Reset policy**: who may reset a key whose PIN is forgotten, and does that require
   a second operator?

## References

- `src/template/plan.rs` (`Arg::Secret`), `src/domain/bootstrap.rs` (`custody`)
- `AGENTS.md` §2, `docs/security-and-compliance.md`
- `features/step-fido2-pin.md`, `features/step-piv-pin-puk-management-key.md`
