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

**Done.** `src/logging.rs`:

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

### Relationship to the audit trail

Different mechanisms on purpose (`features/audit-trail.md`): the log is
operational and may rotate; the audit trail is accountability and never changes.
Some events appear in both — a bootstrap step failure is a log line *and* an audit
entry.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Single entry point, G-002 format, three levels | Done | `src/logging.rs` |
| 2 | File sink with rotation | Todo | today output goes to stderr, which a GUI user never sees |
| 3 | "Show log" panel in the GUI | Todo | last N lines, copyable, so an operator can send them without finding a file |
| 4 | Structured (JSON) sink option | Todo | keep the same three fields; needs ESI agreement before diverging from the text format |
| 5 | Correlation id per bootstrap run | Todo | one id threading every step's log lines and audit entries |

Phase 2 matters more than it looks: a desktop app that logs to stderr has, in
practice, no log.

## Audit events

This feature emits none of its own. It is the mechanism others log through.

## Tests

Unit tests in `src/logging.rs`:

- `levels_map_to_three_categories`
- `first_event_field_wins_and_rest_become_details`

Phase 2+ adds a test that the file sink produces a line matching the G-002 regex
exactly.

## Open questions and gates

- Log **retention** is not fixed by the norm; ESI decides (same decision as audit
  retention).
- If a JSON sink is wanted, the divergence from the specified text format needs
  ESI agreement.

## References

- `src/logging.rs`
- G-002 §Logs; NRM §5.3.10, §5.3.5
