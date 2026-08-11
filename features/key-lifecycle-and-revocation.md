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

**Status transitions only.** `KeyStatus` includes `Lost` and `Retired`, transitions are
guarded, and the Inventory screen can mark a key lost. Nothing downstream happens yet.

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
| 5 | Applet reset actions (PIV / FIDO2 / OTP), explicitly confirmed | Todo | destructive; must name what is lost |
| 6 | "Sanitised" gate before reissue | Todo | refuse reassignment of an unsanitised key |
| 7 | Incident note generation for the ESI | Todo | `features/reports-and-export.md` |
| 8 | RMA tracking (sent, replaced, replacement serial linked) | Todo | |

## Audit events

| Event | Detail |
|---|---|
| `key.reported_lost` | `serial=… holder=… reported_by=… when=…` |
| `key.certificate_revoked` | `cert_serial=… reason=keyCompromise reference=…` |
| `key.credential_removed` | `rp=… credential=…` |
| `key.applet_reset` | `applet=piv|fido2|otp` — destructive, confirmed |
| `key.sanitised` | The key is clear for reassignment |
| `key.retired` | `reason=…` |

## Tests

- Behaviour: report a key lost → the dependency list contains the certificate serial and
  the credential ids recorded by its bootstrap run.
- Behaviour: a returned key cannot be reassigned until sanitised (Phase 6).
- Behaviour: retirement is terminal (already covered by
  `lifecycle_transitions_are_restricted`).
- Applet resets are tested against mock transports only; never against real hardware.

## Open questions and gates

- **Incident reporting**: the norm requires ESI involvement for a possible compromise.
  The tool can prepare the note; who sends it, and within what deadline, is process.
- Does a lost key require the holder's confirmation in writing? Affects Phase 2's form.
- Reset authority: who may reset an applet, and does it need two operators?

## References

- `src/domain/key.rs`, `features/key-inventory.md`, `features/ca-integration.md`
- `docs/operations.md`
