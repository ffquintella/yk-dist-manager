# Feature: Logging

## Summary

One logging entry point for the whole application, three levels, and the log line
format that guide G-002 specifies.

## Motivation

G-002 requires a single logging rule or library across the application, at least
three categories (Informação / Aviso / Erro), the line format
`[dd/mm/aaaa] hh:mm:ss ; evento ; detalhes`, and that **every** error is recorded —
no swallowed exceptions, and errors never rendered to the user's screen instead of
the log.

The practical reason is the same as the normative one: when a bootstrap fails
halfway through on a key that is already half-configured, the log is what tells the
operator which step got there.

## Current state

**Done — phases 1, 2, 3, 6 and 7.** `src/logging.rs` is the entry point and the format,
`src/logfile.rs` is what reaches the disk, and `src/logbuf.rs` is what the panel
shows. One formatted line, three sinks, because a panel or a file showing a
different shape of line from the one in the ticket is how a support conversation
goes wrong.

`src/logging.rs`:

- `logging::init()` installs a `tracing` subscriber with a custom
  `FormatEvent` implementation, `FgvFormat`, that emits exactly the G-002 layout.
- `level_label` maps `tracing` levels onto the three categories:
  `ERROR → Erro`, `WARN → Aviso`, everything else → `Informacao`.
- The `evento` slot is taken from the first of `message`, `event` or `evento`;
  every other field becomes `key=value` in `detalhes`. Call sites therefore never
  format a line by hand:

  ```rust
  tracing::info!(event = "key.detected", serial = 20423633);
  // [10/08/2026] 14:32:05 ; key.detected ; nivel=Informacao serial=20423633
  ```
- Filtering via the `YKDM_LOG` environment variable (`EnvFilter`), default
  `DEFAULT_FILTER` = `info,yubikey=warn`. The crate-level exception is phase 7's,
  and is argued for there.
- `metadata()`, `metadata_line()` and `render_line()`: what identifies this build
  and this process, the G-002 line it is written as, and the one function every
  line in this application is formatted by — including `FgvFormat`'s, so the
  hand-written first line of a file cannot drift from the rest of it.
- `try_init` so a second call in a test binary is harmless.
- `Sinks` is the `MakeWriter` every line goes through: the rotating file, the
  in-memory ring, and stderr. `make_writer_for` is what carries the severity into
  the ring, so the panel can filter on it without parsing the line back.
- `install_panic_hook()` records `app.panic` with the location and a flattened,
  capped message, then defers to the previous hook so a debug build keeps its
  backtrace.

`src/logfile.rs`:

- `logs/` under [`crate::paths::data_dir`] — `%APPDATA%\yk-dist-manager\logs\` on
  Windows, `~/.local/share/yk-dist-manager/logs/` on Linux,
  `~/Library/Application Support/yk-dist-manager/logs/` on macOS. `$YKDM_LOG_DIR`
  overrides it; `$YKDM_DATA_DIR` moves it with everything else, which is what the
  tests use.
- `yk-dist-manager.log` plus `yk-dist-manager.log.1` … `.5`, rotated at `MAX_BYTES` (1 MiB), so
  the ceiling is fixed however long the application is left open. Size-based
  rather than daily: a day with one hand-over produces an empty file, and a day
  spent looping on a failing reader produces an unbounded one.
- **Unbuffered.** No `BufWriter`, because the line this module exists for is the
  last one before a crash.
- A log directory that cannot be opened costs the file sink and nothing else. The
  application still starts and the panel still fills — refusing to launch because
  the diagnostics are unavailable gets the priority backwards.
- The **start-up marker**: `note_stage`, `finished`, `previous_attempt`,
  `stage_of`, and `explain`, which turns a stage into the sentence an operator can
  act on.
- `with_header`: a line written at the head of every generation a **rotation**
  opens. Not on open, because the session that opens a file writes the same line
  to every sink as it starts; on rotation, because a generation that turned over
  in the middle of an afternoon would otherwise carry no version anywhere in it.

`src/logbuf.rs`:

- `shared()` is the one ring the layer writes to and the panel reads from. Before
  it, `YkDistApp` held a `LogBuffer::new()` of its own and the panel could only
  ever show "0 log lines" — phase 3 was built, shipped and never connected.

## Design

### Rules for call sites

- Always pass `event = "dotted.name"`; never interpolate a sentence.
- Never pass a PIN, PUK, management key, access code or database password as a
  field. There is no redaction layer — the rule is enforced by review and by the
  fact that secrets exist as `Arg::Secret` placeholders in the plan.
- Never pass a whole request/response body or an unfiltered error chain that could
  contain one.
- `Result` is never discarded silently. Either handle it or log it at `error`.

### Levels, concretely

| Level | Use here |
|---|---|
| `Informacao` (`info`, `debug`, `trace`) | key read, record written, plan built, backup taken |
| `Aviso` (`warn`) | reader unavailable, unexpected journal mode, ykman version drift, optional step skipped |
| `Erro` (`error`) | audit append failed, database read failed, bootstrap step failed, unlock failed |

### What a failed start leaves behind

The hardest failure to diagnose is the one where nothing appears: no window, no
console, no message. Three mechanisms, because the three ways a launch can fail
leave different amounts behind.

| Failure | Caught by | Evidence |
|---|---|---|
| The application fails on its own terms | the panic hook | `app.panic`, with `location=` and `message=` |
| The window system or driver refuses a window | `run_native`'s `Err` | `app.window.failed`, with the reason |
| The process dies outright — driver fault, `abort`, the OS killing it | the start-up marker | the stage it never got past, read and reported by the *next* launch |

The marker is a file in the log directory holding the last stage entered, the
version and the commit — and, where a stage needs it, the attempt as well: the
`window` stage records **which graphics backend** was being asked for, which is
what lets the next start ask for a different one
(`features/renderer-fallback.md`, built on this phase and the first fault it
diagnosed in the field). `main` writes it before each stage and removes it once
there is a window with an application behind it, so finding one at the next start
means that start never finished. The stages are
`start` → `camera-preflight` → `settings` → `window` → `app-construction`, and
they are named rather than free text because two places have to agree on them:
the launch that writes one, and `explain()`, which turns the one found afterwards
into a sentence — *"it stopped asking the windowing system for a window — usually
the graphics driver, a remote or headless session, or a display that is no longer
attached"*.

One deliberate non-behaviour, and one decision reversed. The panic hook does
**not** touch the marker: a panic during start-up is already covered by the stage
on disk, and one three hours into a session would otherwise accuse the next launch
of a start-up failure that never happened.

The reversal: a launch whose window was **refused** — `run_native` returning `Err`
— used to clear the marker before returning, on the argument that
`app.window.failed` had already recorded the failure properly and a second report
of it was noise. That argument was right about the log and wrong about the ladder.
Since Direct3D 12 became the first rung a Windows start asks for
(`features/renderer-fallback.md`), a workstation with no driver for the backend
being asked for gets an error rather than a dead process — and for the purpose of
choosing which backend to try next, a refused window and a window that killed the
process are the same fact. The marker is therefore left where it is, and the price
is one duplicate `app.start.previous_incomplete` line at the next start. A window
that was created and failed later is unaffected: the marker was removed the moment
the window existed, so there is nothing there to leave.

`--diagnose` prints both the log path and any unfinished start, which matters
because every other line in that report describes a process that *did* start — it
is the one printing the report.

### Relationship to the audit trail

Different mechanisms on purpose (`features/audit-trail.md`): the log is
operational and may rotate; the audit trail is accountability and never changes.
Some events appear in both — a bootstrap step failure is a log line *and* an audit
entry.

## Phases

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | Single entry point, G-002 format, three levels | 0 | Done | `src/logging.rs` |
| 2 | File sink with rotation | 3 | **Done** | `src/logfile.rs`: `logs/` under the per-user data directory, 1 MiB × 5 generations, unbuffered. Taken with the start-up record below, which is what makes a launch that produces no window diagnosable at all |
| 3 | "Show log" panel in the GUI | 0 | **Done** | shipped as `features/gui-shell.md` phase 8: [`crate::logbuf`](../src/logbuf.rs) keeps the last N lines and a resizable bottom panel shows them with a level filter and *Copy all* (⌘/Ctrl + L). Recorded here because this spec is where somebody looks for it |
| 4 | Structured (JSON) sink option | — | Todo | keep the same three fields; needs ESI agreement before diverging from the text format |
| 5 | Correlation id per bootstrap run | 2 | Todo | one id threading every step's log lines and audit entries |
| 7 | The log says which build, platform and process wrote it | 3 | **Done** | `app.build` at the head of every session and every rotated generation: `version`, `commit`, `build`, `os`, `arch`, `pid`, `features`. Plus the default filter that keeps the card library's per-poll line out of the file. Both came from one log collected in the field on 2026-09-10 — see below |
| 6 | The start-up procedure records itself | 3 | **Done** | panic hook, `app.window.failed`, and the stage marker a launch leaves behind when it dies too abruptly to log. Its first use in the field found a Windows workstation whose Vulkan driver killed the process inside `request_device`, which is now recovered from automatically (`features/renderer-fallback.md`) — the marker carries the graphics backend as well as the stage. Added with phase 2 rather than specified ahead of it: the file sink is what made the question answerable, and the question — "it does not open and there is nothing to look at" — is the one that motivated the phase |

Phase 2 mattered more than it looked, and the evidence is that finishing it
uncovered two things nobody had noticed: the *Show log* panel had never been
connected to the logging layer and could only ever show "0 log lines" (phase 3 was
recorded as done in two specs), and the whole start-up path had no way of
reporting a failure at all — `run_native`'s error went into `main`'s return value
and a panic went to a stderr that does not exist in a windows-subsystem binary.

### What one log file could not answer (phase 7)

A log collected from a workstation that would not open, 976 KiB, three failed
launches in it. Everything the previous phases promised was there — the stage, the
explanation, the adapter list — and the file still could not answer two questions
that had to be asked over the telephone:

**Which build is this?** Every `app.start` line in it said `version=0.19.2`, on a
workstation everybody involved believed was running 0.20.0 — the release that
fixes precisely the fault the log was collected for. The version was in the file,
in one event, near the top of a generation that had rotated three times: whoever
reads a log next should not have to know that `app.start` is where to look, nor
find a generation that still has one.

**Which process wrote which line?** 3,500 of the file's lines were the `yubikey`
crate's `connected to reader`, at one to two a second, from an instance opened
that morning that was *still running* — interleaved line by line with three
launches that never got a window. Nothing in the file said there were two
processes, and reading it as one process is nonsense: it appears to poll a card
continuously while simultaneously failing to start.

So: `app.build` at the head of every session *and* every rotated generation, with
`pid` in it, and the polling filtered down to `warn`. The metadata is the build,
the platform and a process id — no personal data and no secret (§2 of
`AGENTS.md`), which is what makes it safe to put at the top of a file that gets
e-mailed into a ticket.

Deliberately *not* in it: the log path (already in `app.start`, and the file
naming its own path is circular), the database path (in `app.start`, and it is the
one field here that could name a person's share), and the renderer (chosen after
logging is up, and `app.renderer` records it — a rotated generation loses that
line, and the metadata line is not the place to duplicate a decision made
elsewhere).

## Audit events

This feature emits none of its own. It is the mechanism others log through.

## Tests

Unit tests in `src/logging.rs`:

- `levels_map_to_three_categories`
- `first_event_field_wins_and_rest_become_details`

In-source tests in `src/logging.rs` for phase 7:

- `the_metadata_line_names_the_build_the_platform_and_the_process`
- `the_metadata_line_has_the_same_shape_as_every_other_line`
- `the_default_filter_keeps_the_card_polling_out_of_the_log`

In-source tests in `src/logfile.rs`: rotation (that it happens, that the
generations move along in order, that the oldest is dropped), that a second run
appends rather than truncating, that a line longer than the whole budget is still
written whole, that `describe` creates nothing, the marker's lifecycle, and the
header — `every_generation_a_rotation_opens_starts_by_saying_what_wrote_it`,
`the_header_counts_against_the_budget_like_any_other_line`,
`a_file_with_no_header_rotates_exactly_as_before`.

`tests/unit_logfile.rs` — the parts that need the real subscriber or the real
environment:

- `the_file_sink_writes_the_g002_line_and_nothing_else` — the normative one this
  section previously owed
- `one_event_reaches_the_file_and_the_panel_alike`
- `severity_survives_the_trip_into_the_panel`
- `the_log_survives_a_directory_that_cannot_be_opened`
- `a_long_session_cannot_fill_the_disk`
- `the_environment_decides_where_the_log_goes`
- `a_panic_message_cannot_break_the_line_format`
- `every_generation_of_the_log_says_which_build_and_process_wrote_it` — the
  composition `init` makes, run long enough to rotate

`tests/behaviour_startup_logging.rs` reproduces `main`'s sequence, which the real
one cannot be tested through because it ends in `eframe::run_native` and a test
binary has no display:

- `a_start_that_never_reached_a_window_is_reported_at_the_next_one`
- `a_start_that_succeeded_does_not_accuse_the_next_one`
- `the_marker_names_the_last_stage_the_launch_reached`
- `what_the_dying_launch_managed_to_log_is_on_disk_afterwards`
- `the_diagnostic_report_names_a_start_that_never_finished`
- `a_window_the_platform_refused_leaves_the_marker_so_the_next_start_steps_down`
- `a_window_that_was_created_and_then_failed_does_not_accuse_the_next_start`

And in `src/diagnostics.rs`,
`the_report_names_the_log_file_and_any_start_that_never_finished`.

## Open questions and gates

- Log **retention** is not fixed by the norm; ESI decides (same decision as audit
  retention).
- If a JSON sink is wanted, the divergence from the specified text format needs
  ESI agreement.

## References

- `src/logging.rs`, `src/logfile.rs`, `src/logbuf.rs`
- [`docs/operations.md`](../docs/operations.md) — *Logs*, and *Runbook: the
  application will not start*
- G-002 §Logs; NRM §5.3.10, §5.3.5
