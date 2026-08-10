# Architecture

## Shape

```
                        ┌──────────────────────────────┐
                        │  src/main.rs                 │
                        │  logging::init, eframe::run  │
                        └──────────────┬───────────────┘
                                       │
                        ┌──────────────▼───────────────┐
                        │  app.rs — YkDistApp          │
                        │  state, cached views,        │
                        │  every mutation + its audit  │
                        └───┬───────┬───────┬──────────┘
                            │       │       │
              ┌─────────────▼─┐ ┌───▼────┐ ┌▼──────────────┐
              │ ui/*.rs       │ │ device │ │ template      │
              │ six screens   │ │ trait  │ │ render + plan │
              │ + unlock      │ │        │ │               │
              └───────────────┘ └───┬────┘ └───────┬───────┘
                                    │              │
                     ┌──────────────┼──────────┐   │
                     │              │          │   │
                ┌────▼────┐  ┌──────▼───┐ ┌────▼───▼──┐
                │ native  │  │ ykman    │ │ domain    │
                │ (PC/SC, │  │ (argv +  │ │ records   │
                │  HID)   │  │ parsers) │ │           │
                └─────────┘  └──────────┘ └─────┬─────┘
                                                │
                                    ┌───────────▼──────────┐
                                    │ store — one SQLite   │
                                    │ file + audit table   │
                                    └───────────┬──────────┘
                                                │
                                    ┌───────────▼──────────┐
                                    │ audit — chain, verify │
                                    └───────────────────────┘
```

## Module boundaries, and the reason for each

| Module | Owns | Must not |
|---|---|---|
| `domain` | Records and their rules: validation, lifecycle transitions, run tallies | Do I/O, know about SQL, egui or `ykman` |
| `device` | Talking to hardware behind `YubiKeyBackend` | Know about the database or the GUI |
| `template` | Templates, variable rendering, and turning a template into a plan | Execute anything, hold a secret value |
| `store` | The SQLite file: schema, migrations, pragmas, CRUD, audit insertion | Contain business rules that belong in `domain` |
| `audit` | The chain: entry shape, hashing, verification, the file sink | Depend on `store` (so `store` can use it, not the reverse) |
| `logging` | The one logging entry point | Be bypassed by a hand-formatted line |
| `app` | State, cached views, and every mutation together with its audit entry | Paint |
| `ui` | Painting, and only painting | Do I/O inside a paint closure |

Everything except `app` and `ui` is headless and testable with no display and no key
attached. That is what makes 76 tests possible without hardware.

## Data flow of one hand-over

1. `ui::inventory` → click → `app.detect_keys()`.
2. `device::YubiKeyBackend::info()` → `DeviceInfo` (native, or `ykman` + parser).
3. `domain::YubiKeyRecord::from_device` / `refresh_from_device`.
4. `store.upsert_key()` — parameterised, upsert on the serial.
5. `app.record("key.added", …)` → `store.append_audit()` → chained entry.
6. `app.refresh()` → cached vectors reloaded.
7. Next paint reads the vectors. No SQLite in the paint pass.

## Rules that shape the code

**No I/O in a paint pass.** egui repaints continuously. Reads come from cached vectors;
writes happen in `app` methods triggered by a click, followed by `refresh()`.

**Deferred mutation in tables.** A row's button records an intent (`status_change`,
`to_return`) into a local variable; the mutation runs after the grid closure. This avoids
borrowing `app` mutably while painting it, and keeps writes out of the paint closure.

**`Store` is `Option`.** A failed open cannot be confused with an empty database, and the
unlock screen is the natural consequence rather than a special case.

**Every mutation carries its audit entry.** They live next to each other in `app` methods,
so a new mutation that forgets its audit entry is visible in review. An audit failure sets
a visible `AUDIT FAILURE:` status and logs at `error`.

**Secrets are placeholders, not values.** `template::Arg::Secret` carries a label. There is
no field anywhere in `app`, `ui`, `domain` or `store` that holds a PIN, so no code path can
render or persist one. The custody design
([`../features/secrets-custody.md`](../features/secrets-custody.md)) has to preserve that
when it introduces real secret input.

**Two transports, chosen per step.** `plan::native_op` is the single table of native
operations and their availability; the plan shows which transport each step will use. A
step's transport is not a global mode.

## Dependencies, and why

| Crate | Why this one |
|---|---|
| `eframe`/`egui` 0.36 | Pure-Rust immediate-mode GUI; no web stack, no system webview |
| `rusqlite` (`bundled`) | One file, SQLite compiled in, nothing to install |
| `yubikey` | PIV over PC/SC, pure Rust, maintained by RustCrypto |
| `ctap-hid-fido2` | CTAP2 over HID — the only way to create a FIDO2 credential |
| `hidapi` | The OTP configuration protocol has no crate; this is the transport for writing it ourselves |
| `sha2`, `hex` | Audit chain |
| `serde`/`serde_json` | Templates and run steps as JSON in the database |
| `chrono` | Timestamps, stored as RFC 3339 strings |
| `uuid` | Record ids |
| `thiserror` | Typed errors with messages an operator can read |
| `tracing`/`tracing-subscriber` | The single logging entry point with a custom formatter |

Deliberately absent: a web framework, an ORM, an async runtime. The tool is a
single-user desktop app over one file; none of them would pay for themselves.

## Where the boundaries will be tested next

- **The executor** (Wave 1) needs write traits per applet, so it can be tested against
  mocks. It belongs in a new `bootstrap` module, not in `app` — `app` should call it, not
  contain it.
- **Per-step run rows** (schema v2) will move `BootstrapRun.steps` out of a JSON blob, so
  reports can query step outcomes.
- **Operator authentication** turns `app.operator` from a string into an identity, and the
  enforcement points belong in `store`, not `ui`, so a UI bug cannot bypass a role check.
