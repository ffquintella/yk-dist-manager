# GUI

egui/eframe 0.36. Six screens plus the database chooser. The audience is an operator at a
desk with a key in hand, often mid-conversation with the person receiving it.

## egui 0.36 notes

The API differs from older tutorials in two ways that matter:

- `eframe::App` requires **`fn ui(&mut self, ui: &mut egui::Ui, frame: &mut Frame)`** — the
  app is handed a root `Ui`, not a `Context`.
- Panels compose **inside** that `Ui`: `egui::Panel::top(id).show(ui, …)`,
  `egui::Panel::bottom(id)`, `egui::CentralPanel::default().show(ui, …)`. There is no
  `TopBottomPanel`.

## Layout

```
┌──────────────────────────────────────────────────────────────┐
│ YubiKey Distribution Manager | Inventory Holders Distribution │  Panel::top
│                                Bootstrap Audit Settings       │
├──────────────────────────────────────────────────────────────┤
│                                                              │
│   CentralPanel, inside ScrollArea::both                      │
│                                                              │
├──────────────────────────────────────────────────────────────┤
│ operator: felipe | db: network share | <last outcome>         │  Panel::bottom
└──────────────────────────────────────────────────────────────┘
```

## Screens

### Database chooser

Shown whenever no database is open — first run, a locked file, an unreachable share, or
the operator closing one to switch. It carries:

- the **recent databases**, each marked *available* or *not reachable* (an unmounted
  share stays listed — that is a network problem, not a decision), with *open*,
  *use path* and *forget*;
- a typed path (which is also how a UNC path or a pasted share path gets in) and a
  password field, cleared immediately after use;
- **Open** and **Create** as separate buttons, plus native *Choose file…* /
  *New file…* dialogs;
- a note when the build lacks `file-dialog` or `encrypted-db`, so a refused password
  is explained rather than mysterious.

Open and Create never guess: opening a path that does not exist is an error, and
creating over an existing file is refused.

### Inventory

The keys the unit owns. "Read attached key" identifies the key in the reader and inserts or
refreshes its row. Columns: serial, model, firmware, form factor, status, applications,
actions. Per-row actions advance the lifecycle, and a refused transition is shown verbatim in
the status bar.

**Add by serial / scan…** opens the intake panel: a text field (which is what a USB
barcode scanner types into — Enter submits), camera controls with a preview when the
build has the `camera` feature, and confirm/discard for a decoded serial. A key
recorded this way is marked as not verified until it is read from the hardware.

### Holders

Registration form — name, corporate e-mail, unit, and the optional registration,
identification number, phone and address — plus the list with a count of keys currently
held. The screen says that optional fields appear on the consignment term when filled in
and omit their line when not. Validation errors appear under the form, in red,
selectable.

### Distribution

The hand-over form (key, holder, delivery method, receipt reference, notes, and whether to
attach the latest bootstrap run) above the history table. The table shows what was applied —
the run summary — and offers "record return" on open records only.

Each row also carries the **term** column: *term* opens the consignment-term panel,
*upload* files a signed scan, and a badge says `none filed` (amber) or `n filed`
(green) so an unsigned hand-over is visible at a glance.

The **term panel** picks a language (falling back with a visible notice when the
requested one has no template), renders the term from the record, and offers *Save as
text…* and *Upload signed term…*. Below it, the filed documents are listed with their
size, short SHA-256 and an *export* action — which verifies the digest and refuses on a
mismatch.

### Bootstrap

Selection (key serial, holder, template), per-step checkboxes with required steps marked,
then the plan table:

| Step | Transport | Operation | Note |

Transport is colour-*and*-text (`native` green, `ykman` amber, `manual` purple) — colour is
never the only carrier of meaning. The operation column is monospace and selectable so it can
be pasted into a ticket. Secrets render as `<FIDO2-PIN>`.

"Execute on key" is present and **disabled**: the screen states its own limitation rather
than implying capability it does not have.

### Audit

The trail, newest first (500 entries), with "Verify chain". A broken chain reports as
`AUDIT CHAIN BROKEN: …` in the status bar — loudly, because it means something is wrong that
no other screen will tell you about.

### Settings

Operator and organisation (persisted between sessions); the database path, locking mode and
whether it is password-protected; the device transport; the recent databases; and the
actions — *Switch database…*, *Open another…*, *Create new…*, integrity check, backup,
reload. Also the version string, so a screenshot identifies the build.

## Rules

1. **No I/O in a paint closure.** Screens read `app`'s cached vectors; mutations happen in
   `app` methods called from a click and end with `refresh()`. Native file dialogs and
   camera frames follow the same rule: a click records a `DbRequest` (or sets a flag) and
   the work happens at the top of the next `ui` call, because a modal dialog inside a paint
   closure blocks rendering.
2. **Deferred mutation in tables.** A row's button records an intent into a local variable;
   the mutation runs after the grid closure. This keeps writes out of the paint pass and
   avoids borrowing `app` mutably while painting it.
3. **Errors are visible and selectable**, and also logged. Never a silent no-op, never an
   `unwrap()` on a store call.
4. **Refusals explain themselves** — the message names the rule that refused.
5. **Nothing writes to hardware without an explicit click and (Wave 1) a confirmation.**
   Navigation never mutates.
6. **Secrets cannot render**, because no widget receives one.
7. **Length-bound inputs**: `char_limit` matches the domain bound.
8. The central panel scrolls; the window never has to be resized to reach a button.

## Planned

Search and filtering (the tables will not scale past a few dozen rows), sortable columns, a
run view with live per-step status and touch prompts, secret prompt panels, a pre-flight
confirmation, window-state persistence, a log panel, pt-BR localisation, and an accessibility
pass (contrast, font scaling).

See [`../features/gui-shell.md`](../features/gui-shell.md) and
[`../features/gui-bootstrap-wizard.md`](../features/gui-bootstrap-wizard.md).

## Testing boundary

Paint code is not unit tested. Everything behind a button lives in a `YkDistApp` method
(`detect_keys`, `submit_holder`, `submit_distribution`, `return_key`, `build_plan`,
`record_dry_run`, `verify_audit`) and is covered by the behaviour suites through the same
store calls. If something in the UI is hard to test, that is the signal to move it down —
not to skip the test.
