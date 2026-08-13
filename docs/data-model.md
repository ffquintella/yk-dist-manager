# Data model

Schema **v3**, tracked in `PRAGMA user_version`. One SQLite file holds everything.
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
`Store::set_key_status`. `in_stock → distributed` is deliberately illegal.

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

A test builds a v1 database by hand — including a run whose steps are a JSON blob
in serde's old spelling — and opens it with the current build, asserting the chain
carries it to v5 with every step intact and in order
(`a_v1_database_migrates_forward_keeping_its_rows`).

v5's backfill runs in **Rust, not SQL**: mapping `Fido2Pin` to `fido2-pin` in SQL
would be a twelve-branch `CASE` hand-kept in step with `StepKind::slug`, and the
first divergence would be silent. A blob that cannot be parsed leaves that run
with no step rows and an `error` log line, rather than refusing to open the
register — covered by `a_run_with_an_unreadable_step_blob_keeps_its_record`.

Planned: **v6** — a `batches` table for bulk enrolment.

## Personal data summary

| Table | Fields | Category |
|---|---|---|
| `holders` | `full_name`, `email`, `unit`, `registration` | Ordinary personal data |
| `holders` | `identification_number`, `phone`, `address` | **Ordinary personal data, higher sensitivity** — an identification number is a step up from a work e-mail. Optional, so a unit that does not need it never collects it |
| `distributions` | `holder_display` (name + e-mail), `distributed_by`, `returned_to` | Ordinary personal data |
| `bootstrap_runs` | `operator`, `holder_id` | Ordinary personal data (identifier) |
| `documents` | `content` — a signed term carries a name, an identification number and a **signature** | Personal data in document form |
| `audit` | `actor`, `target`, `details` may name a person | Ordinary personal data |
| *(not a table)* `<database>.lock` | `operator`, `host`, `pid` — who currently has a cloud-hosted database open ([`../features/cloud-sync-hosting.md`](../features/cloud-sync-hosting.md)) | Ordinary personal data, deleted when the database is closed |

No credential value is stored in any table. Keeping that true is the point of
[`../features/secrets-custody.md`](../features/secrets-custody.md).
