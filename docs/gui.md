# GUI

egui/eframe 0.36. Seven screens plus the database chooser. The audience is an operator at a
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
│ ⇤18⇥ YubiKey Distribution Manager | Inventory Holders …  ⇤18⇥ │  Panel::top
│      Bootstrap Terms Audit Settings                          │
├──────────────────────────────────────────────────────────────┤
│      CentralPanel, inside ScrollArea::vertical               │
│      ┌────────────────────────────────────────────────┐      │
│      │ card — always the page width                   │      │
│      └────────────────────────────────────────────────┘      │
├──────────────────────────────────────────────────────────────┤
│ ⇤18⇥ operator: felipe | db: network share | <last outcome>    │  Panel::bottom
└──────────────────────────────────────────────────────────────┘
```

The layout is **fluid**: `ui::GUTTER` (18px) is the inner margin of all three
panels, so the product name, the screen heading and the status pill share one left
margin, and the content is whatever is left of the window — at any size, on any
screen.

Three rules follow, and they are what the helpers in the [theme](#theme) section
exist to enforce:

1. **A card is the page width.** `elegance::Card` is an `egui::Frame`, which sizes
   itself to its contents, so a card built by hand is as wide as whatever happens
   to be inside it. `ui::card` / `ui::titled_card` claim the row instead — without
   them the Inventory table filled the window while the Holders form stopped two
   thirds of the way across.
2. **A field takes its column, not a constant.** `ui::form_columns` splits the card
   in two even columns; `elegance::TextInput` and `TextArea` already default to the
   width they are given, so most fields pass no width at all. `Select` is the
   exception — it falls back to 160px — which is why the closure is handed the
   column width. The remaining constants cap things that must *not* grow, such as
   the serial field on the Bootstrap screen.
3. **A wide table scrolls inside its own card.** `ui::table` wraps its grid in a
   `ScrollArea::horizontal`, so a 64-character digest does not widen the page. The
   body itself scrolls vertically only. The alternative — a horizontally scrolling
   page — makes every card on a screen as wide as the widest table on it, which is
   the inconsistency this replaced.

The window has a 900px minimum (`main.rs`), which is what makes the unconditional
two-column split safe: half of 900 is still a readable field.

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
text…*, *Upload signed term…* and *Edit wording…* — which opens the Terms screen on the
same language. Under the language row it names the template that produced the text
(`consignment@2 (pt-BR)`), so an edit is visibly in effect. Below it, the filed documents
are listed with their size, short SHA-256 and an *export* action — which verifies the
digest and refuses on a mismatch.

### Bootstrap

Selection (key serial, holder, template), per-step checkboxes with required steps marked,
then the plan table:

| Step | Transport | Operation | Note |

Transport is colour-*and*-text (`native` green, `ykman` amber, `manual` purple) — colour is
never the only carrier of meaning. The operation column is monospace and selectable so it can
be pasted into a ticket. Secrets render as `<FIDO2-PIN>`.

"Execute on key" is present and **disabled**: the screen states its own limitation rather
than implying capability it does not have.

### Terms

Where the wording of the consignment term is edited — the term is institutional text
somebody else owns, which is why it is data and not a constant in the source.

- **Language** selects which translation is being edited; badges say `version N` (or
  `not stored yet`), `unsaved changes`, and whether the build ships this language.
  *Add a language* takes a BCP 47 tag and starts a term for it.
- **Title** and **Body** are the document, length-bound like every other input
  (`MAX_TEXT`, `term::MAX_BODY`). Under the body, a live verdict: `renders` plus the
  variables in use, or the refusal — an unknown `{{variable}}` is caught here, at the
  desk, instead of at the counter with the holder waiting.
- **Preview** renders the draft against a sample context of obviously fictitious values.
  Every sample variable is filled on purpose, so the preview shows every line the real
  document can print — line omission is what the operator needs to *see*.
- **Save as new version** writes version *N+1*; it never overwrites. **Reload stored
  version** discards the edit, and **Restore built-in wording** brings back the text this
  build ships (in the editor — nothing is stored until you save). Switching language with
  unsaved changes is refused, naming both ways forward.
- **Versions on record** lists the versions of that language, marking which one new terms
  use and which are kept because something may have been signed against them.

The audit trail gets `term.template_edited` or `term.template_added` with the id,
language, new version and the version it came from.

### Audit

The trail, newest first (500 entries), with "Verify chain". A broken chain reports as
`AUDIT CHAIN BROKEN: …` in the status bar — loudly, because it means something is wrong that
no other screen will tell you about.

### Settings

Operator and organisation (persisted between sessions); the database path, locking mode and
whether it is password-protected; the device transport; the recent databases; and the
actions — *Switch database…*, *Open another…*, *Create new…*, integrity check, backup,
reload. Also the version string, so a screenshot identifies the build.

## Theme

The look comes from [`egui-elegance`](https://github.com/stephenberry/egui-elegance)
0.15 (`use elegance::…`), which targets the same egui 0.36 this app uses.

`ui::install_theme` runs at the top of every frame, before anything paints. It is
cheap to call repeatedly: `Theme::install` compares against the theme already in
context memory and skips the style write when nothing changed. Installing restyles the
stock egui widgets too, so a plain `ui.label` inherits the palette.

Four palettes ship, all built in: **Slate** (default, dark), **Charcoal** (dark),
**Frost** and **Paper** (light). The operator picks one in *Settings → Operator →
Theme*; the name is stored in `settings.json` (`theme`), and
`settings::normalise_theme` resolves an unknown, differently-cased or absent name to
Slate — a settings file written by a newer build still opens.

What the screens share, all in [`src/ui/mod.rs`](../src/ui/mod.rs):

| Helper | Use |
|---|---|
| `screen_header` | Title + explanatory line, top of every screen |
| `card` / `titled_card` | A card at the page width, optionally captioned |
| `table` | A striped grid with a shared header, spacing and its own horizontal scroll |
| `form_columns` | Two columns splitting the card, each field taking its column |
| `hint` / `faint` | Small muted prose; muted table cells |
| `mono` | Serials, e-mails, paths, digests — selectable |
| `error_label` | A refusal: tinted-danger frame, selectable wrapped text, full width |
| `notice` | A non-error banner (`elegance::Callout`) |
| `capped_input` / `capped_area` | A length-bound text field |
| `table_header` | The header row of a grid (`table` calls it; a label/value grid can too) |
| `status_badge` | A key's lifecycle state, tone-coded |
| `row_button` / `row_button_danger` | Small actions inside a table row |
| `GUTTER` | The page margin the shell puts on all three panels |

Two deliberate departures from the crate's defaults:

- **`error_label` is not a `Callout`.** A callout paints its body text, and rule 3
  below requires a refusal the operator can select and copy. `error_label`
  reproduces the callout's tinted-danger frame around a real selectable
  `egui::Label`.
- **Inputs are capped by hand.** `elegance::TextInput` has no `char_limit`, so
  `capped_input` applies `domain::clamp_text` right after painting the field —
  counting characters, not bytes, so a name in Cyrillic is bounded like one in ASCII.

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
7. **Length-bound inputs**: every field goes through `ui::capped_input` /
   `capped_area` with the matching domain bound.
8. The central panel scrolls vertically; the window never has to be resized to reach
   a button, and never has to be scrolled sideways to read a screen.
9. **Width comes from the layout**, through `ui::card`, `ui::form_columns` and
   `ui::table` — see [Layout](#layout). A width constant in a screen has to say why
   it exists.

## Planned

Search and filtering (the tables will not scale past a few dozen rows), sortable columns, a
run view with live per-step status and touch prompts, secret prompt panels, a pre-flight
confirmation, window-state persistence, a log panel, pt-BR localisation, and an accessibility
pass (contrast measured against the four palettes, font scaling).

See [`../features/gui-shell.md`](../features/gui-shell.md) and
[`../features/gui-bootstrap-wizard.md`](../features/gui-bootstrap-wizard.md).

## Testing boundary

Paint code is not unit tested. Everything behind a button lives in a `YkDistApp` method
(`detect_keys`, `submit_holder`, `submit_distribution`, `return_key`, `build_plan`,
`record_dry_run`, `verify_audit`, `save_term_template`) and is covered by the behaviour
suites through the same store calls. If something in the UI is hard to test, that is the
signal to move it down — not to skip the test.

The Terms screen is the current example of that rule: the paint code holds buffers and
buttons, while the decisions — the next version number, what makes a template storable,
which version a term is generated from, whether an edit is unsaved — are
`term::next_version`, `TermTemplate::check`, `term::choose_template` and `term::is_edited`,
all covered by `tests/unit_term.rs` and `tests/behaviour_terms_and_documents.rs`.
