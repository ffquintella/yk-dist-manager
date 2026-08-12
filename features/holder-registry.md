# Feature: Holder registry

## Summary

The people who receive keys: name, corporate e-mail, unit, optional registration
id. Deliberately the smallest set of personal data the job needs.

## Motivation

Two things require a person record. The distribution record needs to name who holds
a key. The signing certificate needs the holder's e-mail, because an S/MIME or
document-signing certificate without a matching `rfc822Name` is not usable by the
mail client it is meant for.

Everything beyond that is personal data we would be holding without a purpose.
LGPD minimisation is the reason the record is this short, and the norm requires
documenting which tables hold personal data — see
`docs/security-and-compliance.md`.

## Current state

**Done for the basics.** `src/domain/holder.rs`, `src/ui/holders.rs`:

- `Holder::new` validates on construction: name, e-mail and unit are required and
  length-bounded; registration is optional.
- `validate_email` is deliberately strict (single `@`, bounded local part, dotted
  domain, no `..`, no leading/trailing dot) and normalises to lowercase, because the
  address ends up in a certificate SAN where an ambiguous value is a reissue.
- `email` carries a `UNIQUE` constraint: re-registering the same address updates the
  person instead of creating a duplicate.
- `certificate_subject(org, org_unit)` builds an RFC 4514 DN (`CN=…,OU=…,O=…`) with
  proper escaping of `, + " \ < > ; #` and leading/trailing spaces.
- `display()` renders `Name <email>` for tables and receipts.

Not yet done: AD/LDAP lookup, deactivation flow, and the LGPD-driven data-retention
work.

## Design

### Fields, and why each one exists

| Field | Purpose | Required |
|---|---|---|
| `full_name` | Certificate `CN`; the name on the term | yes |
| `email` | Certificate `rfc822Name` SAN; identifies the person | yes |
| `unit` | Certificate `OU`; who to ask when a key goes missing | yes |
| `registration` | Asset control where the unit requires it | no |
| `identification_number` | Named on the consignment term (CPF in Brazil, the local equivalent elsewhere) | no |
| `phone` | Contacting the holder about their key | no |
| `address` | Where a key was posted | no |
| `active` | A person who left; keeps history without suggesting they hold keys | yes (default true) |
| `created_at` | Record keeping | yes |

Still no photo, no date of birth, no bank details. The three optional fields were
added for one stated purpose — the consignment term names the holder and their
identification number — and each is empty unless the operator fills it in, which is
what makes the corresponding line disappear from a rendered term.

Adding a field still means adding a purpose, updating the data documentation, and
(for a new category) the DPO's assessment. An identification number is a step up in
sensitivity from a name and a work e-mail: it is optional precisely so a unit that
does not need it on the term never collects it.

### The e-mail is not in the DN

`certificate_subject` intentionally omits `emailAddress` from the DN. The
`emailAddress` RDN is deprecated for this purpose; modern clients match on
`rfc822Name` in the Subject Alternative Name. A unit test asserts the subject
contains no `@`, so a well-meaning change cannot quietly reintroduce it. Getting
the e-mail into the SAN is the subject of
`features/step-piv-signing-certificate.md`.

### AD integration (Phase 3)

The norm requires integration with the corporate Active Directory. For this tool
that means: type a partial name, pick from AD results, and fill name/e-mail/unit
from the directory rather than by hand — which also removes the "typo in the
certificate" failure mode. It needs an ESI-approved integration mechanism, so it is
a later phase rather than a startup dependency.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Record with validation, unique e-mail, RFC 4514 subject | Done | |
| 2 | Holders screen with the count of keys currently held | Done | |
| 2b | Optional identification number, phone and address | Done | schema v3, for the consignment term; a re-registration fills in and never blanks them |
| 3 | AD / LDAP lookup to fill the record | Todo | needs the ESI-approved integration |
| 4 | Deactivate a holder (and refuse new distributions to them) | Todo | `active` exists but is not enforced |
| 5 | Search and filter | **Done** | shipped as `features/gui-shell.md` phase 3: [`browse::holders`](../src/browse.rs) matches the name, e-mail, unit and registration, with sorting and paging |
| 6 | Per-holder view: keys held, history, bootstrap evidence | Todo | one screen answering "what does Ana have?" |
| 7 | Retention: what happens to a holder record when they leave | Todo | blocked on the DPO/retention decision |

## Audit events

| Event | When |
|---|---|
| `holder.registered` | A person was added, or an existing record updated |
| `holder.deactivated` | Phase 4 |
| `holder.imported` | Phase 3, filled from the directory |

## Tests

`tests/unit_domain.rs`:

- `holder_requires_name_email_and_unit`
- `email_validation_rejects_malformed_addresses` — 8 malformed, 3 valid
- `oversized_input_is_refused`
- `certificate_subject_is_rfc4514_and_excludes_the_email`
- `rfc4514_special_characters_are_escaped`
- `email_is_normalised_to_lowercase` (in `src/domain/mod.rs`)

`tests/behaviour_distribution.rs`:

- `scenario_the_same_person_is_not_duplicated_by_email`

## Open questions and gates

- **DPO gate**: this is personal data under the organisation's control. The privacy notice, the
  lawful basis and the retention period are the DPO's call, and the DCI assesses the
  system. Do not add data categories before that.
- Whether `registration` is required is a per-unit decision; it is optional in the
  model so a unit that does not use it is not forced to invent values.

## References

- `src/domain/holder.rs`, `src/ui/holders.rs`
- `docs/security-and-compliance.md`, `features/step-piv-signing-certificate.md`
