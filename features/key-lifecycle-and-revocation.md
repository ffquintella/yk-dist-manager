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

**The wave is built.** Every phase here is done: the loss report and the dependency list
(2), the revocation record (3), the credential removal (4), the factory reset and its
power cycle (5, 5a), the sanitised gate before reissue (6), the incident note (7) and RMA
tracking (8). Schema **v6** carries the three tables it needs; what a key was *carrying*
is deliberately not one of them — see below.

**One decision shapes phases 3 and 4, and it is the same one that shaped the CA
integration**: the tool **records** these obligations rather than performing them. A
certificate is revoked at the CA that issued it and a credential is removed at the relying
party that holds it, and both are somebody else's system — the decision of 2026-08-13 made
the operator the issuer for the same reason. So the value here is not automation: it is
that after an incident nobody has to remember what was on the key, and nothing that was on
it can be quietly forgotten. Automating either is a later phase, not a missing half.

**Where it lives.** *Lifecycle…* on each Inventory row opens one panel, because during an
incident these are one job: record what you were told, see what was on the key, deal with
each of those elsewhere, record that you did, and produce the note somebody has to send.
It replaced the row's *mark lost* button, which set a status and recorded nothing — no
reporter, no date, and no list of the credentials that were still live.

**The loss report (phase 2).** [`KeyIncident`](../src/domain/lifecycle.rs) is one row per
report: lost or stolen, when, who reported it (required — a register does not assert a loss
on its own authority), the holder as the register knew them, and the circumstances. The
report and the move to `Lost` are **one store operation**
([`Store::report_incident`](../src/store/mod.rs)), and the lifecycle is asked first: the
ordering `features/distribution-records.md` was corrected into, so a key can never be
`Lost` with nothing saying why, or carry a report while the register still says it is in
somebody's hands.

**The dependency list is derived, not stored.** What a key was carrying comes out of the
bootstrap run's step details ([`lifecycle::dependencies`](../src/domain/lifecycle.rs)) —
the certificate serial from the import step, the credential id and relying party from the
credential step, the OTP access code, and where custody of the secrets went. Storing it
would be a second truth about what a run did, and would leave every register written before
this feature with an empty list; derived, a register from schema v1 answers in full. Only
steps that **succeeded** count: a failed import left nothing on the key to revoke.

**The sanitised gate (phase 6).** A key that still carries what a bootstrap put on it
cannot go back into stock or be prepared for somebody else —
[`Store::set_key_status`](../src/store/mod.rs) refuses it with `NotSanitised`, naming the
applets and the action. Per applet and by time: an applet is clear when a sanitisation
covering it was recorded *after* the last run that wrote to it. A factory reset records its
own, from its **outcomes** rather than from what was ticked, so an applet that refused is
not recorded as clean; a key reset elsewhere is recorded by hand, marked as the operator's
word. The one move deliberately not gated is `In stock → Bootstrapped`: that *is* the
bootstrap, and gating it would refuse every key the tool has just prepared.

**Status transitions, and the factory reset (phase 5).** `KeyStatus` includes `Lost` and
`Retired`, transitions are guarded, and the Inventory screen can mark a key lost.

Phases 5 and 5a landed out of turn — see the note in [`roadmap.md`](../roadmap.md). A plugged key
can be **returned to factory default**, per applet, from *Attached now* on the Inventory
screen: [`device::reset`](../src/device/reset.rs) previews what each applet's reset
destroys *and* what this key was read to be carrying, takes an unforgeable
`Confirmation`, records before it writes, and reports one outcome per applet. FIDO2 goes
first because CTAP only accepts a reset within seconds of power-up. A reset **still does not
revoke** the certificate that was on the key — nothing can, from here; what the panel now
does is list that certificate as outstanding and hold the reference once the operator has
revoked it at the CA.

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
| 2 | Lost/stolen workflow: reason, date, reporter, dependency list | **Done** | `KeyIncident` + `lifecycle::dependencies`. Lost and stolen are separate kinds; `reported_by` is required; the report and the move to `Lost` are one store operation, lifecycle asked first. The dependency list is **derived from the run's step details**, so a register written under schema v1 answers in full and there is nothing to backfill. The circumstances are counted in the audit entry, never quoted — an entry cannot be corrected and free text sometimes has to be |
| 3 | Certificate revocation through the issuer | **Done as a record** | The revocation happens at the CA — the issuer is the operator (decision of 2026-08-13, `features/ca-integration.md` phase 1) — and the register holds the RFC 5280 reason and the CA's own reference, defaulting to `keyCompromise` for a key nobody can produce. **Automating it** at an internal CA, BastionVault or an enterprise CA is an automation of this path, tracked with those issuers in `features/ca-integration.md` phases 3–5 |
| 4 | FIDO2 credential removal at the relying party | **Done as a record** | Same shape, and the reason `features/step-fido2-credentials.md` phase 4 recorded the ids at all: the panel names the credential id and relying party to remove, and holds the reference once it has been. Listing and deleting credentials *on the key* stays open in that spec, and needs a relying party this deployment has named |
| 5 | Applet reset actions (PIV / FIDO2 / OTP), explicitly confirmed | **Done** | `device::reset`. Per applet, ticked individually; the loss is named twice (what the applet's reset destroys, and what this key was read to hold); the serial is typed back before the button enables; the run records before it writes and one applet's refusal does not stop the others. FIDO2 and OTP go through `ykman`, labelled — no crate here sends `authenticatorReset` and the OTP frames are unimplemented. PIV is native where the session is |
| 5a | The power-cycle handshake in front of a FIDO2 reset | **Done** | `device::reinsert`. Confirm first, then the panel asks for the key out and back in, polls the port for that one serial (200ms native, 500ms through the subprocess) and sends the reset on the observation that sees it return. A manual arm for a slow port; a window that closes is reported instead of sending a command the applet would refuse; a minute untouched abandons it; a refused FIDO2 row retries the handshake for that applet alone. Nothing is written at any step until the key is back |
| 6 | "Sanitised" gate before reissue | **Done** | `Store::set_key_status` refuses `NotSanitised`, per applet and by time: an applet is clear when a reset covering it was recorded *after* the last run that wrote to it. A reset records its own sanitisation from its **outcomes**, so an applet that refused is not recorded as clean; a bench reset is recorded by hand and marked as the operator's word. `In stock → Bootstrapped` is deliberately not gated — that move *is* the bootstrap |
| 7 | Incident note generation for the ESI | **Done** | [`crate::incident`](../src/incident.rs): what happened, what the key was carrying, what has been dealt with, what is still owed — as text or as a PDF through `crate::pdf`, from one rendering so the copy on screen cannot disagree with the copy that is sent. It **names its own blind spot** (a service the holder registered directly, a disk the key unlocked, an SSH authorised key) rather than reading as exhaustive, and it is not filed in the register: the incident and the remediations are already there, and nothing signs a note. Where it goes is `AppSettings::report_incidents_to`; unset, the note says so instead of printing a placeholder. The custody *report* and the wider export stay with `features/reports-and-export.md` |
| 8 | RMA tracking (sent, replaced, replacement serial linked) | **Done** | `RmaCase`: the supplier's reference (required — an RMA nobody can quote is an RMA nobody can chase), the fault, and either a replacement or a closure. The replacement is a **serial already in the inventory**, refused otherwise: a case pointing at a key nobody recorded is the broken reference `delete_key` refuses to create, and the replacement has to be intaken like any other key. Neither a second replacement nor a closure may overwrite an answered case |

## Audit events

| Event | Detail |
|---|---|
| `key.reported_lost` | `kind=lost\|stolen reported_at=… reported_by=… holder=… circumstance_chars=N` — one event for both kinds, because the trail is filtered by event name and "which keys went missing" is one question. The circumstances are **counted, not quoted** |
| `key.certificate_revoked` | `subject=<cert serial> reason=keyCompromise reference=…` — recorded when the operator has revoked it at the issuing CA |
| `key.credential_removed` | `subject=<credential id> reference=… detail_chars=N`, the relying party in the detail |
| `key.incident_closed` | `kind=… outstanding=N note_chars=N` — `outstanding` is what was still owed when it was closed, so closing over a gap is visible in the trail rather than only in the note |
| `key.incident_note` | `kind=… outstanding=N format=text\|pdf` — the note carries the holder's name and what was on their key, so a copy leaving the tool is an event the register holds |
| `key.rma.sent` | `reference=… sent_at=…` |
| `key.rma.replaced` | `reference=… replacement=<serial>` |
| `key.rma.closed` | `reference=… note_chars=N` — closed with no replacement |
| `key.reset.power_cycle.requested` | `applets=… attempt=N reason=…` — the operator was asked to re-insert the key before a FIDO2 reset |
| `key.reset.power_cycle.armed` | `applets=… attempt=N detected_within=…ms window=…ms` — the key came back and the run is going out now |
| `key.reset.power_cycle.abandoned` | `applets=… attempt=N reason=…` — the window closed, the operator cancelled, or the key was never re-inserted. **Nothing was written** |
| `key.reset.started` | `applets=fido2+piv+otp operator=…` — written **before** the first write; a failure here abandons the reset |
| `key.applet_reset` | `applet=piv\|fido2\|otp transport=native\|ykman detail=…` — destructive, confirmed, and only written when something actually was |
| `key.reset.skipped` | The applet was already at factory default, so nothing was written |
| `key.reset.failed` | `applet=… transport=… reason=…`, the transport's own words |
| `key.reset.finished` | `reset=N failed=N skipped=N` |
| `key.sanitised` | `subject=fido2+piv+otp reference=… source=reset\|operator` — the applets that are at factory default, and **whose word that is**: a reset this tool performed, or an operator's claim about one it did not |
| `key.retired` | `reason=…` |

## Tests

- Behaviour: report a key lost → the dependency list contains the certificate serial and
  the credential ids recorded by its bootstrap run. `tests/behaviour_key_lifecycle.rs`
  drives the whole story through `YkDistApp`: a bootstrapped key handed to a holder is
  reported stolen, a report with no reporter is refused with the key unmoved, the
  dependency list names the certificate and the credential off the run, the revocation and
  the removal are recorded, the note reads *dealt with* for one and `OUTSTANDING` for the
  other, an incident with something owed will not close without a reason, a reset that
  cleared two applets of three leaves the gate closed for the third, and the RMA case
  refuses a replacement nobody has recorded. Nothing in it touches a key.
- Behaviour: a returned key cannot be reassigned until sanitised (Phase 6) —
  `tests/unit_store.rs`, including the two cases that must *not* be refused: a key nothing
  was ever written to, and `In stock → Bootstrapped`, which is the bootstrap itself.
- Unit (`src/domain/lifecycle.rs`): the dependency list off a run's step details; a failed
  step leaving nothing to revoke; a revocation matching a certificate serial however the
  CA's console spelled it (case, `0x`, colons); custody listed but nobody's ticket; the
  per-applet sanitisation rule, including a reset recorded *before* the run that clears
  nothing; only the applets a reset actually answered counting as cleared; a report needing
  somebody to have reported it; and a report date that is a date and is not in the future.
- Unit (`src/incident.rs`): the note names the serial, the holder, the certificate and its
  state; a deployment with no reporting address is told to set one; the PDF renders and its
  metadata carries no personal data.
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
  The tool prepares the note (phase 7); **who sends it, to whom, and within what deadline
  is process**, and the note says so rather than implying the tool has done it. The address
  is `AppSettings::report_incidents_to` — a deployment setting, because it is the unit's to
  state, and the same value the sealed-envelope slip prints for the holder.
- **Who may record a remediation, and does a revocation need a second pair of eyes?**
  *Owner: ESI.* A recorded revocation is a claim about somebody else's system, and today any
  operator with the register open can make it — the same position phase 5's reset ships in,
  for the same reason (`features/operator-auth-and-roles.md` has no roles yet). The trail
  names who claimed it and holds the CA's reference, so the claim is checkable; a second
  approver would be a gate on top of a working action rather than a change to it.
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
