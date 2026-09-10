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

**Done — phases 1, 2 and 3.** `src/logging.rs` is the entry point and the format,
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
- Filtering via the `YKDM_LOG` environment variable (`EnvFilter`), default `info`.
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

Two deliberate non-behaviours. The panic hook does **not** touch the marker: a
panic during start-up is already covered by the stage on disk, and one three
hours into a session would otherwise accuse the next launch of a start-up failure
that never happened. And a launch that gets no window still clears the marker
before returning, because `app.window.failed` has already recorded that properly
and a second report of the same failure is noise.

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
| 6 | The start-up procedure records itself | 3 | **Done** | panic hook, `app.window.failed`, and the stage marker a launch leaves behind when it dies too abruptly to log. Its first use in the field found a Windows workstation whose Vulkan driver killed the process inside `request_device`, which is now recovered from automatically (`features/renderer-fallback.md`) — the marker carries the graphics backend as well as the stage. Added with phase 2 rather than specified ahead of it: the file sink is what made the question answerable, and the question — "it does not open and there is nothing to look at" — is the one that motivated the phase |

Phase 2 mattered more than it looked, and the evidence is that finishing it
uncovered two things nobody had noticed: the *Show log* panel had never been
connected to the logging layer and could only ever show "0 log lines" (phase 3 was
recorded as done in two specs), and the whole start-up path had no way of
reporting a failure at all — `run_native`'s error went into `main`'s return value
and a panic went to a stderr that does not exist in a windows-subsystem binary.

## Audit events

This feature emits none of its own. It is the mechanism others log through.

## Tests

Unit tests in `src/logging.rs`:

- `levels_map_to_three_categories`
- `first_event_field_wins_and_rest_become_details`

In-source tests in `src/logfile.rs`: rotation (that it happens, that the
generations move along in order, that the oldest is dropped), that a second run
appends rather than truncating, that a line longer than the whole budget is still
written whole, that `describe` creates nothing, and the marker's lifecycle.

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

`tests/behaviour_startup_logging.rs` reproduces `main`'s sequence, which the real
one cannot be tested through because it ends in `eframe::run_native` and a test
binary has no display:

- `a_start_that_never_reached_a_window_is_reported_at_the_next_one`
- `a_start_that_succeeded_does_not_accuse_the_next_one`
- `the_marker_names_the_last_stage_the_launch_reached`
- `what_the_dying_launch_managed_to_log_is_on_disk_afterwards`
- `the_diagnostic_report_names_a_start_that_never_finished`

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
