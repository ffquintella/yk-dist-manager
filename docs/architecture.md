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
              │ eight screens │ │ trait  │ │ render + plan │
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
| `template` | Templates, variable rendering, the editable draft, and turning a template into a plan | Execute anything, hold a secret value |
| `versioning` | One answer to "what number does the next edit get?", shared by templates and terms | Know what is being versioned |
| `pdf` | Setting a monospaced document on A4 pages and writing the PDF: encoding, wrapping, pagination, the cross-reference table | Decide what a document *says* — it is handed lines, and it reads no clock |
| `store` | The SQLite file: schema, migrations, pragmas, CRUD, audit insertion | Contain business rules that belong in `domain` |
| `store::cloud` | Making a database in a sync folder (OneDrive, Dropbox, …) strictly sequential: the settle wait, the `<database>.lock` single-writer lock, conflict-copy detection | Pretend to be a distributed lock, or hold anything secret |
| `store::smb` | Reaching an SMB share: parsing a location, the identity to present, connecting and releasing (`WNetAddConnection2W`, `NetFSMountURLSync`), and reporting a local path | Open a database — it hands back a path and a `StoreConfig`, and `Store` never learns what SMB is. Keep a password anywhere but a zeroed-on-drop `Secret`, or put one in an argument vector |
| `audit` | The chain: entry shape, hashing, verification, the file sink | Depend on `store` (so `store` can use it, not the reverse) |
| `logging` | The one logging entry point | Be bypassed by a hand-formatted line |
| `branding` | The embedded application icon, and refusing a malformed one | Depend on an optional feature — the icon exists in every build |
| `app` | State, cached views, and every mutation together with its audit entry | Paint |
| `ui` | Painting, and only painting | Do I/O inside a paint closure |

Everything except `app` and `ui` is headless and testable with no display and no key
attached. That is what makes the whole test suite possible without hardware.

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

**Reading a key is separate from writing to one.** The three applet state reads sit on the
*write* traits, because they need the same connection — but [`device::applets`](../src/device/applets.rs)
is the read-only entry point a screen calls, and it reads each applet independently so a
disabled FIDO2 applet does not blank the PIV answer. A failed read is kept as a **reason**
rather than dropped: "slot 9c is empty" and "PIV was not read" lead to opposite decisions,
and the pre-flight refusal of an already-configured key would be unsound if it could not
tell them apart.

**Destroying what is on a key is its own trait, and it takes no secret.**
[`device::reset`](../src/device/reset.rs) is deliberately not part of `WriteBackend`:
every method there borrows a `Secret`, and a factory reset destroys the credential rather
than presenting it — it is the path for a key whose PIN nobody has. Separating them means
no implementation can be tempted to ask for a PIN in the one operation that must work
without one, and it means no reset detail can carry a secret because none is ever supplied.
The engine borrows the executor's two rules — an unforgeable `Confirmation` re-checked
against the request, and no record before the first write — and adds one of its own: a
single applet's failure does not stop the others, because a half-reset key is worse than a
reset one.

**Management-key authentication belongs to a card session, so the writes that need
it live with it.** [`device::piv_session`](../src/device/piv_session.rs) is one PC/SC
connection that selects the PIV applet, reads the management slot's *actual* cipher,
authenticates with AES, and then issues `GENERATE ASYMMETRIC KEY PAIR` and `PUT DATA`
of a certificate. It exists because the [`yubikey`] crate cannot authenticate to a
firmware-5.7 management slot at all — its `MgmKey` is a 3DES type, and 5.7 removed
3DES — and because authenticating on one connection and then calling the crate on
another authenticates nothing. Everything that needs *no* such authentication
(`change_pin`, `change_puk`, `sign_data`, `attest`, the metadata reads) stays with the
crate: this is one exchange plus its two dependents, not a second PIV implementation.

**A certificate that arrives by hand is checked before the write, by pure code.**
[`device::certificate`](../src/device/certificate.rs) parses PEM or DER, summarises
what the certificate claims, and refuses one whose public key is not the slot's. It is
compiled into every build, including the `ykman`-only one, because reading a
certificate needs no card — and it is where the check lives rather than in the wizard
so that it is covered by tests (`src/ui/` is outside the coverage gate, and §4 of
`AGENTS.md` makes that a contract rather than an amnesty).

**The certificate request is pure code with the signature injected.**
[`device::csr`](../src/device/csr.rs) assembles PKCS#10 — subject, `SubjectPublicKeyInfo`,
the `rfc822Name` SAN wrapped in an `extensionRequest` attribute — and takes a closure that
signs a digest. The card is one closure at one point. Signing needs a key, the key needs a
PIN and the PIN needs a run in progress, so a design where the ASN.1 could only be reached
with a YubiKey attached is a design whose ASN.1 nobody checks.

**Which transport the session reads through** is a separate question, answered once at
startup by [`device::select`](../src/device/select.rs): `probe()` asks the machine, and a
pure `decide()` maps `(requested, availability)` to a transport plus the reason to show
for it. The separation is what makes every branch a unit test on a machine with no
reader. Two rules hold the design together — the *probe* decides rather than the feature
flag, because a flag cannot say whether PC/SC is actually answering; and the probe may
demote but nothing silently promotes, because a wrong native choice fails every read for
the life of the process while a wrong `ykman` choice only costs a subprocess.

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
