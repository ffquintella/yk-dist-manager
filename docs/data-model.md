# Data model

Schema **v8**, tracked in `PRAGMA user_version`. One SQLite file holds everything.
Source of truth: `SCHEMA_V1` in [`src/store/mod.rs`](../src/store/mod.rs).

Conventions:

- Ids are UUIDs stored as hyphenated `TEXT`.
- Timestamps are RFC 3339 UTC `TEXT` (`2026-08-10T14:32:05+00:00`), parsed explicitly.
- Enums are stored as readable snake_case `TEXT` (`in_stock`, `in_person`), mapped by
  functions in `store` — not as JSON-quoted debug output.
- Lists and nested structures are JSON `TEXT`.
- Natural keys carry `UNIQUE`, so re-reading a key or re-registering a person updates
  rather than duplicates.

---

## `keys` — the physical inventory

| Column | Type | Notes |
|---|---|---|
| `id` | TEXT PK | UUID; referenced by `distributions` |
| `serial` | INTEGER **UNIQUE** | Yubico serial — the natural key |
| `model` | TEXT | `YubiKey 5 NFC`; on the native path, the reader name |
| `firmware` | TEXT | `5.4.3`; drives the capability gates |
| `form_factor` | TEXT | `Keychain (USB-A)`; empty on the native path (management applet not covered) |
| `fips` | INTEGER | Inferred from the model name today |
| `applications` | TEXT (JSON) | USB-enabled applications: `["FIDO2","PIV",…]`. **Empty means never read**, not "none enabled" — the flags live in the management applet, which only the `ykman` path reports — and the pre-flight treats it that way: it skips a step for a *missing* application only when the list is non-empty |
| `status` | TEXT | `in_stock` \| `bootstrapped` \| `distributed` \| `returned` \| `lost` \| `retired` |
| `batch` | TEXT | Purchase/invoice reference |
| `notes` | TEXT | The operator's **observation**, edited on the Inventory screen; bounded by `domain::MAX_NOTE`, never overwritten by a device re-read, and never a place for a secret |
| `serial_source` | TEXT | **v2.** How the serial was learned: `device` (verified), `scanned-label`, `manual-entry`. Only ever improves — a device read upgrades it, a later scan never downgrades it |
| `created_at`, `updated_at` | TEXT | |

Transitions are enforced in `domain::KeyStatus::can_transition_to` and refused by
`Store::set_key_status`. `in_stock → distributed` is deliberately illegal, and the
Distribution screen asks the lifecycle before it inserts the hand-over rather than
after. `in_stock → bootstrapped` is performed by a bootstrap run that settles
`Completed`, not by the operator remembering a button.

A row can be **deleted** (`Store::delete_key`), but only as the correction of an
intake mistake: the store refuses a serial that any row in `distributions` or
`bootstrap_runs` refers to, because a hand-over pointing at a serial nobody can look
up is not a register. Taking a key out of service is `retired`, which keeps the row.
Either way the `audit` entries stay — the trail is append-only by trigger, so
`key.added` and `key.removed` outlive the row they describe.

## `holders` — the people

**This is the personal-data table.** Any change here updates
[security-and-compliance.md](security-and-compliance.md) and the organisation's data documentation.

| Column | Type | Notes |
|---|---|---|
| `id` | TEXT PK | UUID |
| `full_name` | TEXT | Certificate `CN`; required |
| `email` | TEXT **UNIQUE** | Certificate `rfc822Name` SAN; lowercased on entry; required |
| `unit` | TEXT | Certificate `OU`; required |
| `registration` | TEXT | Optional |
| `identification_number` | TEXT | **v3.** Optional. CPF or the local equivalent; printed on the consignment term. Named for what it is, not for one country's document |
| `phone` | TEXT | **v3.** Optional |
| `address` | TEXT | **v3.** Optional; for a key sent by post |
| `active` | INTEGER | Default 1; enforcement is a later phase |
| `created_at` | TEXT | |

An optional field is **filled in, never blanked**: re-registering the same e-mail with
the required fields only leaves the identification number, phone and address intact.

## `bootstrap_runs` — what was applied

| Column | Type | Notes |
|---|---|---|
| `id` | TEXT PK | UUID; referenced by `distributions` |
| `key_serial` | INTEGER | Which key |
| `holder_id` | TEXT → `holders(id)` | Nullable: stock preparation has no holder |
| `template_id`, `template_version` | TEXT | Which procedure, at which version |
| `operator` | TEXT | Who ran it |
| `started_at` | TEXT | |
| `finished_at` | TEXT | Null while running |
| `status` | TEXT (JSON enum) | `Planned` \| `Running` \| `Completed` \| `Failed` \| `Aborted` |
| `custody` | TEXT | The custody model: `transport-pin+forced-change` (the decided default), `holder-set`, `escrowed:<external reference>`, or `no-secret-set`. Never a secret value |

## `bootstrap_run_steps` — what each step did (v5)

One row per step of a run, replacing the `bootstrap_runs.steps` JSON blob that v1
to v4 carried.

| Column | Type | Notes |
|---|---|---|
| `run_id` | TEXT → `bootstrap_runs(id)` | PK with `position` |
| `position` | INTEGER | The template's order; PK with `run_id` |
| `step_id` | TEXT | The template's step id, e.g. `piv-csr` |
| `kind` | TEXT | `StepKind::slug` — `fido2-pin`, `piv-keygen`, … |
| `status` | TEXT | `pending` \| `running` \| `done` \| `failed` \| `skipped` |
| `started_at`, `finished_at` | TEXT NULL | |
| `detail` | TEXT | Operator-facing, secret-free |

Indexed on `(kind, status)`, which is the question a report asks: "how many keys
got a signing certificate?"

Why rows rather than a blob:

* **Step-level reporting** becomes a `GROUP BY` instead of parsing every run.
* **The executor writes one outcome at a time.** Rewriting a whole blob per step
  means a run interrupted mid-write loses the steps that had already succeeded —
  which is exactly the record that matters when a key was half-configured.
* **The stored spellings match the rest of the schema** (`fido2-pin`, `done`)
  rather than serde's variant names (`Fido2Pin`, `Done`), so the file stays
  answerable from a SQL console during an audit.

`StepOutcome`: `step_id`, `kind`, `status`, `started_at`, `finished_at`, `detail`.
`detail` is operator-facing text and must be secret-free. For a secret-setting step it also
carries the change enforcement (`enforced-by-firmware` / `instructed-on-handover`), which is
what distinguishes a key that obliges the holder to replace the transport PIN from one that
merely asks.

## `distributions` — the hand-over

| Column | Type | Notes |
|---|---|---|
| `id` | TEXT PK | UUID |
| `key_id` | TEXT → `keys(id)` | |
| `key_serial` | INTEGER | Denormalised on purpose (see below) |
| `holder_id` | TEXT → `holders(id)` | |
| `holder_display` | TEXT | Denormalised: `Name <email>` at the time of hand-over |
| `distributed_at` | TEXT | |
| `distributed_by` | TEXT | The operator who handed it over |
| `method` | TEXT | `in_person` \| `courier` \| `post` |
| `receipt_ref` | TEXT | Signed term / tracking reference |
| `bootstrap_run_id` | TEXT → `bootstrap_runs(id)` | The evidence of what was applied |
| `returned_at` | TEXT | Null while the key is out |
| `returned_to` | TEXT | Who received it back |
| `notes` | TEXT | |

**Why denormalise `key_serial` and `holder_display`:** a distribution report must remain
readable and *historically stable*. If a holder's name is corrected next year, last year's
hand-over record should still say what was on the signed term.

Indexes: `idx_distributions_serial`, `idx_distributions_holder`, `idx_runs_serial`.

## `templates` — the procedures

| Column | Type | Notes |
|---|---|---|
| `id` | TEXT | e.g. `org-standard`; lower-case, digits and hyphens (`template::check_id`) |
| `version` | TEXT | e.g. `1` |
| `name` | TEXT | Display name |
| `body` | TEXT (JSON) | The full `BootstrapTemplate` |
| `updated_at` | TEXT | |
| `retired_at` | TEXT NULL | Set: withdrawn from the wizard, kept on record (v4) |
| | | **PRIMARY KEY (`id`, `version`)** |

An edited procedure is a new *version*, never a mutation of one that has already run.
The **Templates** screen writes here through `Store::save_template_version`, which — like
its term counterpart — reads the versions on record for that id and inserts one past the
highest number, so the version a bootstrap run recorded stays exactly as it was and the
number on the operator's screen cannot decide what gets written. The id itself is
immutable once stored: a run refers to it.

Built-ins are seeded idempotently and never overwrite an edited template of the same id and
version.

`retired_at` is why the wizard and the catalogue read different things:
`Store::templates()` returns the rows where it is NULL (what may be offered), and
`Store::template_catalogue()` returns every row with its run count (what the Templates
screen manages). It is a **column and not a field in `body`** because `body` is the
template format — shared with the future import/export — while retirement is this
deployment's opinion about a template, and because it has to be queryable. Retirement is
also what makes a withdrawal survive the built-in seeding that runs on every open: seeding
asks whether `(id, version)` exists, not whether it is in use.

`Store::delete_template` deletes a row outright, and refuses when a bootstrap run recorded
that version or when this build ships it — see
[`../features/bootstrap-templates.md`](../features/bootstrap-templates.md).

## `term_templates` — the consignment term, per language

| Column | Type | Notes |
|---|---|---|
| `id` | TEXT | e.g. `consignment` |
| `language` | TEXT | BCP 47, e.g. `pt-BR`, `en` |
| `version` | TEXT | An edit is a new version; a signed version stays readable |
| `title` | TEXT | Document heading |
| `body` | TEXT | The document, with `{{variables}}` |
| `updated_at` | TEXT | |
| | | **PRIMARY KEY (`id`, `language`, `version`)** |

Rendering drops any line whose placeholder resolves to empty, which is how the
optional holder fields stay optional — see
[`../features/consignment-terms.md`](../features/consignment-terms.md).

The **Terms** screen writes here through `Store::save_term_template_version`, which
never updates a row: it reads the versions on record for that `(id, language)` and
inserts one past the highest number. So an edit adds `2` beside `1`, the version a
holder may already have signed stays exactly as it was, and the version number the
editor happened to be showing cannot decide what gets written. `term::choose_template`
hands out the **newest** version of a language, which is what makes an edit take effect
on the next term generated.

## `documents` — the signed term and anything filed with it

| Column | Type | Notes |
|---|---|---|
| `id` | TEXT PK | UUID |
| `distribution_id` | TEXT → `distributions(id)` | A document cannot be filed against a hand-over that does not exist |
| `kind` | TEXT | `signed-term` \| `generated-term` \| `return-receipt` \| `other` |
| `filename` | TEXT | Original name, sanitised: any directory component is stripped |
| `media_type` | TEXT | Inferred from the extension; only scanner formats accepted |
| `size_bytes` | INTEGER | Capped at 8 MiB per document |
| `sha256` | TEXT | Recorded at upload; verified before every export |
| `uploaded_at`, `uploaded_by` | TEXT | |
| `content` | BLOB | **The bytes.** Listings never load this column |

The bytes live here rather than as a path because the database is the unit of
deployment: a path breaks the moment the file moves to a share. The consequence — a
signed term is personal data inside the database — is covered in
[security-and-compliance.md](security-and-compliance.md).

## `key_incidents` — a key reported lost or stolen (v6)

| Column | Type | Notes |
|---|---|---|
| `id` | TEXT PK | UUID |
| `key_serial` | INTEGER | Which key. Not a foreign key to `keys(id)`: the report is about a serial, and it outlives an inventory row |
| `kind` | TEXT | `lost` \| `stolen`. Two kinds because they are not the same event — one may turn up, the other is evidence of intent — and they revoke the same credentials for different reasons |
| `reported_at` | TEXT | When it happened, typed by the operator: a loss is reported after the fact |
| `reported_by` | TEXT | Who said so — the holder, their manager, a security desk. **Required** |
| `holder_display` | TEXT | The holder as the register knew them, copied so the report still reads after a holder row is edited |
| `circumstances` | TEXT | What happened, in the reporter's terms. Bounded by `domain::MAX_NOTE`, and **never quoted into the audit trail** — the entry counts the characters, because an entry cannot be corrected and this is a field that gets corrected |
| `recorded_at`, `recorded_by` | TEXT | When the register was told, and by which operator |
| `closed_at` | TEXT NULL | Set when every obligation has been met, or deliberately waived |
| `closing_note` | TEXT | On what basis it was closed. The one thing that makes a waiver visible rather than quiet |

One row per report: a key can be lost, recovered and lost again, and flattening that into
columns on `keys` would overwrite the first report with the second. `Store::report_incident`
writes the row **and** moves the key to `lost` in one transaction, asking the lifecycle
first — so the register can never hold a report for a key whose status contradicts it.

Index: `idx_incidents_serial`.

## `key_remediations` — and what was done about it (v6)

| Column | Type | Notes |
|---|---|---|
| `id` | TEXT PK | UUID |
| `key_serial` | INTEGER | Which key |
| `incident_id` | TEXT NULL → `key_incidents(id)` | Nullable: a sanitisation before reissue answers no incident |
| `kind` | TEXT | `certificate-revoked` \| `credential-removed` \| `sanitised` |
| `subject` | TEXT | What was dealt with: a certificate serial, a credential id, or the applets joined by `+` (`fido2+piv+otp`) — the same spelling the reset's own audit entries use |
| `reference` | TEXT | The CA's revocation reference, the relying party's ticket, or how an operator knows an applet was reset. What lets somebody else check the claim |
| `reason` | TEXT | For a revocation, the RFC 5280 reason (`keyCompromise`, …). Empty for the other kinds |
| `recorded_at`, `recorded_by` | TEXT | |
| `detail` | TEXT | Free text; for a credential removal it carries `rp=<relying party>` |

One table for three kinds because they share one shape — *this specific thing has been dealt
with, elsewhere, and here is the reference* — and because "everything that has been done to
this key" is then one query rather than a union of three.

**Two of the three are records of work done in another system**, and that is the design
rather than a gap: a certificate is revoked at the CA that issued it and a credential is
removed at the relying party that holds it. See
[`../features/key-lifecycle-and-revocation.md`](../features/key-lifecycle-and-revocation.md).

`sanitised` rows are what the **reissue gate** reads: `Store::set_key_status` refuses to put
a key back into stock, or to prepare it for a new holder, unless every applet a run wrote to
has a sanitisation recorded *after* that run.

Index: `idx_remediations_serial` on `(key_serial, kind)`.

## `key_rma` — a key sent back to the supplier (v6)

| Column | Type | Notes |
|---|---|---|
| `id` | TEXT PK | UUID |
| `key_serial` | INTEGER | The faulty key; refused unless it is in the inventory |
| `reference` | TEXT | The supplier's case number. **Required** — an RMA nobody can quote is an RMA nobody can chase |
| `sent_at`, `sent_by` | TEXT | |
| `fault` | TEXT | What is wrong with it |
| `replacement_serial` | INTEGER NULL | The key that replaced it. Must already be in the inventory: a case pointing at a serial nobody recorded is the broken reference `delete_key` exists to prevent |
| `replaced_at` | TEXT NULL | |
| `closed_at` | TEXT NULL | Closed with **no** replacement — a refusal, a refund, a write-off |
| `notes` | TEXT | |

The replacement is a *link*, not a copy: the new key keeps its own inventory row, its own
hand-overs and its own runs. An answered case is answered — neither a second replacement nor
a closure may overwrite one.

Index: `idx_rma_serial`.

### What is deliberately *not* a table

**What a key was carrying.** The certificate serial, the credential ids and their relying
parties, the OTP access code and where custody of the secrets went are read back out of
`bootstrap_run_steps.detail` by `domain::lifecycle::dependencies`, which is where every
other piece of run evidence lives (`bootstrap::credential_evidence`,
`bootstrap::certificate_request`). Two reasons, and the second is the stronger one: a stored
list would be a second truth about what a run did, and it would be empty for every register
written before v6 — derived, a register created under v1 answers in full.

## `batches` / `batch_keys` — a box done in one sitting (v8)

| `batches` | Type | Notes |
|---|---|---|
| `id` | TEXT PK | |
| `shape` | TEXT | `stock` or `assigned` — the distinction that decides everything else |
| `template_id`, `template_version` | TEXT | The procedure the **whole box** gets; a resumed batch finishes with the version it started |
| `operator` | TEXT | |
| `started_at`, `finished_at` | TEXT | `finished_at` is set when nothing is left pending |
| `notes` | TEXT | |

| `batch_keys` | Type | Notes |
|---|---|---|
| `batch_id`, `position` | TEXT, INTEGER | Composite PK. Position is stable: it is how a resumed batch lines up with what was written |
| `key_serial` | INTEGER | `NULL` until a key is presented for this position |
| `holder_id`, `holder_display` | TEXT | Assigned enrolment only |
| `run_id` | TEXT | The evidence. Not a foreign key — see the migration note |
| `state` | TEXT | `pending` / `done` / `failed` / `skipped` |
| `detail` | TEXT | Why it failed or was skipped. A transport error or an operator's reason, never a value |

Written **per key, as it goes** rather than once at the end: a batch persisted at the end
is a batch that loses everything when the laptop closes on key 31, which is the case
resumability exists for.

## `open_sessions` — who has the register open right now (v7)

| Column | Type | Notes |
|---|---|---|
| `id` | TEXT PK | The **run**, not the workstation: a pid is reused, and one host may have two windows open |
| `host` | TEXT | The workstation name, so a banner can say *which* computer |
| `operator` | TEXT | The identity the audit trail records, so it can say *who* |
| `app_version` | TEXT | Useful when two builds are in use on one share |
| `opened_at` | TEXT | |
| `last_seen_at` | TEXT | Rewritten every 60s while the session lives |

**Not a lock.** A row is a claim that a session was here at a moment, and nothing more:
one silent for 15 minutes is not shown, and is pruned by the next session that opens the
register. Two operators writing at once is safe — SQLite serialises them, and the busy
timeout waits — so the danger this answers is neither corruption nor a race, but two
people working out of the same box of keys without knowing it. See
[`src/store/presence.rs`](../src/store/presence.rs); the cloud-sync lock is the other
case and still *refuses* the second workstation, because there the two are not sharing a
lock manager at all.

Pruning happens on the **write** path, deliberately: a read-only session must be able to
read who else is here without writing anything.

## `audit` — the trail

| Column | Type | Notes |
|---|---|---|
| `seq` | INTEGER PK | Monotonic from 1; a gap means deletion |
| `at` | TEXT | |
| `actor` | TEXT | The operator |
| `event` | TEXT | Dotted name: `key.distributed` |
| `target` | TEXT | `serial:20423633`, an e-mail, `database` |
| `details` | TEXT | `key=value` facts; **never a secret** |
| `prev_hash` | TEXT | Previous entry's hash; 64 zeros for the first |
| `hash` | TEXT | `SHA256(seq\|at\|actor\|event\|target\|details\|prev_hash)` |

```sql
CREATE TRIGGER audit_no_update BEFORE UPDATE ON audit
BEGIN SELECT RAISE(ABORT, 'audit trail is append-only'); END;
CREATE TRIGGER audit_no_delete BEFORE DELETE ON audit
BEGIN SELECT RAISE(ABORT, 'audit trail is append-only'); END;
```

Immutability is a **database restriction**, which is what NRM §5.3.1 requires — not an
application convention. The field order in the hash is part of the format: changing it
invalidates every existing chain.

---

## Migrations

`Store::migrate` reads `PRAGMA user_version` and applies forward steps. Rules:

1. A file written by a **newer** schema is refused (`StoreError::SchemaTooNew`) — never
   opened and half-understood. On a shared file that refusal is what protects the dataset
   when one workstation upgrades first.
2. Every schema change bumps `SCHEMA_VERSION`, ships its migration, and is called out in
   `CHANGELOG.md` (MINOR bump while 0.x).
3. Migrations are additive where possible. A destructive migration takes a `VACUUM INTO`
   backup first.

Shipped so far:

| Version | Adds |
|---|---|
| v1 | `keys`, `holders`, `bootstrap_runs`, `distributions`, `templates`, `audit` |
| v2 | `keys.serial_source` — how a serial was learned |
| v3 | optional holder fields, `term_templates`, `documents` |
| v4 | `templates.retired_at` — a procedure can be withdrawn without being deleted |
| v5 | `bootstrap_run_steps` — per-step rows; drops `bootstrap_runs.steps` |
| v6 | `key_incidents`, `key_remediations`, `key_rma` — what happens to a key after the hand-over |
| v7 | `open_sessions` — who has the register open right now |
| v8 | `batches`, `batch_keys` — a box of keys bootstrapped in one sitting |

A test builds a v1 database by hand — including a run whose steps are a JSON blob
in serde's old spelling — and opens it with the current build, asserting the chain
carries it to the current version with every step intact and in order
(`a_v1_database_migrates_forward_keeping_its_rows`). The same test asserts what v6 needs no
backfill for: the dependency list is derived, so that v1 run's FIDO2 PIN is what the
sanitisation gate reports as outstanding without a single row having been migrated.

v5's backfill runs in **Rust, not SQL**: mapping `Fido2Pin` to `fido2-pin` in SQL
would be a twelve-branch `CASE` hand-kept in step with `StepKind::slug`, and the
first divergence would be silent. A blob that cannot be parsed leaves that run
with no step rows and an `error` log line, rather than refusing to open the
register — covered by `a_run_with_an_unreadable_step_blob_keeps_its_record`.

v6 adds three tables and alters nothing, which is why it is a single `execute_batch` with
no backfill: the facts it holds are ones nobody was recording before.

v7 adds one table and needs no backfill for a stronger reason: every row in it is about
*now*. A register migrated from v6 starts with an empty `open_sessions`, which is the
correct answer — nobody had it open a moment ago, and the first session to open it says so
itself.

v8 adds two tables and alters nothing. `batch_keys.run_id` is deliberately **not** a
foreign key: a run row is written by the executor's own recorder, and a batch that could
not be saved must never become a reason for a run not to be.

## Personal data summary

| Table | Fields | Category |
|---|---|---|
| `holders` | `full_name`, `email`, `unit`, `registration` | Ordinary personal data |
| `holders` | `identification_number`, `phone`, `address` | **Ordinary personal data, higher sensitivity** — an identification number is a step up from a work e-mail. Optional, so a unit that does not need it never collects it |
| `distributions` | `holder_display` (name + e-mail), `distributed_by`, `returned_to` | Ordinary personal data |
| `bootstrap_runs` | `operator`, `holder_id` | Ordinary personal data (identifier) |
| `documents` | `content` — a signed term carries a name, an identification number and a **signature** | Personal data in document form |
| `key_incidents` | `reported_by`, `holder_display`, `circumstances` — who reported a loss, whose key it was, and what happened | Ordinary personal data. The circumstances are the operator's free text about an event involving a person, so they are bounded, never audited verbatim, and never a place for anything else |
| `key_remediations` | `recorded_by`; a credential id and a certificate serial identify a *key*, not a person, but they are linked to one through the run | Ordinary personal data (identifier) |
| `key_rma` | `sent_by` | Ordinary personal data |
| *(not a table)* the incident note | The holder's name, e-mail and unit, and what was on their key, rendered as text or PDF for the ESI | Personal data in document form — and **not stored**: it is produced on demand from the rows above, because the register already holds the facts and nothing signs a note. A saved copy is a file the operator chose the location of |
| `audit` | `actor`, `target`, `details` may name a person | Ordinary personal data |
| *(not a table)* `<database>.lock` | `operator`, `host`, `pid` — who currently has a cloud-hosted database open ([`../features/cloud-sync-hosting.md`](../features/cloud-sync-hosting.md)) | Ordinary personal data, deleted when the database is closed |

No credential value is stored in any table. Keeping that true is the point of
[`../features/secrets-custody.md`](../features/secrets-custody.md).
