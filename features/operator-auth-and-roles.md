# Feature: Operator authentication and roles

## Summary

Know *who* is using the tool, and limit what they can do: administrator, distributor,
auditor. Today the operator name comes from `$USER`, which is a label, not
authentication.

## Motivation

Every audit entry records an actor. Right now that actor is whatever the OS says the
username is, editable in Settings, with no verification. The audit trail is therefore
only as strong as the assumption that whoever is at the workstation is who they claim to
be — which is exactly the assumption an audit trail is supposed to avoid needing.

The development norm (NRM) is direct about this: a single authentication and authorisation point,
authorisation by profile or group rather than per user, MFA on sensitive operations, and
integration with the corporate Active Directory. Bootstrapping a security token is a
sensitive operation by any reading.

## Current state

**Phases 1, 2, 3, 5, 6, 7 and 8 are built, called from the application, and
covered by tests. Phase 4 (AD) is not, and is not the implementer's** —
`AGENTS.md` §8 makes every corporate-system integration the ESI's decision, and
this spec already says the mechanism needs their approval.

Phase 7 gained two things in 2026-09-10 that the screen was missing: an
administrator can **set another operator's password** (the store method existed
and had no caller) and can **remove an account that is the actor on no audit
entry**. See *Disabling, and the one case it answers badly* below for where that
line is drawn and why it is not simply "administrators may delete".

`YkDistApp::operator` is no longer a public, editable field: it is private,
derived from the session, and the Settings text input is gone. Roles are enforced
by `Store` — a SQLite authorizer on the connection plus `Store::require` for what
is not a table write — so a UI bug cannot bypass authorisation. Schema **v9** adds
`operators` and `operator_sign_ins`.

**A register written before this release is unaffected until somebody switches the
control on.** An empty `operators` table is `Authority::Unenrolled`, in which
nothing is refused; the first administrator is created by a deliberate, audited
first-run path on the Operators screen. See *The first-run answer* below, and
`scenario_a_register_migrated_to_v9_refuses_nothing` in
`tests/behaviour_operator_auth.rs`, which is the test that must never be
weakened.

**Phase 3 is built, not hardware-verified.** The CTAP2 exchanges — both
`authenticatorMakeCredential` for registering an operator's key and
`authenticatorGetAssertion` for signing in with it — are written against
`ctap-hid-fido2` 3.5 and exercised end to end through `device::write::MockWriter`;
no key was attached to the workstation they were written on. This is the same
label PIV already carries in this repository. The assertion *signature* is still
not checked — see the gap below, verified against the code on 2026-08-28.

### What the second pass found and fixed (2026-08-28)

Everything above was claimed before it was true, and four of the claims were
wrong. Recorded here rather than quietly corrected, because a phase table that
has been wrong once is worth reading sceptically:

1. **The branch did not compile.** `src/ui/operators.rs` was written against an
   `egui-elegance` 0.15 API that does not exist (`CalloutTone::Warn`,
   `Button::primary()`). Six errors, in `HEAD` itself.
2. **The idle clock was never run.** `tick_session` and `touch_session` existed
   and no frame called either, so `last_activity` never advanced and no session
   ever locked or timed out. Phase 6 was a pair of constants nothing read.
3. **`Store::require` had six call sites, all six in operator management.**
   Editing a procedure or a term, factory-resetting an applet, changing the
   database password and taking an export all went through with a session of any
   age behind them — and for the reset and the re-key no authorizer can see them
   at all, so an auditor could have factory-reset a key. Phase 5's claim that it
   was "enforced in `Store::require`, not only at the button" was false for
   everything except the operator list.
4. **Phase 3 had no way in.** `sign_in_with_key` had no caller and
   `register_operator_credential` had nothing to feed it, so no operator could
   ever have held a credential and the sign-in was unreachable code.

Two smaller ones: `operator.authorisation.refused` recorded `reason=role` for
every refusal including a re-verification, and `require_reverification` set a
field that only the *signed-out* branch of the Operators screen painted, so a
signed-in operator got a status line and no prompt to answer it.

## Design

### Roles

| Role | Can | Cannot |
|---|---|---|
| **Administrator** | Everything, including editing templates, resetting applets, changing the database password, managing operators | — |
| **Distributor** | Read the inventory, register holders, run a bootstrap, record hand-overs and returns | Edit templates, reset applets, change settings that affect security |
| **Auditor** | Read everything, verify the audit chain, export reports | Change anything |

Authorisation is by role, never per user (the norm is explicit), and role membership is
itself an audited change.

### Authentication options, in order of preference

1. **The tool's own product** — authenticate the operator with a YubiKey. The unit
   distributing keys is the unit most able to hold one. Either FIDO2 (a credential
   registered for this application, verified locally over CTAP2 with UV) or PIV
   challenge-response against a certificate whose subject is the operator. This is
   self-consistent and gives MFA for free.
2. **Corporate AD** — username and password against the directory, with role mapping from
   AD groups. Satisfies the integration requirement, needs an ESI-approved mechanism, and
   is the natural fit if the workstation is already domain-joined.
3. **Local operator accounts** — a fallback: Argon2id password hashes in the database,
   with the progressive lockout the norm specifies (3 failures → 1 min, +2 → 15 min,
   +2 → 1 h). Least attractive, because it is a new credential store; acceptable only as a
   break-glass path.

Option 1 for daily use with option 2 for identity, if both are available, is the target.

### Disabling, and the one case it answers badly

An operator is **disabled, not deleted**, and the reason is the audit trail: every
entry names its actor as text, so deleting the account behind one leaves an entry
naming somebody the register has never heard of. Nobody can then ask who that was,
which is the question an audit trail exists to answer.

That reasoning covers everybody who *used* the register. It does not cover an
account enrolled by mistake — a mistyped username, somebody who turned out not to
be joining — which has no entries behind it and, under a blanket rule, sits in the
list for the life of the register with nothing standing behind it.

So the rule is the reasoning rather than a summary of it: **an operator who is the
`actor` on any audit entry cannot be removed, and one who is on none can.** The
refusal names disabling as the alternative. A failed sign-in is recorded against
`(not signed in)` and therefore does not count, which is deliberate — a typo at the
sign-in box must not be able to make an account permanent. Two further refusals,
both the same ones `set_operator_active` already makes: the account the session is
signed in as, and the register's last administrator.

Removing takes the password, the registered credential and the `operator_sign_ins`
row with it. The lockout row in particular: one left behind would be waiting on a
username that no longer exists, and would apply to whoever is enrolled under it
next.

### Replacing somebody else's password

An operator who forgot their password has no other way back into a register that
may have no second administrator. `Store::set_operator_password` was written for
this from the start and had **no caller** until phase 7's second pass; it is now on
the row, behind the same re-verification as every other change there.

Two things are said on the screen rather than only here, because both are the kind
of weakness that is only compensated for if somebody knows about it: from the
moment the password is set until the operator changes it, **an administrator knows
their password**, and the new one should be given in person rather than by e-mail.
The audit entry says a credential changed, by which method, and who changed it —
never the password, never its length. Setting one clears the failure history,
because the credential those failures were counted against is gone.

### Consequences elsewhere

- The audit `actor` becomes an authenticated identity, and the Settings field disappears.
- Sensitive operations (template edit, applet reset, password change, export of personal
  data) require a **re-verification** — touch the key again — not just a session.
- A shared workstation needs a lock/logout, and a session timeout.
- The database password (`features/db-password-and-encryption.md`) is a
  confidentiality-at-rest control and is *not* operator authentication. Both are needed;
  neither substitutes for the other. That distinction has to be stated in the UI, or
  someone will assume one password is doing both jobs.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Roles in the data model + enforcement points | **Done** | `operator::Role`/`Action`; enforced by a SQLite authorizer on the connection **and** `Store::require`, never only in the UI |
| 2 | Local operator accounts with Argon2id + progressive lockout | **Done** | break-glass path; parameters are documented defaults **pending ESI ratification** |
| 3 | FIDO2 authentication with the operator's own YubiKey | **Built, not hardware-verified** | Both halves reachable: `register_key_for_operator` makes the credential, `sign_in_with_key` proves possession. Driven through `MockWriter`, no key attached. The assertion *signature* is not checked — see the gap below |
| 4 | AD authentication and group→role mapping | Todo | **ESI's, not the implementer's** (`AGENTS.md` §8). Deliberately not guessed at |
| 5 | Re-verification for sensitive operations | **Done** | 2-minute window. Enforced at the write: `Store::require` for the applet reset, the re-key and the three export paths; `Store::require_fresh_credential` for a procedure or a term, which leaves the *role* refusal to the authorizer that already covers those tables |
| 6 | Session lock, timeout, explicit logout | **Done** | locks at 5 min idle, ends at 30, and the clock is run from the frame loop — an idle session asks for a repaint every 15 s, because egui sleeps when idle |
| 7 | Operator management screen (admins only), fully audited | **Done** | plus the first-run administrator path, replacing another operator's password, and removing an account that wrote nothing |
| 8 | Remove the editable operator field | **Done** | `YkDistApp::operator` is private and derived; the Settings input is gone |

### The first-run answer

A migration cannot create an administrator, because it would have to invent a credential
for one; and a migration that *demanded* one before opening would make every existing
register unopenable until somebody read a release note. A control that locks an operator
out of their own register is a worse outcome than the control being absent.

So the register has three authority states, and the middle one is the whole answer:

| State | When | What is refused |
|---|---|---|
| `Unenrolled` | `operators` is empty — every register migrated to v9 | **Nothing.** The actor is the workstation user, said on screen to be a label |
| `SignedOut` | operators exist, nobody signed in | everything but reading and the sign-in's own bookkeeping |
| `SignedIn(role)` | | whatever the role matrix says |

Turning it on is a deliberate act by somebody at the keyboard, audited as
`operator.enrolled … first=true`, and it is the same shape as the database password and
the template-signature policy: both are off until a deployment turns them on, and both say
so on screen while they are off.

## Audit events

| Event | Detail |
|---|---|
| `operator.login` | `operator=… method=fido2|ad|local` |
| `operator.login.failed` | `operator=… method=… reason=… attempt=<n> lockout=<seconds>` |
| `operator.logout` | Explicit or timeout |
| `operator.role.changed` | `operator=… from=… to=… by=…` |
| `operator.reverified` | `operator=… action=… method=…` |
| `operator.enrolled` | `role=… method=… first=<bool>` — account creation, which the norm requires always |
| `operator.enabled` / `operator.disabled` | `operator=… role=… by=…` — account change |
| `operator.removed` | `operator=… role=… by=…` — the account is gone; only ever written for one that is the actor on no entry, so the trail it leaves behind names nobody |
| `operator.credential.changed` | `operator=… method=local\|fido2 by=…` — never the password, never a length |
| `operator.lockout.cleared` | `operator=… by=…` — an administrator lifted a lockout |
| `operator.session.locked` | `operator=… reason=idle\|explicit` |
| `operator.authorisation.refused` | `operator=… action=… authority=… reason=…` — a refusal is a security event in its own right: either somebody tried what they may not, or a screen offered a button it should have hidden |

The last six are additions to this table, made in the same commit as the code that writes
them. The first five were specified here from the start.

Login, account creation and account changes are the three events the norm requires to be
audited **always**; they are all here.

## Tests

All five the spec asked for are written, plus what the change owed. The two
layers are tested apart: `unit_store_operators` never builds a `YkDistApp`, and
`behaviour_operator_auth` only ever goes through one.

| Test | Where |
|---|---|
| A distributor cannot edit a template; an auditor cannot write anything — **at the store layer**, refused by SQLite | `tests/unit_store_operators.rs`, `tests/behaviour_operator_auth.rs` |
| Progressive lockout timings, exactly as specified | `src/operator/lockout.rs` |
| A failed login is audited with no password material in the entry | `tests/unit_store_operators.rs`, `tests/behaviour_operator_auth.rs` |
| A sensitive operation without re-verification is refused | `tests/unit_store_operators.rs`, `tests/behaviour_operator_auth.rs` |
| Argon2id at the agreed parameters, and nothing of the password in the stored value | `src/operator/credential.rs` |
| An unenrolled register refuses nothing, and the first-run path closes after one use | `tests/unit_store_operators.rs`, `tests/behaviour_operator_auth.rs` |
| The last administrator can be neither demoted nor disabled | `tests/unit_store_operators.rs` |
| An unknown username is refused in the same words as a wrong password | `tests/unit_store_operators.rs`, `tests/behaviour_operator_auth.rs` |
| Failures against a username that does not exist are counted too | `tests/unit_store_operators.rs` |
| The audit actor becomes the authenticated identity, and reverts to a name no person owns on sign-out | `tests/behaviour_operator_auth.rs` |
| A session locks on idle and ends on timeout | `src/operator/session.rs`, `tests/behaviour_operator_auth.rs` |
| Signing in with a key goes through `MockWriter`, with no key attached | `tests/behaviour_operator_auth.rs` |

Added by the second pass, because each of these is a check that existed and was
not called:

| Test | Where |
|---|---|
| A procedure and a term ask for the credential again **at the write itself** | `tests/unit_store_operators.rs` |
| Retiring, reinstating and deleting a version are as sensitive as writing one | `tests/unit_store_operators.rs` |
| The role refusal on a template still comes from the database, so the authorizer stays exercised | `tests/unit_store_operators.rs` |
| Opening a register still seeds its built-in procedures — the seeds run before anybody signs in | `tests/unit_store_operators.rs` |
| An export is refused, prompts, and goes through once the credential is given — and the refusal is recorded as `reason=reverification` rather than `reason=role` | `tests/behaviour_operator_auth.rs` |
| A registered key's counter that does not advance is refused as a clone | `tests/behaviour_operator_auth.rs` |
| The whole trail contains no password and no PIN, in every scenario | `tests/behaviour_operator_auth.rs` |

Added with removal and the password reset:

| Test | Where |
|---|---|
| An operator who wrote nothing is deleted, audited, and their username freed | `tests/unit_store_operators.rs` |
| An operator who wrote an audit entry can only be disabled, and the refusal says so | `tests/unit_store_operators.rs` |
| A failed sign-in does not make an account permanent | `tests/unit_store_operators.rs` |
| Neither the last administrator nor the signed-in account can be removed | `tests/unit_store_operators.rs` |
| A distributor can neither remove an operator nor set anybody's password | `tests/unit_store_operators.rs` |
| A replacement password works, lifts the lockout, and leaves nothing of itself on the trail | `tests/unit_store_operators.rs`, `tests/behaviour_operator_auth.rs` |
| Both new writes ask for the credential again | `tests/unit_store_operators.rs` |
| End to end: a mistyped enrolment is removed and an operator with history is disabled instead | `tests/behaviour_operator_auth.rs` |

## Open questions and gates

Each of these is written down rather than answered, per `AGENTS.md` §8. Everything that
does not depend on them is built.

### Pending the ESI

1. **Argon2id parameters.** Built with `m=19456 KiB, t=2, p=1, 16-byte salt, 32-byte key`
   — the **OWASP Password Storage Cheat Sheet**'s stated minimum for Argon2id. RFC 9106's
   first recommendation (`m=2 GiB, t=1, p=4`) was rejected for this deployment: two
   gibibytes per sign-in is not a cost a unit's laptop can pay without the application
   appearing to hang, and a parameter set the first operator lowers is worse than one
   chosen for the machine. RFC 9106's *second* recommendation (`m=64 MiB, t=3, p=4`) is
   the obvious upgrade if the ESI wants one.
   **These are reasonable defaults, not approved parameters**, handled exactly as the
   SQLCipher KDF parameters are in `db-password-and-encryption.md`. They travel inside the
   stored PHC string, so raising them costs a constant and a re-hash on next sign-in, not
   a migration. `credential::parameter_summary()` says "pending ESI ratification" on the
   screen where a password is chosen, and a test asserts that it does.

2. **AD authentication and group→role mapping (phase 4).** Deliberately **not built**.
   `AGENTS.md` §8 makes integration with a corporate system the ESI's decision, and this
   spec already says the mechanism needs their approval — so guessing at one (LDAP simple
   bind? Kerberos? which DC? which group naming?) would be inventing an architecture
   security premise. `AuthMethod::Directory` exists so a record can name the method, and
   nothing produces it. What is needed from the ESI: the mechanism, the directory to
   reach, and the group→role mapping.

3. **Does the norm's progressive lockout map onto a desktop application?** It is written
   for a web login where the primary control is the source IP and a lockout costs an
   attacker a botnet. Here there is no IP — one workstation, physically in the unit, and
   the person being slowed down is very often the operator who mistyped. Implemented
   **exactly as specified** (3 → 1 min, +2 → 15 min, +2 → 1 h) rather than improvised, and
   made survivable by giving administrators an audited `operator.lockout.cleared`. Worth
   confirming rather than inheriting.

4. **Is per-operator authentication in scope for the first production deployment**, or is
   "the workstation is physically controlled and everyone here is trusted" the accepted
   risk? Still a risk decision, and now a *reversible* one: the register runs unenrolled
   until somebody creates the first administrator, so the answer is a decision rather than
   a release.

5. **Is the session timeout right?** 5 minutes to lock, 30 to end, 2 minutes of
   re-verification. Chosen for a hand-over desk; a unit that works differently will find
   them wrong in one direction or the other.

### Known gaps in what *is* built

- **The FIDO2 assertion signature is not verified.** *Confirmed still true on
  2026-08-28 by reading `Store::sign_in_with_credential`: the four checks there are
  the credential id, the relying party, `user_verified` and the counter, and
  `AssertedCredential` carries no signature field at all.* The register checks that the
  authenticator answering is the one registered for that operator, that it reported user
  verification (a PIN or a biometric, not merely a touch), and that its signature counter
  advanced. It does **not** verify the signature itself, because that needs the
  credential's COSE public key — which `make_credential` does not currently surface — and
  a P-256 verifier this build does not carry. What is proved today is possession of an
  authenticator that answers for that credential id with UV, over USB, on this
  workstation. That is the spec's own words ("verified locally over CTAP2 with UV") and it
  is less than a WebAuthn relying party does. Closing it means carrying the public key on
  `CredentialEvidence` and adding a P-256 dependency.
- **Re-verification is by password even for an operator who signed in with a key.** The
  prompt is there and the refusal is enforced; the second factor for a key-holder is
  currently the same credential rather than a fresh touch.
- **Phase 3 is not hardware-verified**, as stated above — and now for two
  exchanges rather than one, since registering a credential is as unverified as
  asserting with it.
- **A key is addressed by a typed serial when it is registered.** The screen asks
  for the serial rather than reading whichever key is attached, which is the same
  confirmation the factory reset asks for and is deliberate: a credential written
  to the wrong key is one an operator cannot sign in with and cannot easily find.
  It does mean the Operators screen runs no device watch, so an operator has to
  read the serial off the key or off the Inventory screen.
- **`Action::ChangeSecuritySettings` has no call site.** The action exists, the
  role matrix answers for it and `Store::require` would refuse it, and nothing in
  `src/app.rs` asks. The settings it names — the transport, the audit mirror, the
  trusted signing keys — are written through `AppSettings`, which is a file on the
  workstation rather than a table in the register, so the store is not on the path
  at all. Closing it properly means deciding whether a workstation-level setting
  is a *register* authorisation question, which is an architecture premise
  (`AGENTS.md` §8) rather than a missing line.

## References

- `src/operator/` (the model), `src/store/operators.rs` (the refusals),
  `src/ui/operators.rs` (the screen), `src/app.rs` (`operator`, `may`,
  `tick_session`), `src/ui/settings.rs` (where the typed field used to be)
- `tests/unit_store_operators.rs`, `tests/behaviour_operator_auth.rs`
- `docs/security-and-compliance.md`, `docs/gui.md`, `docs/operations.md`,
  `features/db-password-and-encryption.md`
