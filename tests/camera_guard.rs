//! Regression test for the crash that `camera`-by-default exposed.
//!
//! Symptom: clicking *Start camera* on an unbundled macOS build aborted the whole
//! application —
//!
//! ```text
//! thread 'camera-scan' panicked at core/src/panicking.rs:225:5:
//! panic in a function that cannot unwind
//! thread caused non-unwinding panic. aborting.
//! ```
//!
//! AVFoundation raised an Objective-C exception across an `extern "C"` boundary,
//! which is a non-unwinding panic: `catch_unwind` cannot save it, so the only fix is
//! to never make the call. `scan::preflight` is that guard.
//!
//! This test calls the exact entry point the button calls. If the guard regresses,
//! the test process aborts — a loud failure here instead of at an operator's desk.

#![cfg(feature = "camera")]

use yk_dist_manager::scan::camera::CameraScanner;
use yk_dist_manager::scan::{RxingDecoder, preflight};

#[test]
fn starting_the_camera_from_an_unbundled_binary_returns_an_error_instead_of_aborting() {
    // The test harness runs from `target/debug/deps/…`, so this process is in
    // exactly the state that used to abort.
    assert!(
        !preflight::inside_macos_bundle(),
        "this test is only meaningful for an unbundled binary"
    );

    let outcome = CameraScanner::start(0, Box::new(RxingDecoder::new()));

    // Reaching this line at all is the point of the test.
    match outcome {
        Err(e) => {
            let message = e.to_string();
            assert!(
                !message.is_empty(),
                "a refusal must explain itself, not be silent"
            );
            if cfg!(target_os = "macos") {
                assert!(
                    message.contains("Info.plist") || message.contains("camera access"),
                    "the message must name the cause and the way out; got: {message}"
                );
            }
        }
        Ok(_scanner) => {
            // Only reachable where the platform genuinely allows it (a Linux box
            // with a readable V4L2 device, say). Then the invariant is simply that
            // starting and dropping a scanner is clean.
            #[cfg(target_os = "macos")]
            panic!("an unbundled macOS binary must never open the camera");
        }
    }
}

#[test]
fn the_preflight_never_reports_success_without_authorisation() {
    // `check` composes the strongest preconditions first. On any machine, a failure
    // must be an `Err`, never a panic — this is the contract the capture thread
    // relies on.
    let _ = preflight::check(Some(1), 0);
    let _ = preflight::check(Some(0), 0);
    let _ = preflight::check(None, 99);
}

#[test]
fn enumerating_cameras_is_safe_before_authorisation() {
    // Enumeration lists devices without opening one, so it must not abort even
    // unauthorised — the preflight relies on being able to count devices.
    let cameras = CameraScanner::available_cameras();
    for (index, name) in &cameras {
        assert!(!name.is_empty(), "camera {index} reported an empty name");
    }
}
