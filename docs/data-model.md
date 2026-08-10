# Data model

Schema **v1**, tracked in `PRAGMA user_version`. One SQLite file holds everything.
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
| `applications` | TEXT (JSON) | USB-enabled applications: `["FIDO2","PIV",…]` |
| `status` | TEXT | `in_stock` \| `bootstrapped` \| `distributed` \| `returned` \| `lost` \| `retired` |
| `batch` | TEXT | Purchase/invoice reference |
| `notes` | TEXT | Operator notes; never overwritten by a device re-read |
| `created_at`, `updated_at` | TEXT | |

Transitions are enforced in `domain::KeyStatus::can_transition_to` and refused by
`Store::set_key_status`. `in_stock → distributed` is deliberately illegal.

## `holders` — the people

**This is the personal-data table.** Any change here updates
[security-and-compliance.md](security-and-compliance.md) and the FGV data documentation.

| Column | Type | Notes |
|---|---|---|
| `id` | TEXT PK | UUID |
| `full_name` | TEXT | Certificate `CN`; required |
| `email` | TEXT **UNIQUE** | Certificate `rfc822Name` SAN; lowercased on entry; required |
| `unit` | TEXT | Certificate `OU`; required |
| `registration` | TEXT | Optional |
| `active` | INTEGER | Default 1; enforcement is a later phase |
| `created_at` | TEXT | |

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
| `steps` | TEXT (JSON) | `Vec<StepOutcome>`; schema v2 moves these to rows |
| `custody` | TEXT | The custody model: `transport-pin+forced-change` (the decided default), `holder-set`, `escrowed:<external reference>`, or `no-secret-set`. Never a secret value |

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
| `id` | TEXT | e.g. `fgv-standard` |
| `version` | TEXT | e.g. `1` |
| `name` | TEXT | Display name |
| `body` | TEXT (JSON) | The full `BootstrapTemplate` |
| `updated_at` | TEXT | |
| | | **PRIMARY KEY (`id`, `version`)** |

An edited procedure is a new *version*, never a mutation of one that has already run.
Built-ins are seeded idempotently and never overwrite an edited template of the same id and
version.

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

Planned: **v2** — per-step rows for `bootstrap_runs` (queryable step outcomes);
**v3** — a `batches` table for bulk enrolment.

## Personal data summary

| Table | Fields | Category |
|---|---|---|
| `holders` | `full_name`, `email`, `unit`, `registration` | Ordinary personal data |
| `distributions` | `holder_display` (name + e-mail), `distributed_by`, `returned_to` | Ordinary personal data |
| `bootstrap_runs` | `operator`, `holder_id` | Ordinary personal data (identifier) |
| `audit` | `actor`, `target`, `details` may name a person | Ordinary personal data |

No credential value is stored in any table. Keeping that true is the point of
[`../features/secrets-custody.md`](../features/secrets-custody.md).
