# Feature: Reports and export

## Summary

The questions the dataset exists to answer, answered on screen and exportable: what do
we own, who holds it, what is unaccounted for, what expires soon, and what does the
audit trail say.

## Motivation

Records that cannot be summarised do not get used. The specific questions that come up:

- **Asset**: how many keys, of which models, in which states, from which batches?
- **Custody**: who holds a key right now, and for how long?
- **Unaccounted**: which keys are neither in stock nor in someone's hands?
- **Compliance**: which distributed keys have no bootstrap run attached, or ran a
  template version we have since fixed?
- **Expiry**: which signing certificates expire in the next 60 days?
- **Audit**: the trail for a date range, exportable with a verification statement, which
  is what the ESI asks for.

## Current state

**Not started.** The screens show flat tables; there is no export and no aggregation.

## Design

### Reports

| Report | Content | Primary use |
|---|---|---|
| Inventory summary | Counts by status, model, firmware, batch; FIPS split | Asset review |
| Custody | Open distributions: serial, holder, unit, days held, receipt state | "Who has what" |
| Unaccounted | `Distributed` with no open distribution, `Lost`, `Returned` but not sanitised | Reconciliation |
| Bootstrap compliance | Distributed keys with no run, failed runs, superseded template versions | Quality |
| Certificate expiry | Certificates by `not_after`, with the holder | Renewal planning |
| Audit extract | Entries for a range, with a chain-verification statement | ESI requests |
| Custody model | Which keys have escrowed secrets vs forced-change | `features/secrets-custody.md` |

### Export

- **CSV** for spreadsheets, since that is where the data will be cross-checked against
  procurement.
- **JSON** for anything programmatic.
- **PDF** only for the audit extract and the compliance report, where the artefact is
  handed to someone.

Export rules:

- Every export is **audited** with what was exported and by whom (`export.taken`). The
  norm treats export of critical data as an operation to be audited, and this export
  contains personal data.
- Exports never contain a secret. There is no secret to contain, which is the point of
  `features/secrets-custody.md`, and a test asserts it anyway.
- The export names its own scope and generation time in a header row/field, so a
  spreadsheet found on a share six months later can be dated.
- Personal data in an exported file leaves the application's protection. The export
  dialog must say so, and the file should default to a location the operator chooses
  deliberately rather than a scratch directory.

### Audit extract

Special handling: the extract carries the sequence range, the chain head hash, and the
result of verification at export time. That makes it self-describing evidence rather
than a table someone could have edited after the fact. Signing the extract is Phase 6.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Inventory summary and custody report on screen | Todo | aggregation over the cached views |
| 2 | CSV export with an audited action and a scope header | Todo | |
| 3 | Unaccounted / reconciliation report | Todo | needs procurement input for "expected" |
| 4 | Bootstrap compliance report | Todo | depends on the executor producing real runs |
| 5 | Certificate expiry report | Todo | needs certificate fields stored on the run |
| 6 | Audit extract with verification statement, optionally signed | Todo | `features/audit-trail.md` Phase 4 |
| 7 | JSON export | Todo | |
| 8 | Scheduled / one-click "monthly report" bundle | Todo | |

## Audit events

| Event | Detail |
|---|---|
| `export.taken` | `report=custody format=csv rows=42 path=…` |
| `report.viewed` | Optional; only if the classification requires read auditing |

## Tests

- Unit: aggregation over a fixture dataset produces the expected counts, including the
  awkward cases (a key returned and reissued must not be counted as held twice).
- Behaviour: export a report → the file exists, its row count matches, and an
  `export.taken` audit entry was written.
- Secret sweep: no export output contains any generated secret from a mock run.
- Unit: the audit extract's verification statement reports failure when the chain is
  tampered with.

## Open questions and gates

- **Where do exported files go, and who may read them?** An export of the custody report
  is a list of people and the credentials they hold. This needs an answer before Phase 2
  ships, and the DPO should be aware.
- "Expected inventory" for reconciliation has to come from procurement data the tool does
  not have; Phase 3 needs a source or an import.

## References

- `features/audit-trail.md`, `features/key-inventory.md`, `features/distribution-records.md`
- `docs/security-and-compliance.md`
