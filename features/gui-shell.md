# Feature: GUI shell

## Summary

The egui/eframe application: an unlock screen, six tabs, a status bar, and the rule
that no screen queries the database inside a paint pass.

## Motivation

The tool is used at a desk, with a key in hand, by someone who is also talking to the
person receiving it. That sets the design constraints: few screens, no modal mazes,
every refusal visible in place, and nothing that requires reading a manual mid
hand-over.

egui is immediate-mode, which is a good fit for that but has one sharp edge: the paint
function runs continuously, so anything expensive or side-effecting inside it runs
continuously too.

## Current state

**Done for the shell.** `src/app.rs` + `src/ui/`:

- `YkDistApp` implements `eframe::App::ui` (egui 0.36 hands the app a root `Ui`
  rather than a `Context`; `Panel::top`/`Panel::bottom`/`CentralPanel` compose inside
  it).
- Two states: locked (unlock screen) and open. `Store` is `Option`, so a failed open
  cannot be mistaken for an empty database.
- Screens: Inventory, Holders, Distribution, Bootstrap, Audit, Settings, each a module
  exposing `show(&mut YkDistApp, &mut Ui)` and holding no state of its own.
- Cached views (`keys`, `holders`, `distributions`, `runs`, `templates`,
  `audit_view`) refreshed by `refresh()` after every mutation — the paint pass reads
  vectors, never SQLite.
- Status bar shows the operator, whether the database is local or on a share, and the
  last outcome message — the last of these coloured by `status::classify`, so an
  audit failure is loud rather than being one more grey line.
- `ui::error_label` renders errors in red, selectable, on the screen that caused them.
- All text inputs are length-capped by `ui::capped_input` / `capped_area`, matching
  the domain bounds.
- Deferred-mutation pattern: table rows record an intent (`status_change`,
  `to_return`) and the mutation runs after the table closure, so nothing borrows the
  app mutably while the grid is being painted.
- **Themed with `egui-elegance` 0.15** — `ui::install_theme` runs once per frame from
  `App::ui`, before anything paints. Four palettes (Slate by default, Charcoal,
  Frost, Paper), chosen in Settings and persisted in `settings.json`. The shared
  building blocks live in `src/ui/mod.rs`; see [`docs/gui.md`](../docs/gui.md#theme)
  for the helper table and the two deliberate departures from the crate's defaults
  (selectable refusals, hand-applied input caps).
- **Fluid layout** — one gutter (`ui::GUTTER`) on both sides of the top bar, the
  body and the status bar, and every card, banner, form and table spanning what is
  left. Width is the layout's business, not the content's: `ui::card` /
  `ui::titled_card` claim the row, `ui::form_columns` splits it in two, fields ask
  for their column rather than a constant, and `ui::table` contains its own
  horizontal overflow.

Not yet done: keyboard shortcuts, search, window-state persistence, and
localisation.

## Design

### Rules for every screen

1. **No I/O in a paint closure.** Reads come from the cached vectors; writes happen in
   `YkDistApp` methods called from a click, and are followed by `refresh()`.
2. **Errors are visible and selectable.** They also go to the log. Never a
   `println!`, never a silent no-op, never an `unwrap()` on a store call.
3. **Refusals explain themselves.** "recorded, but status not updated: illegal status
   transition: In stock -> Distributed" is the standard.
4. **Destructive actions are explicit.** A button that writes to a key names the key
   and asks. Nothing writes on navigation.
5. **Secrets never render.** A plan shows `<FIDO2-PIN>`; there is no widget that can
   display a PIN because no PIN reaches the UI state.
6. **Tables scroll, the page does not.** The body is a `ScrollArea::vertical`; a
   table too wide for the window scrolls sideways inside its own card
   (`ui::table`). A page that scrolled horizontally would make every card as wide
   as the widest table on the screen, and nothing would line up.
7. **Width comes from the layout.** No screen invents a field width: a card takes
   the page, a column takes half of it, and a field takes its column. The only
   constants left are caps on things that must *not* grow (a serial field), and
   they are named and justified where they sit.

### Screen inventory

| Screen | Answers |
|---|---|
| Inventory | What keys do we have, in what state, what does the hardware say? |
| Holders | Who can receive a key, and how many do they hold? |
| Distribution | Who has what, since when, from whom, with what applied — and record a return |
| Bootstrap | Plan and (Wave 1) run the procedure for one key and holder |
| Audit | The trail, newest first, with chain verification |
| Settings | Operator, organisation, database location and locking mode, integrity check, backup |

### Unlock screen

Shown whenever `store` is `None`, which covers both "needs a password" and "the file is
unusable". It prints the path, the error, and a password field that is cleared
immediately after submission. It also says that password support needs the
`encrypted-db` build, so an operator on a default build understands why their password
is rejected.

## Phases

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | Shell, eight screens, unlock, status bar | 0 | Done | Inventory, Holders, Distribution, Bootstrap, Templates, Terms, Audit, Settings |
| 2 | Deferred-mutation pattern in tables | 0 | Done | avoids borrow conflicts and mid-paint writes |
| 3 | Search / filter on Inventory, Holders, Distribution | 0 | **Done** | [`crate::browse`](../src/browse.rs) + all three screens; one query box across every displayed field, plus a status filter on Inventory and outstanding-only on Distribution |
| 4 | Sortable columns and pagination | 0 | **Done** | clickable sort headers with a text arrow, ties broken on a stable field so rows cannot shuffle under the cursor, and paging that clamps rather than stranding an empty page |
| 5 | Window size/position and last-tab persistence | 0 | **Done** | `settings::WindowState`, restored in `main.rs` and saved on change (not per frame — the file may be on a share) |
| 6 | Keyboard flow: Enter to submit, Tab order, shortcuts for detect/refresh | 0 | Todo | matters for repeated hand-overs |
| 7 | Confirmation dialogs for hardware writes | 0 | **Done** | one confirmation for the whole run, naming the serial, holder, step count and the steps that cannot be undone. It is the only place a `bootstrap::Confirmation` is constructed |
| 8 | Log panel (last N lines, copyable) | 0 | **Done** | [`crate::logbuf`](../src/logbuf.rs) + a resizable bottom panel with a level filter and *Copy all* |
| 9 | Localisation (pt-BR / en) | — | Todo | the audience is Brazilian; log format is already pt |
| 10 | Accessibility pass: contrast, font scaling, no colour-only meaning | 0 | Todo | the transport column relies on colour plus text — keep both; contrast now has to hold in all four palettes |
| 11 | Theming | 0 | Done | `egui-elegance` 0.15; four palettes, the choice persisted in `settings.json` |
| 12 | Fluid layout | 0 | Done | one gutter, full-width cards, columns that split the page, tables that contain their own overflow (`ui::card`, `titled_card`, `table`, `form_columns`) |

## Audit events

The shell emits `app.opened`; everything else is emitted by the feature the screen
drives.

## Tests

GUI painting is not unit tested. Instead, the logic behind every button lives in
`YkDistApp` methods (`detect_keys`, `submit_holder`, `submit_distribution`,
`return_key`, `build_plan`, `record_dry_run`, `verify_audit`), which are exercised by
the behaviour suites through the same store calls.

That is the deliberate testing boundary — see `features/testing-strategy.md`. If
something is hard to test, it is in the wrong place: move it out of the paint pass.

## Open questions and gates

- Whether to port to `ironroot-gui` once it is published. The `App` implementation is
  small and isolated, so it is a contained change; there is no reason to do it before
  the framework exists.
- Language: the UI is currently English while the log format is Portuguese (the norm
  specifies it). Decide before Phase 9.

## References

- `src/app.rs`, `src/ui/*.rs`
- `docs/gui.md`
