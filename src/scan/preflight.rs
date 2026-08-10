//! Checks that must pass **before** any AVFoundation / V4L2 / MSMF call.
//!
//! Why this module exists: on macOS, touching the camera without the right
//! preconditions does not return an error. AVFoundation raises an Objective-C
//! exception, which crosses an `extern "C"` boundary and becomes
//! `panic in a function that cannot unwind` — a **process abort**. There is no
//! `catch_unwind` for that. The only defence is to not make the call.
//!
//! So every precondition is checked first, and a failure is a typed error the
//! operator can act on:
//!
//! | Condition | Consequence if skipped |
//! |---|---|
//! | `nokhwa_initialize` called (macOS) | nokhwa's own docs: "your responsibility … before anything else" |
//! | Camera authorisation granted | AVFoundation exception → abort |
//! | Running from a bundle with `NSCameraUsageDescription` | TCC has nothing to attribute the request to → abort |
//! | The requested camera index exists | opening a missing device → abort |

use super::ScanError;

/// Set once `nokhwa_initialize` has been called, so it happens exactly once and
/// only from the main thread.
static INITIALISED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Escape hatch for an operator who has arranged camera access another way (a
/// signed binary with an embedded plist, or `tccutil` entries). Documented as
/// "may abort", because that is the honest description.
pub const OVERRIDE_ENV: &str = "YKDM_ALLOW_UNBUNDLED_CAMERA";

/// Request camera access. **Must be called from the main thread**, early, before
/// any capture attempt. Cheap and idempotent; a no-op off macOS.
///
/// On macOS this triggers the system permission prompt, which is why it belongs in
/// `main` rather than behind a button: the prompt has to appear while the operator
/// is looking at the application.
pub fn initialise() {
    use std::sync::atomic::Ordering;

    if INITIALISED.swap(true, Ordering::SeqCst) {
        return;
    }

    #[cfg(feature = "camera")]
    nokhwa::nokhwa_initialize(|granted| {
        if granted {
            tracing::info!(event = "camera.authorised");
        } else {
            tracing::warn!(event = "camera.authorisation.denied");
        }
    });
}

/// Whether the platform reports camera access as granted.
pub fn authorised() -> bool {
    #[cfg(feature = "camera")]
    {
        nokhwa::nokhwa_check()
    }
    #[cfg(not(feature = "camera"))]
    {
        false
    }
}

/// True when the running executable is inside a macOS `.app` bundle.
///
/// A bare binary — anything launched by `cargo run`, or from a terminal — has no
/// `Info.plist`, so macOS has no `NSCameraUsageDescription` to show and no bundle
/// identity to attribute the grant to. Asking for the camera in that state is what
/// aborts the process.
pub fn inside_macos_bundle() -> bool {
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let path = executable.to_string_lossy();
    path.contains(".app/Contents/MacOS/")
}

/// Parse the override value. Pure, so it is testable without touching the
/// process environment — which matters because tests run in parallel in one
/// process and a shared env var is a race, not a fixture.
pub fn parse_override(raw: Option<&str>) -> bool {
    matches!(
        raw.map(|value| value.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1" | "true" | "yes")
    )
}

fn override_enabled() -> bool {
    parse_override(std::env::var(OVERRIDE_ENV).ok().as_deref())
}

/// Is the *platform* in a state where a camera call is safe to attempt?
///
/// `allow_unbundled` is passed in rather than read from the environment so this
/// predicate is pure and directly testable.
pub fn check_platform(allow_unbundled: bool) -> Result<(), ScanError> {
    if !cfg!(feature = "camera") {
        return Err(ScanError::Camera(
            "this build has no camera support — rebuild with `--features camera`".into(),
        ));
    }

    if cfg!(target_os = "macos") && !inside_macos_bundle() && !allow_unbundled {
        return Err(ScanError::Camera(format!(
            "macOS will not grant camera access to a bare binary: there is no Info.plist, so \
             nothing declares NSCameraUsageDescription, and attempting the capture aborts the \
             process instead of failing. Use the bundled application, or scan with a USB barcode \
             reader — it types into the serial field and needs no camera. To attempt it anyway \
             (it may abort), set {OVERRIDE_ENV}=1."
        )));
    }

    Ok(())
}

/// Does the requested device exist?
pub fn check_device(camera_count: Option<usize>, index: u32) -> Result<(), ScanError> {
    match camera_count {
        Some(0) => Err(ScanError::Camera(
            "no camera was found. A USB barcode reader is the alternative: it types into the \
             serial field."
                .into(),
        )),
        Some(count) if index as usize >= count => Err(ScanError::Camera(format!(
            "camera {index} does not exist ({count} found)"
        ))),
        _ => Ok(()),
    }
}

/// Run every precondition, strongest first. `Ok(())` means a capture attempt is
/// safe to make.
///
/// The error text is written for the person at the desk, not for a stack trace:
/// each one says what to do next.
pub fn check(camera_count: Option<usize>, index: u32) -> Result<(), ScanError> {
    check_platform(override_enabled())?;

    if !authorised() {
        return Err(ScanError::Camera(
            "camera access has not been granted. Approve the prompt, or allow this application \
             under System Settings → Privacy & Security → Camera, then try again."
                .into(),
        ));
    }

    check_device(camera_count, index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_binary_is_not_a_bundle() {
        // The test harness runs from `target/debug/deps/…`, so this is also a
        // check that the bundle detection is not accidentally always true.
        assert!(!inside_macos_bundle());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn an_unbundled_macos_binary_is_refused_before_touching_avfoundation() {
        // This is the case that aborted the process. It must be a refusal with an
        // actionable message, and it must not depend on a camera being present or on
        // authorisation having been granted.
        let err = check_platform(false).expect_err("must refuse");
        let message = err.to_string();
        assert!(message.contains("Info.plist"), "got: {message}");
        assert!(
            message.contains("USB barcode reader"),
            "the message must offer the alternative: {message}"
        );
        assert!(message.contains(OVERRIDE_ENV), "and the escape hatch");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_override_lets_an_unbundled_binary_through() {
        // Deliberately available, and deliberately documented as "may abort".
        assert!(check_platform(true).is_ok());
    }

    #[test]
    fn a_missing_camera_is_reported_rather_than_opened() {
        let err = check_device(Some(0), 0).expect_err("must refuse");
        assert!(
            err.to_string().contains("no camera was found"),
            "got: {err}"
        );
    }

    #[test]
    fn an_out_of_range_index_is_reported() {
        let err = check_device(Some(1), 3).expect_err("must refuse");
        let message = err.to_string();
        assert!(
            message.contains("camera 3 does not exist"),
            "got: {message}"
        );
        assert!(message.contains("1 found"), "got: {message}");
    }

    #[test]
    fn an_existing_camera_passes_the_device_check() {
        assert!(check_device(Some(2), 0).is_ok());
        assert!(check_device(Some(2), 1).is_ok());
        assert!(
            check_device(None, 0).is_ok(),
            "unknown count is not a refusal"
        );
    }

    #[test]
    fn the_override_accepts_the_usual_spellings() {
        for value in ["1", "true", "TRUE", " yes "] {
            assert!(
                parse_override(Some(value)),
                "`{value}` should enable the override"
            );
        }
        for value in ["0", "no", "", "maybe"] {
            assert!(!parse_override(Some(value)), "`{value}` should not");
        }
        assert!(!parse_override(None), "unset means off");
    }

    #[test]
    fn the_full_check_refuses_on_this_machine_without_aborting() {
        // Whatever this machine's state, `check` must return an error rather than
        // reach the backend — that is the whole point of the module.
        let outcome = check(Some(1), 0);
        if cfg!(target_os = "macos") {
            assert!(outcome.is_err(), "an unbundled test binary must be refused");
        }
    }

    #[test]
    fn initialise_is_idempotent() {
        // Called from `main`, and again defensively before a capture attempt.
        initialise();
        initialise();
    }
}
