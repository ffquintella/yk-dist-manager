# GUI

egui/eframe 0.36. Eight screens plus the database chooser. The audience is an operator at a
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
│ ⇤18⇥ ▣ YubiKey Distribution Manager 0.10.0 | Inventory …  ⇤18⇥ │  Panel::top
│      Bootstrap Terms Audit Settings                          │
├──────────────────────────────────────────────────────────────┤
│      CentralPanel, inside ScrollArea::vertical               │
│      ┌────────────────────────────────────────────────┐      │
│      │ card — always the page width                   │      │
│      └────────────────────────────────────────────────┘      │
├──────────────────────────────────────────────────────────────┤
│ ⇤18⇥ operator: felipe | db: share | 5 NFC 20423633 attached | … │  Panel::bottom
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

## The top bar, and the About box

The mark sits beside the product name, small — that row is what an operator glances at
to tell this window from the register, the term to sign and a terminal during a
hand-over, which is the whole reason the icon exists
(`features/application-icon.md`).

The **version badge is clickable, and is the About affordance**: it is already the
thing somebody points at when asked which version they are running, so a separate
*About* item would be a second place to look for the same answer. The box shows the
mark, the version, and the **`--diagnose` report** — selectable, with *Copy the
report*, because the point of showing it is that somebody pastes it into a ticket. The
report is gathered when the box opens, not per frame.

## Screens

### Database chooser

Shown whenever no database is open — first run, a locked file, an unreachable share, or
the operator closing one to switch. The application mark is at the top at 96 px: this is
the screen an operator is looking at while a password prompt waits, which is when "am I
typing this into the right application?" is worth answering at a glance. It carries:

- the **recent databases**, each marked *available* or *not reachable* (an unmounted
  share stays listed — that is a network problem, not a decision), with *open*,
  *use path* and *forget*;
- a typed path (which is also how a UNC path or a pasted share path gets in) and a
  password field, cleared immediately after use;
- **Open** and **Create** as separate buttons, plus native *Choose file…* /
  *New file…* dialogs;
- a note when the build lacks `file-dialog` or `encrypted-db`, so a refused password
  is explained rather than mysterious;
- the **strength meter**, but only when the typed path is not a file yet — which is
  the only case in which the password would become a *new* register's key. Grading
  a password typed to unlock a register that already has one would be a judgement
  nobody can act on here;
- after repeated wrong passwords, the **wait** the throttle has earned: a banner
  counting down, and the buttons that submit a password disabled while it runs. The
  banner says it is not a lockout, because there is nobody to lift one.

Open and Create never guess: opening a path that does not exist is an error, and
creating over an existing file is refused.

It also carries an **Open from a network share (SMB)** card, so the register on the
unit's file server can be reached without the share having been mounted first: the
location (`smb://…`, UNC or `//server/share/…`), the **identity** as three radio
buttons with a sentence each — the signed-in account (the default), guest, or a named
account — and, only for a named account, a user and a password field that is cleared
the moment it is used. Radios rather than a dropdown because each option needs its
sentence, and because picking the wrong one opens the register as an identity nobody
reviewed. Shares used before are listed with the identity that reached them and a
*use* / *forget* pair; the password is never among what *use* fills in. *Connect and
open* and *Connect and create* stay separate for the same reason **Open** and
**Create** do. On a platform that cannot mount a share itself, the card says so and
names the alternative instead of offering a button that fails.

A database in a cloud-sync folder that another workstation has open produces an **In use
by another workstation** card instead of a bare error: the path, who holds it (operator,
computer, since when), and one action. While that session is alive the action is *Try
again*; once its lock has gone unrefreshed for fifteen minutes it becomes *Take the lock
over*, in the red accent, with the warning that a live session mid-hand-over is exactly
what must not be interrupted. A lock held by this computer says so, because the usual
cause is a second window of this application.

### Inventory

The keys the unit owns. "Read attached key" identifies the key in the reader and inserts or
refreshes its row. Columns: serial, model, firmware, form factor, status, applications,
observation, actions. Per-row actions advance the lifecycle, and a refused transition is
shown verbatim in the status bar.

**Attached now** is what the background watch can see, refreshed on its own while this
screen is open (`features/device-detection.md` phases 2 and 3): a row per key with its
serial, model, firmware and applications. With one key attached it is named and becomes
what every operation acts on. With **several**, a warning says so and each row offers *Use
this one* — nothing is chosen for the operator, because writing a PIN to whichever key a
transport happened to list first is not something an operator can undo. A key that
enumerates but will not describe itself is shown as itself, with the reason, rather than
being counted as absent: that is usually a driver or a permission, and "no key attached"
would send somebody after a cable. Plugging a key in fills this list and the wizard's
serial field and **records nothing** — recording stays a click. The card says how often it
looks, and that it never looks while a bootstrap is writing to a key.

**Add by serial / scan…** opens the intake panel: a text field (which is what a USB
barcode scanner types into — Enter submits), an *Observation (optional)* field, camera
controls with a preview when the build has the `camera` feature, and confirm/discard for
a decoded serial. A key recorded this way is marked as not verified until it is read from
the hardware. The observation is **kept between adds**, so a whole box shares one note.

**observation…** opens an editor below the table for a key already on record — up to
`domain::MAX_NOTE` characters, with a live character count, saved or cancelled
explicitly. The cell shows one line of it (newlines folded, cut with an ellipsis, an em
dash when empty). A device re-read never touches it.

**remove** never deletes on the click. It opens a panel that names what goes (the
inventory row and its observation), what stays (the audit trail, which no code path can
edit), and what the alternative is — removal is for a mistake at intake, retirement is
for a key going out of service. For a key with a hand-over or a bootstrap run against
it, that panel carries the store's refusal *instead of* the confirm button, with the
counts, so the operator reads why before clicking rather than after.

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

Each row carries the **Term** column, which is the *signature state* rather than a count
of attachments (`features/receipts-and-terms.md` phase 4): `awaiting signature · 3d`,
`overdue · 30d`, `signed`, `returned unsigned · 50d`, or `term not used`. The days are on
the badge because "overdue" without a number is not something an operator can prioritise,
and the whole sentence — including what an unsigned term costs — is on hover. A generated
term filed against a hand-over deliberately does **not** make it read as signed.

One line at the top of the screen says what is outstanding across the register, and
**nothing at all when there is nothing** — a banner that is always on screen is one
nobody reads.

Row actions: *term* opens the panel, *upload* files a signed scan, *record return* closes
an open hand-over, and on a returned row *return receipt* produces the mirror document
with a `no receipt` badge until the signed copy is filed.

The **term panel** serves both documents — the consignment term and the return receipt —
and says which in its heading, because the two are legally different things and a
reviewer must never be unsure which is on screen. It picks a language (falling back with
a visible notice when the requested one has no template), renders from the record, and
offers *Export as PDF…*, *Save as text…*, *Upload signed term…* (or *…receipt…*, filed as
the matching kind) and *Edit wording…* — which opens the Terms
screen on the same language. The PDF is the sheet to print and have signed; the text is
the same document for a ticket, and both come from one rendering so they cannot disagree.
When the term uses a character the PDF font cannot set, an amber notice says which ones
and that the text output carries them correctly — before anything is printed. Under the
language row it names the template that produced the text
(`consignment@2 (pt-BR)`), so an edit is visibly in effect. Below it, the filed documents
are listed with their size, short SHA-256 and an *export* action — which verifies the
digest and refuses on a mismatch.

### Bootstrap

Selection (key serial, holder, template), per-step checkboxes with required steps marked,
then the plan table.

Above the serial field is what is attached: one key is named and its serial fills the field
by itself, and **several** produce a warning and a button per key, because the next thing
this screen does is write to one of them and nothing may be assumed about which. The watch
that feeds it is stopped — and its thread joined — before the first write, so nothing else
is touching the hardware during a run.

The plan table:

| Step | Transport | Operation | Note |

Transport is colour-*and*-text (`native` green, `ykman` amber, `manual` purple) — colour is
never the only carrier of meaning. The operation column is monospace and selectable so it can
be pasted into a ticket. Secrets render as `<FIDO2-PIN>`.

"Execute on key" is present and **disabled**: the screen states its own limitation rather
than implying capability it does not have.

The template list offers the **newest version of each template in use**; *Manage
templates…* opens the Templates screen on whichever one is selected.

### Templates

Where the bootstrap procedure itself is edited. Same argument as the Terms screen: the
procedure is content the unit owns, so changing a step must not mean changing Rust.

- **A signing banner at the top**, because the spec's condition on pilot mode is that it
  be visible: either *signatures are required on this workstation*, or *pilot mode:
  unsigned templates may be run*. A control that silently permits unsigned procedures is
  indistinguishable from having no control.
- **On record** lists every template version: name and id, version, how many steps are
  selected by default, how many runs recorded it, badges (`offered` / `superseded` /
  `retired`, plus `built-in`), the **signature verdict** as a badge with the whole sentence
  on hover, and the date. Per row: *Edit*, *Duplicate*, *Compare*, *Export*,
  *Retire* / *Reinstate*, and *Remove*.
- **Compare** shows what changed between two versions of one procedure, from two version
  pickers. Structural rather than a text diff: steps are matched by id, so a step that
  changed places reads as **moved** — the fact that matters, since the order of the steps
  is the order of execution. Every row says its kind of change in words as well as by
  colour.
- **Share a procedure with another unit** exports a version as one readable JSON file
  (with the canonical bytes beside it, for signing) and imports one. Import is a
  *preview*: what the file contains, whether its signature verifies here, and a diff
  against the newest version this register holds — and only then *Store as a new version*.
  The receiving register assigns the version number, and importing the same file twice
  stores nothing. See the runbooks in `operations.md`.
- **Remove is disabled where it cannot be granted**, with the reason on the button — a
  version a run recorded, or one this build ships (that one would be re-created on the next
  open). Both point at retirement instead. When it is allowed, it asks first, in a panel
  that says what goes and what stays.
- **The editor** takes an id (fixed once stored — a run refers to it), a name, a
  description, and the steps. Each step row carries its `enabled` and `required`
  checkboxes, its id, ↑ / ↓ to change the order of execution, *Remove step*, and *Details*
  — which opens the step id, its description and its parameters as `name = value` lines.
  *Add a step* appends one of the twelve kinds **with the parameters that kind reads**
  already filled in.
- **A live verdict**, `plans` or the exact refusal, from a real `plan()` against a
  fictitious holder and key. An unknown `{{variable}}`, a missing parameter, a duplicate
  step id or a template with nothing enabled is caught at the desk — and the store applies
  the same gate, so nothing unplannable can be saved.
- **Save as new version** writes version *N+1* and never overwrites; *Reload stored
  version* discards the edit; *Restore built-in steps* brings back what this build ships
  (in the editor — nothing is stored until you save). Starting or duplicating a template
  with unsaved changes is refused, naming both ways forward.
- **Variables** lists every name a description or parameter may use, with the value the
  draft check substitutes.

The audit trail gets `template.created`, `template.changed`, `template.retired`,
`template.reinstated` or `template.removed` — with the id, version, previous version, step
count and run count, and never the procedure text.

### Terms

Where the wording of the holder-facing documents is edited — institutional text somebody
else owns, which is why it is data and not a constant in the source.

- **Document** selects which one: the *consignment term* the holder signs on receiving a
  key, or the *return receipt* that closes the custody loop when it comes back. A picker
  rather than two screens, because the return receipt is a second template id and nothing
  more — everything below it is identical.
- **Language** selects which translation is being edited; badges say `version N` (or
  `not stored yet`), `unsaved changes`, and whether the build ships this language.
  *Add a language* takes a BCP 47 tag and starts a term for it.
- **Title** and **Body** are the document, length-bound like every other input
  (`MAX_TEXT`, `term::MAX_BODY`). Under the body, a live verdict: `renders` plus the
  variables in use, or the refusal — an unknown `{{variable}}` is caught here, at the
  desk, instead of at the counter with the holder waiting.
- **Two or more spaces make a column.** Whatever follows such a gap stays at that
  position however long the substituted value before it turns out to be, which is what
  keeps a two-column signature block aligned for a holder called *Yu* and one called
  *Maria da Conceição Albuquerque Fonseca*. A single space is spacing and is left
  alone, as is a gap at the start of a line — so the indentation of a wrapped clause is
  never touched.
- **Preview** renders the draft against a sample context of obviously fictitious values.
  Every sample variable is filled on purpose, so the preview shows every line the real
  document can print — line omission is what the operator needs to *see*. The preview
  also offers **Export as PDF…**, which is how the wording reaches the people who own
  it: a reviewer reads the document as it will be printed rather than a template full
  of `{{variables}}`. The footer says `@draft`, nothing is stored, and no hand-over is
  involved.
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

A **Password protection** card sets a password on a plain register, changes the one it
has, or takes it off. It is the only control in the application that can make the
register unopenable, so it does not sit as a bare button among the maintenance
actions: it has to be opened, the password is typed twice and graded as it is typed,
the sentence about there being no recovery is on screen while the operator types, and
removal asks for a confirmation of its own that names what becomes readable. The
register is backed up first and reopened under the new password afterwards. In a build
without `encrypted-db` the card says which build would do it instead of hiding.

A **Template signatures** card holds the public keys this deployment accepts on a
bootstrap procedure, and the switch that makes a signature mandatory. Public keys only:
the application verifies signatures and cannot make one, so the private half never comes
near it. A key that could not verify anything is refused as it is typed — a trust store
with a broken key in it reports every template as *altered*, which sends the operator
after the wrong problem. Requiring signatures while trusting nobody is called out as an
error, because it would refuse every procedure including the ones the build ships.

For a database in a cloud-sync folder there is one more row — the **single-writer lock**:
held by this workstation, by whom since when, and the path of the lock file. The warning
that used to be a flat refusal now says what the lock does *and* what it does not cover,
because "locked" must not read as "solved"; if the lock was declined it goes back to being
an error. A sync conflict copy next to the database is reported here as an error too: the
register may have forked, and no other screen would say so.

When the register is on a share **this session connected**, there is a **network share**
row — the share, and as whom — plus a *Close and disconnect the share* action. Both
appear only for a connection this session made: a share the operator mounted themselves
is not this application's to take down, so the button would be a lie.

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
`record_dry_run`, `verify_audit`, `save_term_template`, `save_template`) and is covered by
the behaviour suites through the same store calls. If something in the UI is hard to test,
that is the signal to move it down — not to skip the test.

The Terms and Templates screens are the current examples of that rule: the paint code holds
buffers and buttons, while the decisions live below it and are covered by tests —
`term::next_version` / `versioning::next_version`, `TermTemplate::check` /
`BootstrapTemplate::check`, `term::choose_template` / `template::latest_per_id`,
`term::is_edited` / `TemplateDraft::is_dirty`, and the whole editing model in
`template::draft` (add, remove and move a step, parse the parameter lines). Covered by
`tests/unit_term.rs`, `tests/unit_template.rs`,
`tests/behaviour_terms_and_documents.rs` and `tests/behaviour_templates.rs`.
