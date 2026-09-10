//! Which graphics backend to ask for, and what to do when the last attempt
//! killed the process.
//!
//! `features/renderer-fallback.md`. The fault this exists for was reported from
//! a workstation with an Intel HD Graphics 630 on driver 31.0.101.2111: the
//! application wrote its start-up lines, `egui-wgpu` enumerated four adapters,
//! chose the Vulkan one — and the process died inside `request_device`, in the
//! driver, before `eframe` could return an error or the panic hook could catch
//! anything. There was no window and nothing on screen to explain it.
//!
//! `WGPU_BACKEND=dx12` fixed it outright, which is the useful part: the machine
//! has a working Direct3D 12 driver for the same GPU, and only the Vulkan one is
//! broken. So the failure is not "this workstation cannot draw" — it is "the
//! backend wgpu prefers here is the one backend that faults", and an application
//! that dies rather than trying the next one is choosing to be unusable.
//!
//! ## Why a ladder and not a retry
//!
//! Nothing can be retried in-process. A driver fault is not a `Result` and not a
//! panic — the process is gone mid-call, so there is no `else` branch to run.
//! The only place a second attempt can happen is the *next* start, and the only
//! thing that survives to tell it what to do differently is the start-up marker
//! [`crate::logfile`] already writes (`features/logging.md` phase 6).
//!
//! That marker names the stage a dead start reached. Give it the renderer as
//! well and it names the *attempt*, which is everything this module needs:
//!
//! | The marker left behind | This start does |
//! |---|---|
//! | nothing | the remembered renderer, or the platform default |
//! | `stage=window renderer=automatic` | step down — try Direct3D 12 |
//! | `stage=window renderer=dx12` | step down — try OpenGL |
//! | `stage=window renderer=gl` | stay; the ladder is out of rungs and says so |
//! | any other stage | nothing; that start died of something else |
//!
//! ## Why the answer is remembered
//!
//! A ladder alone recovers exactly once. The marker is removed the moment a
//! window exists, so the *next* start would find no marker, go back to the
//! default, and fault again — an application that works every second launch.
//! The rung that produced a window is therefore written to the settings file
//! ([`remember_in`]), next to the window geometry, which is the other thing
//! remembered about how this workstation comes up.
//!
//! ## What is deliberately not here
//!
//! `$WGPU_BACKEND` is left alone rather than wrapped. It is wgpu's own variable,
//! `egui-wgpu`'s default configuration already honours it, and it is what a
//! support call reaches for first — so when it is set this module stands aside
//! entirely and records that it did. `$YKDM_RENDERER` is the same probe in this
//! tool's own vocabulary, and neither is remembered: an environment variable is
//! how somebody *tests* a workstation, and a test that silently became a
//! permanent setting is a fault nobody would think to look for.

use crate::settings::AppSettings;

/// A rung of the fallback ladder: which graphics backends a start lets wgpu
/// consider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Renderer {
    /// Whatever wgpu prefers on this platform, which is what every start did
    /// before this module existed. The top of every ladder, and the only rung on
    /// a workstation where nothing has ever failed.
    #[default]
    Automatic,
    /// Direct3D 12 only.
    Dx12,
    /// OpenGL only — the bottom of every ladder that has one, because it is the
    /// backend most likely to be there when the modern ones are broken.
    Gl,
}

impl Renderer {
    /// The stable name written to the settings file and the start-up marker.
    pub fn slug(&self) -> &'static str {
        match self {
            Renderer::Automatic => "automatic",
            Renderer::Dx12 => "dx12",
            Renderer::Gl => "gl",
        }
    }

    /// Words for a human: `--diagnose` prints this, and a support call reads it
    /// out loud.
    pub fn label(&self) -> &'static str {
        match self {
            Renderer::Automatic => "the platform default",
            Renderer::Dx12 => "Direct3D 12",
            Renderer::Gl => "OpenGL",
        }
    }

    /// Read a [`slug`](Renderer::slug), or the name an operator typed into
    /// `$YKDM_RENDERER`.
    ///
    /// Lenient about the spellings somebody would actually try — `d3d12`,
    /// `opengl`, `default` — because the alternative is a variable that silently
    /// does nothing on the one machine it was set to rescue.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "automatic" | "auto" | "default" => Some(Renderer::Automatic),
            "dx12" | "d3d12" | "directx" | "direct3d" => Some(Renderer::Dx12),
            "gl" | "gles" | "opengl" => Some(Renderer::Gl),
            _ => None,
        }
    }

    /// The backends to restrict wgpu to, or `None` to leave its own preference
    /// order — and therefore `$WGPU_BACKEND` — untouched.
    pub fn backends(&self) -> Option<eframe::wgpu::Backends> {
        match self {
            Renderer::Automatic => None,
            Renderer::Dx12 => Some(eframe::wgpu::Backends::DX12),
            Renderer::Gl => Some(eframe::wgpu::Backends::GL),
        }
    }
}

/// The rungs this platform has, in the order they are tried.
///
/// Per target because the alternatives are: Windows has two independent drivers
/// for the same GPU and the reported fault is exactly one of them being broken;
/// Linux has a GL path worth reaching for when Vulkan is not there; macOS has
/// Metal and nothing else, so there is no second rung to invent — a start that
/// dies there has not run out of backends, it has run out of graphics.
///
/// `cfg!` rather than `#[cfg]` deliberately. Nothing here is platform-specific —
/// `Backends::DX12` is a bitflag that exists everywhere — so every arm can be
/// compiled everywhere, and making it so means a `cargo check` on this
/// workstation type-checks the *Windows* ladder. Cross-compiling to prove it
/// instead is not available in this repository (bundled SQLite), which is
/// exactly why the arms should not need it.
pub fn ladder() -> &'static [Renderer] {
    if cfg!(windows) {
        &[Renderer::Automatic, Renderer::Dx12, Renderer::Gl]
    } else if cfg!(target_os = "linux") {
        &[Renderer::Automatic, Renderer::Gl]
    } else {
        &[Renderer::Automatic]
    }
}

/// The rung below `current` on `ladder`, if there is one.
///
/// A `current` that is not on this platform's ladder — a settings file copied
/// from a Windows workstation to a Mac — has no rung below it, which is the
/// honest answer rather than a guess at where it would have sat.
pub fn next_after(ladder: &[Renderer], current: Renderer) -> Option<Renderer> {
    let at = ladder.iter().position(|rung| *rung == current)?;
    ladder.get(at + 1).copied()
}

/// Why a start is using the renderer it is using.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// `$WGPU_BACKEND` is set, so this module stood aside and wgpu reads it.
    Environment,
    /// `$YKDM_RENDERER` named a rung outright.
    Chosen,
    /// The previous start died asking for a window, so this one stepped down.
    SteppedDown,
    /// The previous start died asking for a window and there is no rung below
    /// the one it used. Whatever is wrong is not the choice of backend.
    Exhausted,
    /// The rung that produced a window last time on this workstation.
    Remembered,
    /// Nothing has ever failed here.
    FirstTry,
}

/// The renderer a start will ask for, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub renderer: Renderer,
    pub reason: Reason,
    /// The rung the start that died had used, when that is what decided this.
    pub after: Option<Renderer>,
}

impl Decision {
    /// Whether producing a window with this renderer is worth writing down.
    ///
    /// False for both environment variables: they are how a workstation is
    /// *probed*, and a probe that turned itself into a permanent setting would
    /// outlive the person who typed it.
    pub fn worth_remembering(&self) -> bool {
        !matches!(self.reason, Reason::Environment | Reason::Chosen)
    }

    /// Whether this decision is a symptom rather than a routine choice.
    ///
    /// The log level follows from it, and the predicate lives here rather than
    /// at the call site because `main.rs` is outside the coverage gate by
    /// contract (AGENTS.md §4) and "which of these reasons is bad news" is a
    /// decision, not painting. Both stepping down and running out of rungs mean
    /// a start died in the graphics driver, and a support call needs to find
    /// that by filtering the log for warnings.
    pub fn worth_a_warning(&self) -> bool {
        matches!(self.reason, Reason::SteppedDown | Reason::Exhausted)
    }

    /// What to put in front of the operator, when there is anything to say.
    ///
    /// `None` for the ordinary cases, because a screen that warns about a healthy
    /// workstation teaches the operator to ignore it. The wording lives here
    /// rather than in the Settings card for the reason AGENTS.md §4 gives:
    /// *which* of these outcomes deserves a paragraph, and which paragraph, is a
    /// decision, and `src/ui/` is outside the coverage gate.
    ///
    /// Both cases are read in a window that exists, so both are describing a
    /// **previous** start's death rather than a present failure, and both say so.
    pub fn alert(&self) -> Option<&'static str> {
        match self.reason {
            Reason::SteppedDown => Some(
                "The previous start of this application died while asking the graphics \
                 driver for a window, so this one asked for a different graphics backend and \
                 got one. There is nothing for you to do about it: this workstation will go on \
                 using the backend named above, and the register, the keys and the audit trail \
                 are untouched by any of it. It is worth reporting once, because what it means \
                 is that a graphics driver on this machine takes the whole application down \
                 instead of returning an error — and the next driver update is the thing that \
                 fixes that, not this application.",
            ),
            Reason::Exhausted => Some(
                "Every graphics backend this platform has was tried, and a start died \
                 asking for a window on each of them — so whatever is wrong is not the choice \
                 of backend. Look at the graphics driver, at the session (a remote desktop with \
                 no graphics card of its own), or at a display that is no longer attached. This \
                 window exists, so what is recorded here is earlier starts, not this one. Send \
                 the log file and this report with the ticket.",
            ),
            Reason::Environment | Reason::Chosen | Reason::Remembered | Reason::FirstTry => None,
        }
    }

    /// One line for `--diagnose` and the log: the renderer and what chose it.
    pub fn describe(&self) -> String {
        let renderer = self.renderer.label();
        match self.reason {
            Reason::Environment => {
                format!("{renderer} (left to $WGPU_BACKEND, which is set)")
            }
            Reason::Chosen => format!("{renderer} (chosen by $YKDM_RENDERER)"),
            Reason::SteppedDown => format!(
                "{renderer} (stepped down: the previous start died asking for a window with {})",
                self.after.unwrap_or_default().label()
            ),
            Reason::Exhausted => format!(
                "{renderer} (ALARM: the previous start died asking for a window with {}, and this \
                 platform has no other backend to try — the fault is not the choice of renderer)",
                self.after.unwrap_or_default().label()
            ),
            Reason::Remembered => format!("{renderer} (remembered: it is what worked here)"),
            Reason::FirstTry => format!("{renderer} (nothing has failed here)"),
        }
    }
}

/// Decide against a ladder named outright.
///
/// The whole decision, and pure: every input is an argument, so each rung of
/// every platform's ladder is exercised by the tests on whichever platform runs
/// them. `previous_marker` is the marker a dead start left behind, verbatim.
pub fn decide_on(
    ladder: &[Renderer],
    wgpu_backend: Option<&str>,
    ykdm_renderer: Option<&str>,
    previous_marker: Option<&str>,
    remembered: Renderer,
) -> Decision {
    // wgpu's own variable wins, and wins over the ladder too: somebody who set
    // it is standing at the machine trying backends, and a fallback that
    // overrode them would be fighting the person diagnosing it.
    if wgpu_backend.is_some_and(|raw| !raw.trim().is_empty()) {
        return Decision {
            renderer: Renderer::Automatic,
            reason: Reason::Environment,
            after: None,
        };
    }
    if let Some(chosen) = ykdm_renderer
        .filter(|raw| !raw.trim().is_empty())
        .and_then(Renderer::from_name)
    {
        return Decision {
            renderer: chosen,
            reason: Reason::Chosen,
            after: None,
        };
    }

    // A start that died at any other stage says nothing about the renderer: it
    // got its window, or never got as far as asking for one.
    let died_asking_for_a_window = previous_marker.filter(|marker| {
        crate::logfile::field_of(marker, "stage") == Some(crate::logfile::stage::WINDOW)
    });

    if let Some(marker) = died_asking_for_a_window {
        // A marker from a build before this module has no renderer field, and
        // what that build did was the platform default.
        let attempted = crate::logfile::field_of(marker, "renderer")
            .and_then(Renderer::from_name)
            .unwrap_or_default();
        return match next_after(ladder, attempted) {
            Some(next) => Decision {
                renderer: next,
                reason: Reason::SteppedDown,
                after: Some(attempted),
            },
            None => Decision {
                renderer: attempted,
                reason: Reason::Exhausted,
                after: Some(attempted),
            },
        };
    }

    if remembered == Renderer::Automatic {
        Decision {
            renderer: Renderer::Automatic,
            reason: Reason::FirstTry,
            after: None,
        }
    } else {
        Decision {
            renderer: remembered,
            reason: Reason::Remembered,
            after: None,
        }
    }
}

/// [`decide_on`] for this platform's [`ladder`] and the real environment.
pub fn decide(previous_marker: Option<&str>, remembered: Renderer) -> Decision {
    let wgpu_backend = std::env::var("WGPU_BACKEND").ok();
    let ykdm_renderer = std::env::var("YKDM_RENDERER").ok();
    decide_on(
        ladder(),
        wgpu_backend.as_deref(),
        ykdm_renderer.as_deref(),
        previous_marker,
        remembered,
    )
}

/// Restrict `configuration` to `renderer`'s backends, reporting whether it did.
///
/// Only the backend list is touched. Everything else `eframe` put there — the
/// low-latency surface configuration, the device limits, the display handle it
/// fills in later — is left exactly as it was, because none of it is what
/// faulted and a configuration built from scratch here would silently drop it.
pub fn apply(configuration: &mut eframe::egui_wgpu::WgpuConfiguration, renderer: Renderer) -> bool {
    let Some(backends) = renderer.backends() else {
        return false;
    };
    match &mut configuration.wgpu_setup {
        eframe::egui_wgpu::WgpuSetup::CreateNew(create) => {
            create.instance_descriptor.backends = backends;
            true
        }
        // Nothing in this application hands `eframe` a device it made itself,
        // so there is no instance here to restrict.
        eframe::egui_wgpu::WgpuSetup::Existing(_) => false,
    }
}

/// Record the renderer that produced a window, reporting whether that changed
/// anything.
///
/// The caller saves; this decides. `Automatic` is written like any other rung —
/// a workstation that used to need Direct3D 12 and stopped needing it should
/// stop being told to use it.
pub fn remember_in(settings: &mut AppSettings, renderer: Renderer) -> bool {
    if settings.renderer == renderer {
        return false;
    }
    settings.renderer = renderer;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Windows ladder, named here so every test below exercises all three
    /// rungs whatever platform is running them.
    const THREE: &[Renderer] = &[Renderer::Automatic, Renderer::Dx12, Renderer::Gl];

    fn marker(stage: &str, renderer: Option<&str>) -> String {
        match renderer {
            Some(r) => format!("stage={stage} renderer={r} version=0.0.0-test commit=abc"),
            None => format!("stage={stage} version=0.0.0-test commit=abc"),
        }
    }

    #[test]
    fn a_workstation_where_nothing_has_failed_gets_the_platform_default() {
        let decision = decide_on(THREE, None, None, None, Renderer::Automatic);
        assert_eq!(decision.renderer, Renderer::Automatic);
        assert_eq!(decision.reason, Reason::FirstTry);
        assert!(decision.worth_remembering());
    }

    #[test]
    fn a_start_that_died_asking_for_a_window_steps_down_one_rung() {
        // Given the fault this feature exists for: the default renderer took the
        // process down inside the driver
        let dead = marker(crate::logfile::stage::WINDOW, Some("automatic"));
        // When the next start decides
        let decision = decide_on(THREE, None, None, Some(&dead), Renderer::Automatic);
        // Then it tries the next backend rather than the one that just died
        assert_eq!(decision.renderer, Renderer::Dx12);
        assert_eq!(decision.reason, Reason::SteppedDown);
        assert_eq!(decision.after, Some(Renderer::Automatic));
    }

    #[test]
    fn the_ladder_is_walked_one_rung_at_a_time_not_jumped_to_the_bottom() {
        let dead = marker(crate::logfile::stage::WINDOW, Some("dx12"));
        let decision = decide_on(THREE, None, None, Some(&dead), Renderer::Automatic);
        assert_eq!(decision.renderer, Renderer::Gl);
        assert_eq!(decision.reason, Reason::SteppedDown);
    }

    #[test]
    fn the_bottom_rung_dying_stops_stepping_and_says_the_fault_is_elsewhere() {
        // Given every backend tried and the process still dying
        let dead = marker(crate::logfile::stage::WINDOW, Some("gl"));
        // When the next start decides
        let decision = decide_on(THREE, None, None, Some(&dead), Renderer::Automatic);
        // Then it stays where it is rather than starting the ladder again, and
        // the reason it reports is that the renderer is not the problem
        assert_eq!(decision.renderer, Renderer::Gl);
        assert_eq!(decision.reason, Reason::Exhausted);
        assert!(decision.describe().contains("no other backend"));
    }

    #[test]
    fn a_marker_from_a_build_that_had_no_ladder_is_read_as_the_default_rung() {
        // The 0.19.2 marker: a stage and no renderer, because that build had no
        // choice to record. What it did was the platform default.
        let dead = marker(crate::logfile::stage::WINDOW, None);
        let decision = decide_on(THREE, None, None, Some(&dead), Renderer::Automatic);
        assert_eq!(decision.renderer, Renderer::Dx12);
        assert_eq!(decision.after, Some(Renderer::Automatic));
    }

    #[test]
    fn a_start_that_died_after_it_had_a_window_does_not_move_the_ladder() {
        // The register failing to open is not the graphics driver's fault, and
        // stepping down would hide the real failure behind a renderer change.
        let dead = marker(crate::logfile::stage::APP, Some("automatic"));
        let decision = decide_on(THREE, None, None, Some(&dead), Renderer::Dx12);
        assert_eq!(decision.renderer, Renderer::Dx12);
        assert_eq!(decision.reason, Reason::Remembered);
    }

    #[test]
    fn the_rung_that_worked_is_used_again_rather_than_rediscovered() {
        // The point of remembering: the marker is gone once a window exists, so
        // without this the next start would go back to the rung that faulted.
        let decision = decide_on(THREE, None, None, None, Renderer::Dx12);
        assert_eq!(decision.renderer, Renderer::Dx12);
        assert_eq!(decision.reason, Reason::Remembered);
    }

    #[test]
    fn wgpu_s_own_variable_wins_and_is_not_overridden_by_the_ladder() {
        // Somebody is at the machine trying backends by hand; a fallback that
        // argued with them would be worse than none.
        let dead = marker(crate::logfile::stage::WINDOW, Some("automatic"));
        let decision = decide_on(THREE, Some("dx12"), None, Some(&dead), Renderer::Gl);
        assert_eq!(decision.reason, Reason::Environment);
        assert_eq!(decision.renderer, Renderer::Automatic);
        // And wgpu must be left to read it: restricting the backends here would
        // overwrite the very thing that was set.
        assert_eq!(decision.renderer.backends(), None);
        assert!(!decision.worth_remembering());
    }

    #[test]
    fn an_empty_environment_variable_is_not_a_choice() {
        let decision = decide_on(THREE, Some("  "), Some(""), None, Renderer::Automatic);
        assert_eq!(decision.reason, Reason::FirstTry);
    }

    #[test]
    fn an_explicit_choice_overrides_the_remembered_rung_and_is_not_remembered() {
        let decision = decide_on(THREE, None, Some("opengl"), None, Renderer::Dx12);
        assert_eq!(decision.renderer, Renderer::Gl);
        assert_eq!(decision.reason, Reason::Chosen);
        assert!(!decision.worth_remembering());
    }

    #[test]
    fn an_unrecognised_choice_is_ignored_rather_than_failing_the_start() {
        let decision = decide_on(THREE, None, Some("banana"), None, Renderer::Automatic);
        assert_eq!(decision.reason, Reason::FirstTry);
    }

    #[test]
    fn every_spelling_an_operator_would_try_names_a_rung() {
        for (name, expected) in [
            ("automatic", Renderer::Automatic),
            ("auto", Renderer::Automatic),
            ("DEFAULT", Renderer::Automatic),
            ("dx12", Renderer::Dx12),
            ("d3d12", Renderer::Dx12),
            (" Direct3D ", Renderer::Dx12),
            ("gl", Renderer::Gl),
            ("gles", Renderer::Gl),
            ("OpenGL", Renderer::Gl),
        ] {
            assert_eq!(Renderer::from_name(name), Some(expected), "{name}");
        }
        assert_eq!(Renderer::from_name("vulkan"), None);
    }

    #[test]
    fn a_rung_this_platform_does_not_have_has_nothing_below_it() {
        // A settings file carried from a Windows workstation to a Mac.
        assert_eq!(next_after(&[Renderer::Automatic], Renderer::Dx12), None);
    }

    #[test]
    fn this_platform_s_ladder_starts_at_the_default_and_repeats_no_rung() {
        let rungs = ladder();
        assert_eq!(rungs.first(), Some(&Renderer::Automatic));
        for (at, rung) in rungs.iter().enumerate() {
            assert!(
                !rungs[at + 1..].contains(rung),
                "{} appears twice, so the ladder would loop",
                rung.slug()
            );
        }
    }

    #[test]
    fn every_rung_has_a_distinct_slug_and_reads_back_as_itself() {
        for rung in THREE {
            assert_eq!(Renderer::from_name(rung.slug()), Some(*rung));
            assert!(!rung.label().is_empty());
        }
    }

    #[test]
    fn every_reason_explains_itself_naming_the_renderer() {
        for reason in [
            Reason::Environment,
            Reason::Chosen,
            Reason::SteppedDown,
            Reason::Exhausted,
            Reason::Remembered,
            Reason::FirstTry,
        ] {
            let decision = Decision {
                renderer: Renderer::Dx12,
                reason,
                after: Some(Renderer::Automatic),
            };
            let described = decision.describe();
            assert!(
                described.contains(Renderer::Dx12.label()),
                "{reason:?} does not name the renderer: {described}"
            );
        }
    }

    #[test]
    fn only_a_start_that_died_in_the_driver_is_worth_a_warning() {
        let dead = marker(crate::logfile::stage::WINDOW, Some("automatic"));
        let bottom = marker(crate::logfile::stage::WINDOW, Some("gl"));
        for (decision, expected) in [
            (
                decide_on(THREE, None, None, Some(&dead), Renderer::Automatic),
                true,
            ),
            (
                decide_on(THREE, None, None, Some(&bottom), Renderer::Automatic),
                true,
            ),
            (
                decide_on(THREE, None, None, None, Renderer::Automatic),
                false,
            ),
            (decide_on(THREE, None, None, None, Renderer::Dx12), false),
            (
                decide_on(THREE, Some("gl"), None, None, Renderer::Automatic),
                false,
            ),
            (
                decide_on(THREE, None, Some("gl"), None, Renderer::Automatic),
                false,
            ),
        ] {
            assert_eq!(
                decision.worth_a_warning(),
                expected,
                "{:?} is logged at the wrong level",
                decision.reason
            );
        }
    }

    #[test]
    fn exactly_the_outcomes_worth_a_warning_have_something_to_say_on_screen() {
        // A screen that warns about a healthy workstation teaches the operator to
        // ignore it, so the two must agree: an alert exists for precisely the
        // reasons that are logged as warnings.
        for reason in [
            Reason::Environment,
            Reason::Chosen,
            Reason::SteppedDown,
            Reason::Exhausted,
            Reason::Remembered,
            Reason::FirstTry,
        ] {
            let decision = Decision {
                renderer: Renderer::Dx12,
                reason,
                after: Some(Renderer::Automatic),
            };
            assert_eq!(
                decision.alert().is_some(),
                decision.worth_a_warning(),
                "{reason:?} disagrees between the log and the screen"
            );
        }
    }

    #[test]
    fn an_alert_is_read_in_a_window_that_exists_so_it_says_which_start_it_means() {
        // Both alerts are painted by a running application, so neither may read
        // as though the failure were happening now.
        let dead = marker(crate::logfile::stage::WINDOW, Some("automatic"));
        let stepped = decide_on(THREE, None, None, Some(&dead), Renderer::Automatic);
        assert!(stepped.alert().unwrap().contains("previous start"));

        let bottom = marker(crate::logfile::stage::WINDOW, Some("gl"));
        let exhausted = decide_on(THREE, None, None, Some(&bottom), Renderer::Automatic);
        assert!(exhausted.alert().unwrap().contains("This window exists"));
    }

    #[test]
    fn restricting_the_backends_leaves_the_rest_of_the_configuration_alone() {
        // Given the configuration eframe hands out, which carries a surface
        // setting and device limits this module has no opinion about
        let mut configuration = eframe::NativeOptions::default().wgpu_options;
        let untouched = configuration.clone();
        // When a rung restricts it
        assert!(apply(&mut configuration, Renderer::Dx12));
        // Then only the backend list moved
        let (
            eframe::egui_wgpu::WgpuSetup::CreateNew(after),
            eframe::egui_wgpu::WgpuSetup::CreateNew(before),
        ) = (&configuration.wgpu_setup, &untouched.wgpu_setup)
        else {
            panic!("eframe's default configuration creates its own instance");
        };
        assert_eq!(
            after.instance_descriptor.backends,
            eframe::wgpu::Backends::DX12
        );
        assert_eq!(after.power_preference, before.power_preference);
        assert_eq!(configuration.surface, untouched.surface);
    }

    #[test]
    fn the_default_rung_restricts_nothing_so_wgpu_keeps_its_own_order() {
        let mut configuration = eframe::NativeOptions::default().wgpu_options;
        assert!(!apply(&mut configuration, Renderer::Automatic));
    }

    #[test]
    fn the_rung_that_worked_is_written_down_only_when_it_is_news() {
        let mut settings = AppSettings::default();
        assert_eq!(settings.renderer, Renderer::Automatic);
        assert!(remember_in(&mut settings, Renderer::Dx12));
        assert_eq!(settings.renderer, Renderer::Dx12);
        // Saving on every start would rewrite the settings file for nothing.
        assert!(!remember_in(&mut settings, Renderer::Dx12));
        // And a workstation that stops needing the fallback stops being told to
        // use it.
        assert!(remember_in(&mut settings, Renderer::Automatic));
    }
}
