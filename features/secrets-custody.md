# Feature: Secrets custody

## Summary

What happens to the PINs, PUKs, access codes and management keys the bootstrap sets: how
they are produced, how they reach the holder, and what — if anything — is kept. The
database records **where custody went**, never the value.

> **Decided 2026-08-10 — model B: transport secret plus forced change.** The operator sets
> a temporary secret, the key is marked so the holder must change it before first use, and
> this tool retains nothing.

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

**Model decided, vocabulary fixed in code, and the secret-handling machinery built.**

What is in place:

- `domain::CustodyModel` — the four models with a stored form
  (`transport-pin+forced-change`, `holder-set`, `escrowed:<reference>`,
  `no-secret-set`), `DEFAULT` = model B, and `parse` for reading a run back. Only
  `Escrowed` carries a reference, and the reference points at an *external* store.
- `domain::ChangeEnforcement` — whether the key enforces the change
  (`enforced-by-firmware`) or the hand-over term instructs it
  (`instructed-on-handover`), decided from the firmware for FIDO2 and always
  procedural for PIV.
- `StepKind::Fido2ForcePinChange` and a `fido2-force-pin-change` step in both built-in
  templates, planned as `ykman fido access force-change` with the firmware gate stated on
  the step.
- Secrets in a plan remain `Arg::Secret` placeholders; a dry run records
  `no-secret-set`.

- `crate::secret` — the generate / show-once / zeroise path. `Secret` has no
  `Clone`, no `Serialize` and no `Display`; its `Debug` prints `<redacted>`, so a
  panic message, a stray `dbg!` or a mis-pointed `tracing` field prints the
  redaction rather than the value. Values come from the OS CSPRNG with rejection
  sampling, so the digits of a generated PIN are not biased towards 0–5 the way
  `byte % 10` would make them. `ShowOnce` is wiped by `dismiss()` and by `Drop`,
  and offers no second look.
- The executor audits `secret.generated` (kind and length, never the value) and
  `secret.change_enforcement` (`enforced-by-firmware` for FIDO2 where the
  firmware has it, `instructed-on-handover` for PIV always).
- A behaviour scenario greps every persisted record and audit entry of a complete
  mock run against every value it generated — the blunt instrument that catches
  leakage through a field nobody thought about.

Still to build: the custody report.

## Design

### The three models, and the one chosen

| Model | How it works | Cost |
|---|---|---|
| **A. Holder-set at the desk** | The holder types their own PIN during hand-over; the operator never learns it | Requires the holder present; no recovery — a forgotten PIN means a reset |
| **B. Transport secret + forced change** ← **chosen** | The operator sets a temporary secret, marks it for mandatory change, and the holder replaces it on first use | The transport secret must reach the holder out of band |
| **C. Generated + escrowed** | The tool generates the secret and records it in an external secret store (never in this database) | Recovery possible; creates a secret store with per-device values |

**B applies to every secret-setting step**, which is what makes the procedure uniform for
a desk hand-over and a posted key alike. Model A remains available per template (a holder
who is present may simply type their own), and **C is opt-in and never uses this
database** — the target is an external store (BastionVault KV under a per-serial path) and
what is recorded here is the *reference*, `escrowed:bastionvault:kv/yubikeys/20423633`.

#### What B means per secret

| Secret | Under model B | Enforcement |
|---|---|---|
| **FIDO2 PIN** | Operator sets a transport PIN; `forcePINChange` marks the key | **Firmware** from 5.7 (CTAP 2.1); **procedural** below. Two reference keys are now on the bench: a 5.4.3 (procedural path) and a **5.7.4** (firmware path), so both are testable |
| **PIV PIN** | Operator sets a transport PIN; the term instructs the change | **Procedural always** — PIV has no force-change flag at any firmware level |
| **PIV PUK** | Handed over in the same envelope; nothing retained | Procedural; **confirmed 2026-08-11** |
| **PIV management key** | `--protect --generate`: random, stored on the key, PIN-guarded | Nothing to hand over and nothing to retain — unaffected by B |
| **OTP access code** | Handed over in the same envelope; nothing retained | **Reversed 2026-08-11** — was generate-and-discard |

The honest consequence of "enforcement is sometimes procedural" is that a transport PIN
can survive if the holder ignores the instruction. That is why the run records
`ChangeEnforcement`: an audit can then tell an enforced change from an instructed one, and
a report can list the keys where it was only instructed.

#### The two sub-decisions B left open — both settled 2026-08-11

1. **The PUK: sealed envelope, nothing retained.** The default is confirmed. A blocked PIN
   with a lost PUK costs a PIV applet reset and a new certificate; that price was accepted
   over retaining the PUK for support, which would be escrow — a per-device secret store
   with everything that implies.
2. **The OTP access code: sealed envelope too.** This *reverses* the default. Generate-and-
   discard froze the slot deliberately, so reprogramming it later meant an OTP applet reset.
   Carrying the code keeps that door open, at the price of one more line on a slip the
   holder is told to destroy after use.

The rule that falls out is simpler than the one it replaces: **every generated secret
travels except the management key**, which is `--protect`ed onto the key itself and so has
nothing to hand over. That is exactly what `SecretKind::goes_to_the_holder` now says, and
what the slip renders.

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
| The custody model (`transport-pin+forced-change`, `holder-set`, `escrowed:<reference>`, `no-secret-set`) | A hint, a partial value, or a reversible transform |
| Who performed the run, and when | A "temporary" copy in a note field |
| Whether the change was enforced by firmware or only instructed | |

`BootstrapRun.custody` holds the stored form of `CustodyModel`, so a report can answer
"which keys have escrowed secrets?" and "which keys were only *instructed* to change the
transport PIN?" without parsing prose.

### Hand-over channel — now a required part of the procedure

Model B always hands a secret over, so the channel is not an afterthought: in person, or a
sealed printed envelope, never the same e-mail the key's certificate protects and never a
chat message that persists. Phase 5's sealed-envelope slip is therefore no longer optional
polish — it is how model B is executed for a posted key, and it is a prerequisite for
distributing by courier or post.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Decide the model | **Done (2026-08-10)** | model B — transport secret + forced change, nothing retained |
| 2 | Typed custody vocabulary on the run | Done | `CustodyModel` + `ChangeEnforcement`; stored in the existing `custody` column, so no migration. A dedicated column arrives with schema v2 if reporting needs one |
| 3 | Secret input: prompt, generate, show-once, zeroise, redacted `Debug` | **Done** | [`src/secret.rs`](../src/secret.rs) — `zeroize` + the OS CSPRNG, `ShowOnce` wiped on dismissal and on drop |
| 4 | `forcePINChange` in the FIDO2 step | **Done** | the executor calls it and audits the enforcement, and the CTAP transport behind it is hardware-verified on a 5.7.4 key (`step-fido2-pin.md` phase 6). Below 5.7 the firmware cannot enforce it and the pre-flight says so: the signed term becomes the mechanism |
| 5 | Sealed-envelope slip rendering | **Done** | [`src/envelope.rs`](../src/envelope.rs) — never stored, bytes zeroised, refuses a dismissed panel |
| 6 | Optional external escrow (BastionVault KV), reference-only in the database | Todo | never the value here |
| 7 | Custody report: which keys hold which model, and where the change was only *instructed* | Todo | `features/reports-and-export.md` |
| 8 | Resolve the PUK and OTP-access-code sub-decisions | **Done (2026-08-11)** | both answered by the owner and recorded in `roadmap.md`: the **PUK** travels in the sealed envelope and nothing is retained, and the **OTP access code** does too — which reversed the generate-and-discard default this was built with, so a protected slot can be reprogrammed later without an applet reset. `SecretKind::goes_to_the_holder` is where the answer lives, and the management key is now the only secret that does not travel, because it is protected onto the key itself. Phase 7's custody *report* is separate and still Todo |

## Audit events

| Event | Detail |
|---|---|
| `secret.generated` | `step=<id> kind=fido2-pin length=6` — never the value |
| `secret.custody.recorded` | `run=<id> custody=transport-pin+forced-change` (or the escrow reference) |
| `secret.change_enforcement` | `step=fido2-pin enforcement=enforced-by-firmware\|instructed-on-handover` |
| `secret.shown` | The show-once panel was displayed and dismissed |
| `secret.escrowed` | `reference=bastionvault:kv/yubikeys/20423633` |

## Tests

- Existing: `no_plan_output_can_leak_a_secret`,
  `pin_carrying_steps_use_a_secret_placeholder`,
  `scenario_the_plan_never_shows_a_pin`.
- Model B: `the_default_is_model_b`, `model_b_needs_an_out_of_band_channel_but_nothing_retained`,
  `escrow_is_the_only_model_that_records_a_pointer`,
  `a_stored_note_reads_back_including_its_reference`,
  `a_pre_5_7_key_cannot_enforce_the_change_itself`,
  `the_standard_template_forces_the_holder_to_change_the_transport_pin`,
  `the_fido_only_template_keeps_the_forced_change`,
  `a_dry_run_records_that_no_secret_was_set`.
- Phase 3 adds: a generated secret's `Debug` output contains no digits of the value; a
  secret's buffer is zeroised after use; nothing containing the value reaches the
  audit or log sinks (assert by capturing both sinks during a mock run).
- A test that greps every persisted record of a completed mock run for the generated
  values and fails if any appears — the blunt instrument that catches accidental
  leakage through a field nobody thought about.

## Open questions and gates

1. ~~Which model?~~ **Answered 2026-08-10: model B.**
2. ~~The PUK?~~ **Answered 2026-08-11: handed over in the sealed envelope, nothing
   retained.** No escrow.
3. ~~The OTP access code?~~ **Answered 2026-08-11: carried in the envelope**, reversing the
   generate-and-discard default.
4. **Reset policy**: who may reset a key whose PIN is forgotten, and does that require a
   second operator? Model B makes this more likely to be needed, not less: a holder who
   forgets the PIN they just set has no recovery path.
5. If escrow is ever switched on for a template, the store, its access control and its
   retention are ESI decisions, and the DPO should know a credential store now exists.

## References

- `src/template/plan.rs` (`Arg::Secret`), `src/domain/bootstrap.rs` (`custody`)
- `AGENTS.md` §2, `docs/security-and-compliance.md`
- `features/step-fido2-pin.md`, `features/step-piv-pin-puk-management-key.md`
