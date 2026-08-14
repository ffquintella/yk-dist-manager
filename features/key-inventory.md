# Feature: Key inventory

## Summary

Every YubiKey the unit owns, identified from the hardware, with a guarded
lifecycle: `InStock → Bootstrapped → Distributed → Returned → Retired`, plus
`Lost`.

## Motivation

"How many keys do we have, where are they, and which ones are unaccounted for?" is
the question an asset audit asks. It needs a row per physical key, keyed on the
serial the hardware reports, with a state that cannot be quietly wrong.

The lifecycle is not decoration. It encodes the rule that a key must be
bootstrapped before it is handed over: a key that goes out with a factory-default
PIV PIN (`123456`) and PUK (`12345678`) is worse than no key, because it looks like
security.

## Current state

**Done for the basics.** `src/domain/key.rs`, `src/store/mod.rs`,
`src/ui/inventory.rs`:

- Fields: `serial` (unique), `model`, `firmware`, `form_factor`, `fips`,
  `applications`, `status`, `batch`, `notes`, `created_at`, `updated_at`.
- `from_device` builds a record from a `DeviceInfo`; `refresh_from_device` updates
  only the hardware-derived fields, leaving lifecycle and notes alone.
- `firmware_triple()` and `supports_fido_min_pin_length()` drive the template's
  firmware gates.
- `can_transition_to` defines the legal moves; `Store::set_key_status` refuses
  anything else with `StoreError::Transition` and the UI shows the refusal.
- Inventory screen: table plus "Read attached key", with per-row actions.
- **The observation** (`notes`) is editable: at intake, in the "Add by serial /
  scan…" panel, and afterwards through the row's `observation…` action.
  `Store::set_key_notes` is the write; a device re-read never touches it.
- **Removal** of an inventory row, confirmed, for a mistake at intake only:
  `Store::delete_key` refuses a serial with a hand-over or a bootstrap run against
  it (`StoreError::HasHistory`) and points at retirement instead.

Not yet done: batch/procurement fields in the UI, search and filtering, engraving
or asset-tag reconciliation, and the "already bootstrapped" detection.

## Design

### Lifecycle

```
InStock ──► Bootstrapped ──► Distributed ──► Returned ──► InStock / Bootstrapped
   │             │                │             │
   │             │                └──► Lost ────┘
   └─────────────┴──────────────────────────► Retired (terminal)
```

Legal transitions (`domain::key::KeyStatus::can_transition_to`):

| From | To |
|---|---|
| `InStock` | `Bootstrapped`, `Retired`, `Lost` |
| `Bootstrapped` | `Distributed`, `InStock`, `Retired`, `Lost` |
| `Distributed` | `Returned`, `Lost`, `Retired` |
| `Returned` | `InStock`, `Bootstrapped`, `Retired` |
| `Lost` | `Returned`, `Retired` |
| `Retired` | — (terminal) |

Notably absent: `InStock → Distributed`. A behaviour test asserts it is refused, and
the Distribution screen asks the lifecycle *before* it writes the hand-over, so the
refusal arrives with nothing recorded.

**What performs `InStock → Bootstrapped`** is the run itself: a bootstrap run that
settles `Completed` moves the key (`YkDistApp::settle_key_status`), audited
`key.status_changed` with the run id. Only `Completed` counts — the engine marks a run
with a required step unmet as `Failed` and audits `bootstrap.incomplete`, "the key is
not ready to hand over" — and a run never overrules a key that is lost, retired or
already distributed. *Mark bootstrapped* on the Inventory screen stays for the key
configured outside the tool; it is no longer the only thing that moves a key, which it
was until 0.13, and forgetting it surfaced much later as a refused hand-over.

`Bootstrapped → InStock` exists for the case where a bootstrap is undone by an
applet reset.

### Identity

The serial is the natural key and carries a `UNIQUE` constraint, so reading the
same key twice updates one row. The UUID `id` exists for foreign keys, so a
distribution record survives even if a key row is later archived.

### The observation, and what removal is for

Two operator-facing fields sit outside the lifecycle, and they are easy to confuse
with it.

**The observation** (`notes`) is the one field on a key that no device can supply:
the shipment it arrived in, a bent connector, why a key is being held back. It is
bounded by `domain::MAX_NOTE`, kept when the key is re-read
(`refresh_from_device` leaves it alone), and **never a place for a secret** — the
rule in AGENTS.md §2 applies to a field an operator types as much as to a log line,
and the UI says so where the field is.

Its audit entries record *shape, not content*: `key.note_changed` carries
`note=set|cleared|changed|unchanged chars=<n> was_chars=<n>`, and `key.removed`
carries `note_chars=<n>`. The reason is that an audit entry cannot be corrected or
deleted, by trigger, while operator free text is precisely the field that sometimes
must be — quoting it into the chain would make a mistyped observation permanent and
put uncontrolled text into the immutable record.

**Removal** is the correction of an *intake mistake*: a mis-typed serial, a label
scanned twice, a shipment recorded against the wrong unit. It is not a lifecycle
exit — `Retired` is, and retirement keeps the record
(`features/key-lifecycle-and-revocation.md`). So `Store::delete_key` refuses any
serial that a hand-over or a bootstrap run refers to: a distribution record
pointing at a serial nobody can look up is not a register. The GUI asks for
confirmation before a removal that *is* allowed, and shows the refusal *before* the
click for one that is not, with the counts and the alternative named.

The row goes; the trail does not. `key.added` and `key.removed` both stay in the
audit chain, so "this serial was registered and then removed, by whom and when"
survives the deletion.

`fips` is inferred from the model string (`"FIPS"` in the marketing name) because
that is what the management applet reports through `ykman info`. When the native
management applet lands (`features/native-device-transport.md` Phase 5), read the
real FIPS flag instead.

## Phases

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | Record, upsert on serial, hardware-derived fields | 0 | Done | |
| 1b | Serial provenance (`SerialSource`), never downgraded | 0 | Done | schema v2; see `features/serial-scanning.md` |
| 2 | Guarded lifecycle transitions | 0 | Done | refusals surfaced, not swallowed |
| 3 | Firmware capability gates | 0 | Done | 5.7 floor for min-PIN-length |
| 4 | Batch / invoice / procurement fields in the UI | 2 | Todo | column exists in the schema |
| 5 | Search and filter (serial, holder, status, firmware) | 0 | **Done** | shipped as `features/gui-shell.md` phase 3: [`browse::keys`](../src/browse.rs) matches the serial, model, firmware, form factor, batch and observation, with a status filter and paging. **Not** by holder — the Inventory table has no holder column, and "which key does Ana have" is answered on Distribution, where it does |
| 6 | "Already configured" detection before re-bootstrap | 1 | **Done elsewhere** | shipped as [`device-detection.md`](device-detection.md) phase 5: a `Blocking` pre-flight finding with **no override**, per the decision of 2026-08-13, resting on `PivState::configured_slots` so the factory attestation slot `f9` is not mistaken for a configuration. The refusal names the factory reset as the way forward |
| 6b | "Unverified keys" view: scanned or typed, never read | 2 | Todo | the natural companion to provenance |
| 7 | Reconciliation report: expected vs present vs unaccounted | 2 | Todo | `features/reports-and-export.md` |
| 8 | Bulk import of an existing spreadsheet | 0 | **Done** | shipped as `features/storage-sqlite-single-file.md` phase 8: [`store::import`](../src/store/import.rs) — column mapping, a preview that writes nothing, then apply |
| 9 | Observation per key, and confirmed removal of an intake mistake | 0 | Done | out of turn, v0.4.0 — see the roadmap note |

## Audit events

| Event | When |
|---|---|
| `key.added` | New serial recorded; `source=… verified=… note_chars=<n>` |
| `key.refreshed` | Known serial re-read |
| `key.status_changed` | Lifecycle move, with `to=<status>`; a move made by a completed run also carries `from=<status> run=<id>` |
| `key.note_changed` | Observation stored; `note=set\|cleared\|changed\|unchanged chars=<n> was_chars=<n>` — shape, never content |
| `key.removed` | Inventory row deleted; `status=… source=… model=… note_chars=<n>`. The entry outlives the row |

A refused transition is logged and shown — a hand-over refused by the lifecycle is
logged `distribution.refused.lifecycle` with the serial and the state it is in —
and (Phase 2 of the audit feature) should also be *audited*: an attempt to hand out
an unbootstrapped key is information worth keeping.

## Tests

`tests/unit_domain.rs`:

- `key_starts_in_stock_and_derives_fields_from_the_device`
- `fips_series_is_flagged_from_the_model_name`
- `refresh_keeps_lifecycle_and_notes`
- `lifecycle_transitions_are_restricted`
- `an_observation_is_summarised_for_one_line`
- `an_observation_change_is_audited_by_shape_not_by_content`
- `removal_detail_names_what_the_register_is_losing`
- `stored_and_audited_names_are_one_spelling`

`tests/unit_store.rs`:

- `an_observation_is_stored_against_the_serial`
- `an_observation_on_an_unknown_serial_is_not_found`
- `removing_a_key_deletes_the_row_and_returns_what_was_removed`
- `removing_an_unknown_serial_is_not_found`
- `a_key_with_a_bootstrap_run_cannot_be_removed`
- `history_counts_report_what_refers_to_a_serial`

`tests/behaviour_distribution.rs`:

- `scenario_a_key_cannot_be_distributed_straight_from_stock`
- `scenario_reading_the_same_key_twice_does_not_duplicate_inventory`
- `scenario_a_key_that_has_been_handed_over_cannot_be_removed_from_the_inventory`
- `scenario_a_serial_typed_by_mistake_is_removed_with_its_observation`
- `scenario_an_observation_survives_reading_the_key_again`

`tests/behaviour_app_key_ready_to_hand_over.rs`:

- `scenario_a_key_is_handed_over_once_a_run_has_made_it_ready` — a run that settles
  `Completed` moves the key and audits the move once (a second run for a key already
  there writes nothing); a run that ends `Failed` leaves the key where it was.

## References

- `src/domain/key.rs`, `src/ui/inventory.rs`
- `docs/data-model.md`, `features/key-lifecycle-and-revocation.md`
