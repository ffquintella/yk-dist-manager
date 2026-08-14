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

**Built** — a **Reports** screen with all seven reports, CSV and JSON export for every
one of them, PDF for the two that are handed to a person, and a one-click bundle that
writes the lot into a dated folder with a manifest. Every export writes `export.taken`.

Everything is **derived** on demand ([`src/report/`](../src/report/)): there is no report
table, no cached aggregate and nothing to refresh, for the same reason the signature
state and the dependency list are derived — a stored summary is a second truth about the
register, and the moment it disagrees with the records nobody can tell which one is wrong.

Two things are deliberately *not* built, and each says so where it would otherwise be
assumed:

- **Expected-versus-present reconciliation.** The unaccounted report says what the
  register cannot account for; it does not compare against what was purchased, because
  the tool has no procurement data. The report carries that sentence as a note, so a
  reader is not left thinking the comparison was made and passed.
- **A signature on the audit extract.** The extract carries the range, the chain head and
  the result of verification at export time, which is what makes it self-describing
  evidence. Signing it needs a private key this build does not hold — see
  [`features/audit-trail.md`](audit-trail.md) phase 4.

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

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | Inventory summary and custody report on screen | 2 | **Done** | aggregation over the cached views; custody counts a key handed out twice **once** |
| 2 | CSV export with an audited action and a scope header | 2 | **Done** | RFC 4180, `#` preamble naming the file, `export.taken` per file. A cell leading with `=`/`+`/`-`/`@` is neutralised: a certificate subject must not run as a formula when the file is double-clicked |
| 3 | Unaccounted / reconciliation report | 2 | **Done**, minus the comparison | Four kinds of gap in one table. "Expected versus present" needs procurement data this tool has never been given, and the report **says so** rather than looking exhaustive |
| 4 | Bootstrap compliance report | 2 | **Done** | no run, no *completed* run, superseded template version, template no longer in the catalogue |
| 5 | Certificate expiry report | 2 | **Done** | read out of the run's step details, so a register written before this release answers in full. A certificate whose validity cannot be parsed is **listed**, not dropped |
| 6 | Audit extract with verification statement, optionally signed | 2 | **Done**, unsigned | Range, chain head and verification-at-export-time, and it is produced *even when the chain does not verify* — refusing would leave the one person who has to investigate with nothing to look at. The signature is [`audit-trail.md`](audit-trail.md) phase 4 |
| 7 | JSON export | 2 | **Done** | rows as objects keyed by column name, so a consumer survives a column being added |
| 8 | Scheduled / one-click "monthly report" bundle | 2 | **Done**, one-click | Every report into a dated folder from **one** moment, with a manifest. Scheduling is deliberately not built: a schedule inside a desktop application fires only when somebody has it open, so it would look like a control and not be one — see [`src/report/bundle.rs`](../src/report/bundle.rs) |

## Audit events

| Event | Detail |
|---|---|
| `export.taken` | `report=custody format=csv rows=42 path=…` — one **per file**, including inside a bundle |
| `export.bundle` | `files=9 path=…` — the folder, in addition to the nine entries above, not instead of them |
| `report.viewed` | Not emitted. Read auditing is a classification decision, and the answer is level 2 |

## Tests

- Unit: aggregation over a fixture dataset produces the expected counts, including the
  awkward cases (a key returned and reissued must not be counted as held twice).
  — `src/report/mod.rs`
- Unit: a certificate whose validity cannot be parsed is listed rather than dropped, and
  an expired one sorts above a valid one.
- Unit: CSV quoting survives a comma and a quote, and a formula-leading cell is
  neutralised without its value being rewritten. — `src/report/export.rs`
- Unit: a half-typed date narrows nothing, and `until` means the end of that day.
- Behaviour: export a report → the file exists, its row count matches, and an
  `export.taken` audit entry was written. — `tests/behaviour_reports.rs`
- Behaviour: the bundle writes every promised file from one moment, with a manifest, and
  audits each one.
- Behaviour: exporting before generating writes no file and no audit entry.
- Secret sweep: **every report in every format it may leave in** is searched for
  `pin=`, `puk=`, `management_key=`, `access_code=` and `password`.
- Unit: the audit extract's verification statement reports failure when the chain is
  broken, and the extract is still produced.

## Open questions and gates

- **Where do exported files go, and who may read them?** *Still open, and now in front of
  whoever exports.* An export of the custody report is a list of people and the
  credentials they hold, and the screen says so before the file is written
  (`report::PERSONAL_DATA_WARNING`), with the location chosen deliberately rather than
  defaulted to a scratch directory. What is **not** decided is the unit's own rule about
  where those files are allowed to live and who may open them — an application cannot
  make that rule, and the **DPO** should be aware that the artefact exists.
- "Expected inventory" for reconciliation has to come from procurement data the tool does
  not have; the comparison stays unbuilt, and the report says so rather than reading as
  exhaustive.

## References

- `features/audit-trail.md`, `features/key-inventory.md`, `features/distribution-records.md`
- `docs/security-and-compliance.md`
