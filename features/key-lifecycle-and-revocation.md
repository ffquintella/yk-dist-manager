# Feature: Key lifecycle and revocation

## Summary

What happens after the hand-over: a key reported lost or stolen, a key handed back, a
key recycled to another holder, a key retired. Each path has a technical action, not
just a status change.

## Motivation

A status field that says `Lost` while the certificate is still valid and the FIDO
credential still works is theatre. The lifecycle is only meaningful if each state
transition triggers the action it implies: revoke the certificate, remove the
credential from the relying party, reset the applets before reissue, and record all of
it.

This is also where the tool earns its keep during an incident: "Ana's key is missing" has
to produce, in one screen, the serial, the certificate serial to revoke, the credential
ids to remove, and the record of what was on it.

## Current state

**Status transitions, and the factory reset (phase 5).** `KeyStatus` includes `Lost` and
`Retired`, transitions are guarded, and the Inventory screen can mark a key lost.

Phase 5 landed out of turn — see the note in [`roadmap.md`](../roadmap.md). A plugged key
can be **returned to factory default**, per applet, from *Attached now* on the Inventory
screen: [`device::reset`](../src/device/reset.rs) previews what each applet's reset
destroys *and* what this key was read to be carrying, takes an unforgeable
`Confirmation`, records before it writes, and reports one outcome per applet. FIDO2 goes
first because CTAP only accepts a reset within seconds of power-up. Revocation, the
dependency list and the sanitised gate are still Todo, so a reset today destroys what is
on the key and does **not** revoke the certificate that was on it.

**The power cycle a FIDO2 reset needs (phase 5a).** That "within seconds of power-up" was,
until now, a sentence in the preview asking the operator to unplug the key and plug it back
in *before* confirming — a race they could not see the start of, and one they mostly lost:
the reset came back `ERROR: Reset failed. Reset must be triggered within 5 seconds after
the YubiKey is inserted`. So the tool now runs the race itself
([`device::reinsert`](../src/device/reinsert.rs)). The confirmation is taken first and the
applets are frozen into it; the panel then asks for the key to be pulled out and put back,
watches the port with a fast `list_serials` poll of its own, and fires the run on the
observation that sees the serial return. The operator can arm it by hand for a slow port,
the window closing is reported rather than risked as a doomed command, an untouched key
ends the handshake after a minute, and a FIDO2 row that says *refused* offers the whole
thing again for that applet alone.

## Design

### Lost or stolen

The response sequence, all of it recorded:

1. Mark the key `Lost` (Done today), with when and who reported it.
2. **Revoke the PIV certificate** at the issuing CA, with reason `keyCompromise`, and
   store the revocation reference (`features/ca-integration.md` Phase 8).
3. **Remove the FIDO2 credential(s)** from the relying party, using the credential ids
   recorded on the bootstrap run — this is the concrete payoff for recording them.
4. If the key protected anything else (a database password via challenge-response,
   an SSH authorised key), list those dependencies so nothing is missed.
5. Produce an incident note: serial, holder, what was on the key, what was revoked, and
   when. The norm treats a possible credential compromise as an incident to be
   reported to the ESI — the tool should produce that text, not leave it to memory.

### Returned

A returned key is not automatically reusable. Before it goes to another holder:

- Reset the PIV applet (destroys the key and certificate in 9c) **and** revoke the old
  certificate — a returned certificate is still valid until revoked.
- Reset FIDO2 (destroys every credential and the PIN) if it is being reassigned.
- Confirm the OTP access code situation: a protected slot cannot be reprogrammed
  without the code, so a key whose code was never recorded may be partly frozen.
- Only then may the key move to `InStock` or be bootstrapped again.

The tool should refuse `Returned → Bootstrapped` until a "sanitised" action is recorded,
so a key cannot silently carry the previous holder's credentials to a new one. That is
a stronger rule than today's transition table and needs its own phase.

### Retired

Terminal. Reasons: hardware failure (RMA), firmware end of support, physical damage,
or loss written off. A retired key's certificate must be revoked if it was not already,
and the record stays forever — retirement is not deletion.

Deletion exists, and is deliberately *not* on this path: the Inventory screen can
remove an inventory row, confirmed, for a mistake at intake — a mis-typed serial, a
label scanned twice. `Store::delete_key` refuses any serial a hand-over or a
bootstrap run refers to, so a key that ever went out or was ever bootstrapped can
only be retired. See `features/key-inventory.md`, "The observation, and what removal
is for".

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Status transitions with guards | Done | `features/key-inventory.md` |
| 2 | Lost/stolen workflow: reason, date, reporter, dependency list | Todo | |
| 3 | Certificate revocation through the issuer | Todo | needs `features/ca-integration.md` |
| 4 | FIDO2 credential removal at the relying party | Todo | uses the recorded credential ids |
| 5 | Applet reset actions (PIV / FIDO2 / OTP), explicitly confirmed | **Done** | `device::reset`. Per applet, ticked individually; the loss is named twice (what the applet's reset destroys, and what this key was read to hold); the serial is typed back before the button enables; the run records before it writes and one applet's refusal does not stop the others. FIDO2 and OTP go through `ykman`, labelled — no crate here sends `authenticatorReset` and the OTP frames are unimplemented. PIV is native where the session is |
| 5a | The power-cycle handshake in front of a FIDO2 reset | **Done** | `device::reinsert`. Confirm first, then the panel asks for the key out and back in, polls the port for that one serial (200ms native, 500ms through the subprocess) and sends the reset on the observation that sees it return. A manual arm for a slow port; a window that closes is reported instead of sending a command the applet would refuse; a minute untouched abandons it; a refused FIDO2 row retries the handshake for that applet alone. Nothing is written at any step until the key is back |
| 6 | "Sanitised" gate before reissue | Todo | refuse reassignment of an unsanitised key |
| 7 | Incident note generation for the ESI | Todo | `features/reports-and-export.md` |
| 8 | RMA tracking (sent, replaced, replacement serial linked) | Todo | |

## Audit events

| Event | Detail |
|---|---|
| `key.reported_lost` | `serial=… holder=… reported_by=… when=…` |
| `key.certificate_revoked` | `cert_serial=… reason=keyCompromise reference=…` |
| `key.credential_removed` | `rp=… credential=…` |
| `key.reset.power_cycle.requested` | `applets=… attempt=N reason=…` — the operator was asked to re-insert the key before a FIDO2 reset |
| `key.reset.power_cycle.armed` | `applets=… attempt=N detected_within=…ms window=…ms` — the key came back and the run is going out now |
| `key.reset.power_cycle.abandoned` | `applets=… attempt=N reason=…` — the window closed, the operator cancelled, or the key was never re-inserted. **Nothing was written** |
| `key.reset.started` | `applets=fido2+piv+otp operator=…` — written **before** the first write; a failure here abandons the reset |
| `key.applet_reset` | `applet=piv\|fido2\|otp transport=native\|ykman detail=…` — destructive, confirmed, and only written when something actually was |
| `key.reset.skipped` | The applet was already at factory default, so nothing was written |
| `key.reset.failed` | `applet=… transport=… reason=…`, the transport's own words |
| `key.reset.finished` | `reset=N failed=N skipped=N` |
| `key.sanitised` | The key is clear for reassignment |
| `key.retired` | `reason=…` |

## Tests

- Behaviour: report a key lost → the dependency list contains the certificate serial and
  the credential ids recorded by its bootstrap run.
- Behaviour: a returned key cannot be reassigned until sanitised (Phase 6).
- Behaviour: retirement is terminal (already covered by
  `lifecycle_transitions_are_restricted`).
- Applet resets are tested against mock transports only; never against real hardware.
  `tests/behaviour_key_reset.rs` drives `device::reset::MockResetter` against a real
  `Store`: a whole key returned to factory default leaves one entry per applet in the
  chain; a register that cannot record stops the reset before the first write; a stale
  confirmation is refused; one applet refusing does not stop the others; an unplugged key
  reports which applets never ran; and the inventory row and its observation outlive the
  reset. The routing, the preview and the confirmation gate are unit tested in
  `src/device/reset.rs`.
- Behaviour: a confirmed FIDO2 reset writes nothing while the key stays in the port, runs
  the moment it is taken out and put back, and leaves `power_cycle.requested` →
  `power_cycle.armed` → the reset's own entries in that order; a power cycle nobody
  performs writes nothing and records why. Both in `tests/behaviour_key_reset.rs`, against
  synthetic instants — no test here sleeps, and none touches a key.
- The handshake itself is unit tested in `src/device/reinsert.rs`: a key already in the
  port never arms; the reset fires once, on the observation that sees the key return; a
  window that closes is reported rather than sent; the operator's own arm; an abandoned
  handshake; and the presence poll, which reports the one serial it was given, treats an
  empty port as an answer, keeps trying after a busy reader, gives up on a missing
  transport, and stops promptly when dropped.

## Open questions and gates

- **Incident reporting**: the norm requires ESI involvement for a possible compromise.
  The tool can prepare the note; who sends it, and within what deadline, is process.
- Does a lost key require the holder's confirmation in writing? Affects Phase 2's form.
- Reset authority: who may reset an applet, and does it need two operators? *Owner: ESI.*
  Phase 5 ships on the assumption already in force everywhere else in this tool — the
  operator is `$USER` and there are no roles yet
  ([`features/operator-auth-and-roles.md`](operator-auth-and-roles.md), which lists
  "resetting applets" as an administrator action). So today **any operator with the
  register open can reset a plugged key**, and the trail names who did. If the answer is
  two operators, or administrators only, it is a gate on top of a working action rather
  than a change to it.

## References

- `src/domain/key.rs`, `features/key-inventory.md`, `features/ca-integration.md`
- `docs/operations.md`
