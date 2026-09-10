# Feature: Renderer fallback

## Summary

When a start dies asking the graphics driver for a window, the next start asks
for a different backend instead of the same one. The rung that finally produces a
window is remembered, so the recovery happens once rather than every second
launch.

## Motivation

Reported from a workstation on 2026-09-09, against 0.19.2: the application did
not open. Nothing appeared — no window, no message, no crash dialog.

The start-up record from `features/logging.md` phase 6 answered it on its first
use in the field. Two launches, a minute apart, and both logs end on the same
line — `egui-wgpu` listing the four adapters it found, which it prints *after*
choosing one, immediately before `adapter.request_device`:

```
app.start.previous_incomplete ; nivel=Erro stage=window
    explanation=it stopped asking the windowing system for a window — usually the
    graphics driver, a remote or headless session, or a display that is no longer attached
```

After that line: no `app.panic`, so it was not a Rust panic; no
`app.window.failed`, so `run_native` never returned an error; no `app.stopped`.
The process was gone mid-call, inside the driver. The machine is an Intel HD
Graphics 630 on driver 31.0.101.2111 — a Kaby Lake iGPU on a driver Intel now
only supports as legacy — and the adapter chosen was the **Vulkan** one, because
`egui-wgpu` defaults to `Backends::PRIMARY | GL` with `PowerPreference::HighPerformance`.

`WGPU_BACKEND=dx12` started it immediately. That is the whole argument for this
feature: the workstation has a perfectly good Direct3D 12 driver for the same
GPU, four adapters were on offer, and the application died because it tried
exactly one of them and had no way to try another. The fault is not "this machine
cannot draw" — it is "the backend preferred here is the one that faults", and an
application that stops at that is choosing to be unusable on hardware that works.

It cannot be left to the operator. The failure is silent by construction: a
windows-subsystem binary with no console, no window and no dialog. Nobody at a
reception desk is going to discover an environment variable, and a support call
that starts with "it does nothing" costs more than the ladder does.

## Current state

**Done, all six phases**, shipped in 0.20.0, and **phase 7** — Direct3D 12 as the
first rung on Windows rather than the second — pending release.
[`src/renderer.rs`](../src/renderer.rs) decides, the start-up marker carries the
attempt, `settings.json` remembers the answer, `--diagnose` and the About box
print it, and **Settings → Graphics** shows it with a warning when a start died in
the driver. The ladder is per platform: Windows has both rungs (the reported
fault), Linux has the OpenGL one, macOS has none — Metal is the only backend
there, so a start that dies has not run out of backends, it has run out of
graphics.

## Design

### Why the next start, and not a retry

Nothing here can be retried in process. A driver fault is not a `Result` and not
a panic: the process is gone during `request_device`, so there is no `else`
branch to run and nothing to catch. The only place a second attempt can happen is
the **next** start, and the only thing that survives to tell it what to do
differently is the marker `features/logging.md` phase 6 already writes.

That marker named the stage. It now names the attempt as well —
`stage=window renderer=automatic` — which is the single field this whole
mechanism needs.

| The marker left behind | The next start does (Windows) |
|---|---|
| nothing | the remembered renderer, or the top of the ladder — Direct3D 12 |
| `stage=window renderer=dx12` | step down — OpenGL |
| `stage=window renderer=gl` | stay; the ladder is out of rungs, and says so |
| `stage=window renderer=automatic`, or no `renderer=` at all | start the ladder at the top — Direct3D 12 |
| any other stage | nothing — that start died of something else |

The last two rows are the ones worth defending. **Staying** at the bottom rather
than starting the ladder again: three backends faulting is not a backend problem,
and cycling would give the operator a different failure every launch instead of a
stable one to report. **Ignoring other stages**: a start that died building the
application had its window, and stepping the renderer down would hide a register
failure behind a graphics change.

### Why Windows starts at Direct3D 12 (phase 7)

The ladder as first shipped began at wgpu's own preference, on the reasonable
principle that a machine where nothing has failed should be left alone. Applied to
Windows, that principle is wrong, and the log that arrived on 2026-09-10 is why:
the same workstation, three more launches, each one ending on the adapter list
with the Vulkan adapter chosen, each one leaving a marker the *next* launch then
acted on. Recovering on the next start is recovery, but what the operator sees is
an application that fails to open, and then opens.

wgpu's preference on Windows is Vulkan, and the Vulkan driver is the only thing
that has ever faulted here. Direct3D 12 is the interface Microsoft ships and
supports with the operating system, every driver on a supported Windows has one,
and it is what fixed the reported machine outright. So the Windows ladder is now
**Direct3D 12 → OpenGL**, and wgpu's preference is not a rung of it at all — it
stays reachable through `$YKDM_RENDERER=auto` and `$WGPU_BACKEND`, which is where
a deliberate probe belongs. Linux keeps the default → OpenGL, macOS keeps Metal
and nothing else: neither has had a fault reported, and restricting the backends
on a machine that works is a way to break one.

Two consequences fell out of it, both in `next_after`:

- An attempt that is **not a rung of this platform's ladder** has used up none of
  it, so the untried rungs are all of them and the next one is the top. That is
  what a marker with no `renderer=` field means — a build from before this
  module, i.e. every un-upgraded workstation still leaving markers behind — and
  the rung it wants is Direct3D 12, not "whatever follows wgpu's preference".
- A **refused** window steps the ladder down like a fatal one. Asking for a single
  backend means a machine with no driver for it gets an `Err` out of `run_native`
  rather than dying, so `main` no longer clears the marker on that path
  (`features/logging.md`, *What a failed start leaves behind*).

### Why the answer is remembered

A ladder alone recovers exactly once. The marker is removed the moment a window
exists, so the next start would find nothing, return to the default, and fault
again — an application that works every second launch, which is arguably worse
than one that never works, because nobody believes the bug report.

So the rung that produced a window is written to `settings.json`, beside the
window geometry, which is the other thing remembered about how this workstation
comes up rather than about what the register contains. It is written from the
`run_native` creator closure, before `YkDistApp` loads the settings, and only
when it changes — a start that confirms what was already known must not rewrite
the file.

`Automatic` is written like any other rung: a workstation that needed Direct3D 12
and stops needing it should stop being told to use it. It is also the value that
means *nothing has been remembered yet* — a settings file written before this
feature has it — and on Windows the ambiguity cannot bite, because `Automatic` is
not a rung there and so is never what produced a window.

### The two environment variables

`$WGPU_BACKEND` is wgpu's own, `egui-wgpu`'s default configuration already reads
it, and it is the first thing a support call reaches for. When it is set this
feature **stands aside entirely** — no restriction is applied, no ladder is
walked, and `--diagnose` says so. A fallback that argued with the person standing
at the machine trying backends by hand would be worse than no fallback.

`$YKDM_RENDERER` is the same probe in this tool's vocabulary (`automatic`,
`dx12`, `gl`, and the spellings somebody would actually try). Neither variable is
remembered: an environment variable is how a workstation is *tested*, and a test
that quietly became a permanent setting would outlive whoever typed it.

### What is touched, and what is not

Only the backend list, and only inside the configuration `eframe` hands out.
Everything else in it — the low-latency surface configuration, the device limits,
the display handle filled in later — is left alone, because none of it is what
faulted and a `WgpuConfiguration` built from scratch here would silently drop
whatever `eframe` puts there next.

The power preference is deliberately not changed either. With the backends
restricted to one, the only competitor left on the reported machine is the
Microsoft Basic Render Driver, and `HighPerformance` already prefers the real GPU
to it.

### Where the decision lives

`renderer::decide_on` takes the ladder as an argument and reads no environment
and no disk: every rung of the Windows ladder is therefore exercised by tests on
whichever platform runs them, which is the split AGENTS.md §4 asks for. `main.rs`
holds no decision — it reports the one it was given, hands the backends to
`eframe`, and writes the marker.

## Phases

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | The marker carries the attempt | 3 | **Done** | `logfile::note_stage_with`, `logfile::field_of`; a marker from 0.19.2 has no `renderer=` and reads as the platform default, which is what that build did |
| 2 | The ladder, and stepping down | 3 | **Done** | `src/renderer.rs`: per-platform rungs, `decide_on` pure and tested against the three-rung ladder on every platform |
| 3 | Remember what worked | 3 | **Done** | `settings.renderer`, written from the creator closure once a window exists |
| 4 | Say so | 3 | **Done** | `app.renderer` (warn when it stepped down or ran out of rungs), `app.renderer.remembered`, and a `renderer:` line in `--diagnose` |
| 5 | Operator override | 3 | **Done** | `$YKDM_RENDERER`, documented in `--help`; `$WGPU_BACKEND` takes precedence over everything and is left to wgpu |
| 7 | Direct3D 12 first on Windows | 3 | **Done** | The ladder becomes `dx12 → gl`, wgpu's preference leaves it, an off-ladder attempt starts from the top, and a refused window steps down as well as a fatal one. From three more launches of the same workstation, on 2026-09-10 — recovering on the next start still shows the operator a failure |
| 6 | A visible explanation on screen | 3 | **Done** | **Settings → Graphics**, beside the device transport, plus the renderer in the About box's report. Reports, deliberately without a picker — see *Why the card does not offer a choice* |

### Why the card does not offer a choice

The Graphics card sits next to the device-transport card and looks like it, which
makes the missing dropdown the first question anybody asks. Two reasons, and the
second is the important one.

The backend is chosen **before there is a window to put a control in**. Anything
selected in a running application would do nothing until the next launch, and a
control that appears to work and does not is worse than no control.

And a persisted preference would fight the mechanism this feature *is*. The point
is that the machine discovers what works on itself; the settings field is a record
of that discovery, not an operator's opinion. Turning it into a picker would mean
an operator could pin a workstation to a backend, forget, and have a driver update
that fixed Vulkan never take effect — with nothing on screen to explain why. So
the card reports, `$YKDM_RENDERER` remains the escape hatch, and it remains
unremembered.

### Two places, one decision

`Report::gather` re-derives the renderer from the settings file and the marker.
That is correct for `--diagnose`, which runs before any start and has nothing else
to go on, and wrong inside a running window: the marker is removed the moment the
window appears, so a re-derivation reports a start that has just *stepped down* as
merely "remembered" — losing the one fact worth reporting.

So `YkDistApp` carries the decision, initialised from the settings file (which is
what a `YkDistApp` built by a test can honestly say) and replaced by `main` with
what that start actually did. Both the About report and the Settings card read
that one field. The About box's own documentation already warned against a second
answer to the same question drifting from the first; this keeps them the same
answer.

## Audit events

None. Nothing here changes the register: no key, holder, distribution or run is
touched, and the only persisted state is one field of the workstation's own
settings file — the same category as the window geometry. The decision is
recorded in the log (`app.renderer`) and in `--diagnose`, which is where a
support call reads it.

## Tests

In-source in [`src/renderer.rs`](../src/renderer.rs) — all pure, and all against a
ladder passed in as an argument, so the Windows one is exercised by a macOS run:

- `a_workstation_where_nothing_has_failed_gets_the_top_of_its_ladder`
- `windows_does_not_offer_wgpu_s_own_preference_as_a_rung`
- `a_start_that_died_asking_for_a_window_steps_down_one_rung`
- `the_ladder_is_walked_one_rung_at_a_time_not_jumped_to_the_bottom`
- `the_bottom_rung_dying_stops_stepping_and_says_the_fault_is_elsewhere`
- `a_marker_from_a_build_that_had_no_ladder_is_read_as_the_default_rung`
- `a_start_that_died_after_it_had_a_window_does_not_move_the_ladder`
- `the_rung_that_worked_is_used_again_rather_than_rediscovered`
- `wgpu_s_own_variable_wins_and_is_not_overridden_by_the_ladder`
- `an_empty_environment_variable_is_not_a_choice`
- `an_explicit_choice_overrides_the_remembered_rung_and_is_not_remembered`
- `an_unrecognised_choice_is_ignored_rather_than_failing_the_start`
- `every_spelling_an_operator_would_try_names_a_rung`
- `a_rung_this_platform_does_not_have_starts_the_ladder_from_the_top`
- `this_platform_s_ladder_starts_where_it_should_and_repeats_no_rung`
- `every_rung_has_a_distinct_slug_and_reads_back_as_itself`
- `every_reason_explains_itself_naming_the_renderer`
- `only_a_start_that_died_in_the_driver_is_worth_a_warning`
- `exactly_the_outcomes_worth_a_warning_have_something_to_say_on_screen`
- `an_alert_is_read_in_a_window_that_exists_so_it_says_which_start_it_means`
- `restricting_the_backends_leaves_the_rest_of_the_configuration_alone`
- `the_default_rung_restricts_nothing_so_wgpu_keeps_its_own_order`
- `the_rung_that_worked_is_written_down_only_when_it_is_news`

In-source in [`src/logfile.rs`](../src/logfile.rs), for the marker field:

- `a_stage_can_carry_the_field_the_next_start_has_to_act_on`
- `extra_fields_go_before_the_timestamp_so_nothing_after_them_is_lost`
- `a_marker_from_a_build_with_no_such_field_reads_as_absent_not_empty`
- `a_field_is_not_matched_by_the_tail_of_another_ones_name`

`tests/unit_settings.rs` covers the round trip of the new field through a
settings file written before it existed.

In `tests/behaviour_startup_logging.rs`, for phase 7's marker change:

- `a_window_the_platform_refused_leaves_the_marker_so_the_next_start_steps_down`
- `a_window_that_was_created_and_then_failed_does_not_accuse_the_next_start`

What is **not** tested, and cannot be: that Direct3D 12 works where Vulkan
faults. That is one driver on one machine, it is the fault this exists for, and
the evidence is the log quoted under *Motivation* plus `WGPU_BACKEND=dx12`
starting the same binary on the same workstation.

## Open questions and gates

Nothing for ESI or the DPO: no personal data, no security premise, no
integration. The renderer is a property of the workstation's graphics stack.

## References

- `features/logging.md` phase 6 — the start-up marker this reads
- [`docs/operations.md`](../docs/operations.md) — *"the application will not start"*,
  the runbook this feature changes: stage `window` now usually needs nothing but a
  second launch
- `egui-wgpu` 0.36 `setup.rs`, `WgpuSetupCreateNew::default` — where
  `Backends::PRIMARY | GL` and `PowerPreference::HighPerformance` come from
