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

**Phases 1, 2, 3, 5, 6, 7 and 8 are built. Phase 4 (AD) is not, and is not the
implementer's to build** — `AGENTS.md` §8 makes every corporate-system integration the
ESI's decision, and this spec already says the mechanism needs their approval.

`YkDistApp::operator` is no longer a public, editable field: it is private, derived from
the session, and the Settings text input is gone. Roles are enforced by `Store` — a SQLite
authorizer on the connection plus `Store::require` for what is not a table write — so a UI
bug cannot bypass authorisation. Schema **v9** adds `operators` and `operator_sign_ins`.

**A register written before this release is unaffected until somebody switches the control
on.** An empty `operators` table is `Authority::Unenrolled`, in which nothing is refused;
the first administrator is created by a deliberate, audited first-run path on the Operators
screen. See *The first-run answer* below.

**Phase 3 is built, not hardware-verified.** The CTAP2 `authenticatorGetAssertion` exchange
is written against `ctap-hid-fido2` 3.5 and exercised end to end through
`device::write::MockWriter`; no key was attached to the workstation it was written on. This
is the same label PIV already carries in this repository.

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
| 3 | FIDO2 authentication with the operator's own YubiKey | **Built, not hardware-verified** | `Fido2Writer::get_assertion`; driven through `MockBackend`, no key attached. The assertion *signature* is not checked — see the gap below |
| 4 | AD authentication and group→role mapping | Todo | **ESI's, not the implementer's** (`AGENTS.md` §8). Deliberately not guessed at |
| 5 | Re-verification for sensitive operations | **Done** | 2-minute window; enforced in `Store::require`, not only at the button |
| 6 | Session lock, timeout, explicit logout | **Done** | locks at 5 min idle, ends at 30 |
| 7 | Operator management screen (admins only), fully audited | **Done** | plus the first-run administrator path |
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
| `operator.credential.changed` | `operator=… method=local\|fido2 by=…` — never the password, never a length |
| `operator.lockout.cleared` | `operator=… by=…` — an administrator lifted a lockout |
| `operator.session.locked` | `operator=… reason=idle\|explicit` |
| `operator.authorisation.refused` | `operator=… action=… authority=… reason=…` — a refusal is a security event in its own right: either somebody tried what they may not, or a screen offered a button it should have hidden |

The last six are additions to this table, made in the same commit as the code that writes
them. The first five were specified here from the start.

Login, account creation and account changes are the three events the norm requires to be
audited **always**; they are all here.

## Tests

All five the spec asked for are written, plus what the change owed:

| Test | Where |
|---|---|
| A distributor cannot edit a template; an auditor cannot write anything — **at the store layer** | `tests/unit_store_operators.rs` |
| Progressive lockout timings, exactly as specified | `src/operator/lockout.rs` |
| A failed login is audited with no password material in the entry | `tests/unit_store_operators.rs`, `tests/behaviour_operator_auth.rs` |
| A sensitive operation without re-verification is refused | `tests/unit_store_operators.rs`, `tests/behaviour_operator_auth.rs` |
| Argon2id at the agreed parameters, and nothing of the password in the stored value | `src/operator/credential.rs` |
| An unenrolled register refuses nothing, and the first-run path closes after one use | `tests/unit_store_operators.rs`, `tests/behaviour_operator_auth.rs` |
| The last administrator can be neither demoted nor disabled | `tests/unit_store_operators.rs` |
| An unknown username is refused in the same words as a wrong password | `tests/unit_store_operators.rs` |
| Failures against a username that does not exist are counted too | `tests/unit_store_operators.rs` |
| The audit actor becomes the authenticated identity | `tests/behaviour_operator_auth.rs` |
| A session locks on idle and ends on timeout | `src/operator/session.rs`, `tests/behaviour_operator_auth.rs` |
| Signing in with a key goes through `MockBackend`, with no key attached | `tests/behaviour_operator_auth.rs` |

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

- **The FIDO2 assertion signature is not verified.** The register checks that the
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
- **Phase 3 is not hardware-verified**, as stated above.

## References

- `src/app.rs` (`operator`), `src/ui/settings.rs`
- `docs/security-and-compliance.md`, `features/db-password-and-encryption.md`
