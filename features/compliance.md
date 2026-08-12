# Feature: Compliance artefacts

## Summary

The documents the institutional *system acquisition, development and maintenance norm*
(NRM) requires for a system like this one: classification, registration with the ESI, data
documentation, change records, and homologation.

## Motivation

The norm is binding and carries sanctions from the CSI for non-compliance. A system in
use without ESI registration is a finding, not a pending detail. Producing these artefacts
is part of the work, not paperwork appended to it — and several of them (the data
documentation especially) are genuinely useful: they are the only place that answers
"where is the personal data in this thing?".

Working code that cannot be deployed because its registration is missing is not delivered
work.

## Current state

**Not started.** `docs/security-and-compliance.md` maps the norm's rules onto the
codebase and proposes a classification, but no artefact has been produced or submitted.

## Design

### Classification (proposal, subject to ESI validation)

Data processed: names, corporate e-mails, organisational units of employees; serial
numbers of security tokens; the record of which credential material is on which token;
and — depending on the custody decision — potentially the credentials themselves.

| Reading | Level |
|---|---|
| Ordinary personal data (name, e-mail, unit) | 2 |
| Security-relevant data, strategically sensitive to the institution: the token↔person map and what is on each token | **3** |
| If secrets are ever escrowed here | 3, arguably 4 — so we do not |

**Set: level 2** *(2026-08-11, the owner's decision).* This feature proposed 3, arguing that
the map of who holds which credential is the reconnaissance an attacker wants, and so sits
above "ordinary personal data". The owner placed it at 2 instead: the directory fields are
ordinary, and the map is protected by the controls rather than by the level.

The table above is left as written, because it is the argument, not the outcome — and its
last row still stands: **escrow would move the level**, which is one more reason model B
retains nothing ([`secrets-custody.md`](secrets-custody.md)).

The controls that level 3 would have implied — immutable hash-chained audit, encryption at
rest — are built and kept regardless. They are cheap to keep and awkward to argue for
re-adding later.

### Artefacts required

| Artefact | Content | Template |
|---|---|---|
| System registration | Name, level and justification, responsible analyst, business owner, unit, coordinator, publication owner, homologation owners (TIC and user), infrastructure owner, environments and servers, integrations, password-change locations and procedures, ESI authorisation | `templates/registro-de-sistema.md` in the secure-development skill |
| Data documentation | Which tables hold personal or sensitive data, and which fields | `templates/documentacao-de-dados.md` |
| Change document | What changed and where, for every version installed anywhere | `templates/documento-de-mudanca.md` |
| Homologation report | Evidence that the change was validated before production | `templates/relatorio-de-homologacao.md` |
| Audit mechanism documentation | How the audit trail works, delivered to the ESI on request | this repository: `features/audit-trail.md` + `docs/security-and-compliance.md` |

The data documentation is short here by design: `holders` (name, e-mail, unit,
registration) and the denormalised `holder_display` on `distributions`. Keeping that list
short is a design goal, and this artefact is where it is visible.

### Approval gates

Cannot be self-approved. From `docs/security-and-compliance.md`:

| Gate | Owner |
|---|---|
| Architecture security premises | ESI |
| Pre-production security verification (every version) | ESI |
| Privacy notice, lawful basis, consent | DPO |
| Assessment of a system processing personal data under the organisation's control | DCI |
| Every integration mechanism (AD, CA, BastionVault) | ESI |
| Audit and log retention | ESI |
| Classification level | ESI |

### Known gaps to declare rather than hide

1. **Audit segregation** — the norm wants audit data in a separate instance; the design is
   one file with trigger-enforced immutability plus an optional mirror. Declare it and ask.
2. **Operator authentication** — `$USER` is a label, not authentication, until
   `features/operator-auth-and-roles.md` lands. Declare it; do not let a reader assume
   otherwise.
3. **AD integration** — required by the norm, not yet built.
4. **G-002 v2.0 (July 2026)** could carry more specific requirements (OWASP ASVS, NIST
   SSDF, DevSecOps); the version this repository was written against could not be read.
   Ask the ESI for the current text before homologation.

Declaring gaps is the process the norm anticipates (a documented adequacy plan), and it is
better than a claim of conformance that does not survive a look at the code.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Classification proposal with justification | Todo | proposal drafted in `docs/security-and-compliance.md` |
| 2 | System registration submitted to the ESI | Todo | needs the named owners |
| 3 | Data documentation | Todo | short, and kept in sync with the schema |
| 4 | Audit mechanism document for the ESI | Todo | largely already written |
| 5 | Change document template wired into the release process | Todo | `features/packaging-and-release.md` |
| 6 | Homologation report for the first production version | Todo | |
| 7 | Declared-gaps list with an adequacy plan and dates | Todo | the four gaps above |
| 8 | Ask the ESI for G-002 v2.0 and reconcile | Todo | may add requirements |

## Audit events

None in the application. These are process artefacts.

## Tests

Not testable in code. The check is a review before each production version: does the
change document exist, is the data documentation still accurate against the schema, and is
the declared-gaps list current?

Phase 3 is worth automating partially: a test that fails when a new column appears in
`holders` (or any new table stores personal data) without the data documentation being
updated in the same commit.

## Open questions and gates

Everything in this feature is a gate. The named owners (analyst, business owner,
coordinator, publication and homologation owners, infrastructure owner) have to be filled
in by a person, not inferred.

## References

- `docs/security-and-compliance.md`
- NRM v2 (April 2021); G-002 v1.0 (June 2013); G-002 v2.0 (July 2026, unread)
- The secure-development skill templates under `templates/`
