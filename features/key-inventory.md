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

Notably absent: `InStock → Distributed`. A behaviour test asserts it is refused.
`Bootstrapped → InStock` exists for the case where a bootstrap is undone by an
applet reset.

### Identity

The serial is the natural key and carries a `UNIQUE` constraint, so reading the
same key twice updates one row. The UUID `id` exists for foreign keys, so a
distribution record survives even if a key row is later archived.

`fips` is inferred from the model string (`"FIPS"` in the marketing name) because
that is what the management applet reports through `ykman info`. When the native
management applet lands (`features/native-device-transport.md` Phase 5), read the
real FIPS flag instead.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Record, upsert on serial, hardware-derived fields | Done | |
| 1b | Serial provenance (`SerialSource`), never downgraded | Done | schema v2; see `features/serial-scanning.md` |
| 2 | Guarded lifecycle transitions | Done | refusals surfaced, not swallowed |
| 3 | Firmware capability gates | Done | 5.7 floor for min-PIN-length |
| 4 | Batch / invoice / procurement fields in the UI | Todo | column exists in the schema |
| 5 | Search and filter (serial, holder, status, firmware) | Todo | the table will not scale past ~50 rows |
| 6 | "Already configured" detection before re-bootstrap | Todo | occupied 9c slot, FIDO PIN already set |
| 6b | "Unverified keys" view: scanned or typed, never read | Todo | the natural companion to provenance |
| 7 | Reconciliation report: expected vs present vs unaccounted | Todo | `features/reports-and-export.md` |
| 8 | Bulk import of an existing spreadsheet | Todo | CSV with a dry-run preview |

## Audit events

| Event | When |
|---|---|
| `key.added` | New serial recorded |
| `key.refreshed` | Known serial re-read |
| `key.status_changed` | Lifecycle move, with `to=<status>` |

A refused transition is logged and shown, and (Phase 2 of the audit feature)
should also be audited: an attempt to hand out an unbootstrapped key is
information worth keeping.

## Tests

`tests/unit_domain.rs`:

- `key_starts_in_stock_and_derives_fields_from_the_device`
- `fips_series_is_flagged_from_the_model_name`
- `refresh_keeps_lifecycle_and_notes`
- `lifecycle_transitions_are_restricted`

`tests/behaviour_distribution.rs`:

- `scenario_a_key_cannot_be_distributed_straight_from_stock`
- `scenario_reading_the_same_key_twice_does_not_duplicate_inventory`

## References

- `src/domain/key.rs`, `src/ui/inventory.rs`
- `docs/data-model.md`, `features/key-lifecycle-and-revocation.md`
