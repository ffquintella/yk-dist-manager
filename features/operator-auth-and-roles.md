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

The FGV norm is direct about this: a single authentication and authorisation point,
authorisation by profile or group rather than per user, MFA on sensitive operations, and
integration with the corporate Active Directory. Bootstrapping a security token is a
sensitive operation by any reading.

## Current state

**Not started.** `YkDistApp::operator` defaults to `$USER`/`$USERNAME` and is an editable
text field. It is honest about being a label — but it must not be mistaken for identity,
and the documentation says so.

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
| 1 | Roles in the data model + enforcement points | Todo | enforce in `Store`, not only in the UI |
| 2 | Local operator accounts with Argon2id + progressive lockout | Todo | break-glass path |
| 3 | FIDO2 authentication with the operator's own YubiKey | Todo | needs `native-fido` |
| 4 | AD authentication and group→role mapping | Todo | needs the ESI-approved mechanism |
| 5 | Re-verification for sensitive operations | Todo | touch-to-confirm |
| 6 | Session lock, timeout, explicit logout | Todo | shared workstation |
| 7 | Operator management screen (admins only), fully audited | Todo | |
| 8 | Remove the editable operator field | Todo | the gate for calling this feature done |

## Audit events

| Event | Detail |
|---|---|
| `operator.login` | `operator=… method=fido2|ad|local` |
| `operator.login.failed` | `operator=… method=… reason=… attempt=<n> lockout=<seconds>` |
| `operator.logout` | Explicit or timeout |
| `operator.role.changed` | `operator=… from=… to=… by=…` |
| `operator.reverified` | A sensitive operation was re-authorised |

Login, account creation and account changes are the three events the norm requires to be
audited **always**; they are all here.

## Tests

- Unit: role checks — a distributor cannot edit a template, an auditor cannot write
  anything, enforced at the store layer (so a UI bug cannot bypass it).
- Unit: progressive lockout timings, exactly as specified.
- Behaviour: a failed login is audited without any password material in the entry.
- Behaviour: a sensitive operation without re-verification is refused.
- Unit: password hashing uses Argon2id with the agreed parameters, and no password is
  logged.

## Open questions and gates

- **Is per-operator authentication in scope for the first production deployment**, or is
  "the workstation is physically controlled and everyone here is trusted" the accepted
  risk? That is a risk decision, and it should be written down either way rather than
  inherited by default.
- Argon2id parameters and the AD integration mechanism both need ESI approval.
- Whether the norm's progressive lockout (written for web login, with IP as the primary
  control) maps onto a desktop app — here there is no IP, only a workstation. Worth
  confirming with the ESI rather than improvising.

## References

- `src/app.rs` (`operator`), `src/ui/settings.rs`
- `docs/security-and-compliance.md`, `features/db-password-and-encryption.md`
