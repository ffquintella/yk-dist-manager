//! Live camera capture, feeding frames to the barcode decoder.
//!
//! Behind the `camera` feature, because it links a platform capture backend
//! (AVFoundation on macOS, Media Foundation on Windows, V4L2 on Linux).
//!
//! The capture runs on its own thread and publishes the **latest** frame plus any
//! successful scan through a mutex. The GUI polls that in its paint pass — it
//! never blocks on a camera, and a slow or wedged camera degrades to a stale
//! preview rather than a frozen application.
//!
//! Platform notes worth knowing before this is packaged:
//!
//! * macOS requires camera permission. A bundled `.app` needs
//!   `NSCameraUsageDescription` in its `Info.plist`; a bare `cargo run` binary is
//!   prompted (or silently denied) depending on how it was launched.
//! * Linux needs the user in the `video` group, or a matching udev rule.
//! * A laptop's built-in camera is usually fixed-focus and poor at close range.
//!   Getting a 1D barcode to decode often means holding the label ~20cm away and
//!   filling the frame width — which is exactly why the USB barcode wedge remains
//!   the recommended option for a receiving desk.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use nokhwa::Camera;
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};

use super::{BarcodeDecoder, LumaFrame, ScanError};

/// What the GUI reads between frames.
#[derive(Debug, Default)]
pub struct ScanState {
    /// Latest preview frame as packed RGB, with its dimensions.
    pub preview: Option<(u32, u32, Vec<u8>)>,
    /// The serial, once a barcode decoded to a plausible one.
    pub serial: Option<u32>,
    /// Last decode failure, for the operator ("hold it steadier", "wrong label").
    pub last_error: Option<String>,
    /// Frames captured, so the UI can tell "no camera" from "no barcode yet".
    pub frames: u64,
}

/// A running capture session. Dropping it stops the thread.
pub struct CameraScanner {
    state: Arc<Mutex<ScanState>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    description: String,
}

impl CameraScanner {
    /// Open the camera and start scanning.
    ///
    /// The camera is opened **on the capture thread**, not here: `nokhwa`'s
    /// `Camera` is not `Send`, so it must never cross a thread boundary. The
    /// thread reports the outcome of opening back over a channel, which is what
    /// this call waits for — bounded, so a wedged capture backend cannot hang the
    /// GUI.
    ///
    /// `decoder` is boxed rather than generic so the GUI can hold a
    /// `CameraScanner` without naming the decoder type.
    pub fn start(index: u32, decoder: Box<dyn BarcodeDecoder + Send>) -> Result<Self, ScanError> {
        // Nothing may touch the capture backend until these pass: on macOS an
        // unauthorised or unbundled attempt aborts the process instead of
        // returning an error. See `scan::preflight`.
        super::preflight::initialise();
        let cameras = Self::available_cameras();
        super::preflight::check(Some(cameras.len()), index)?;

        let state = Arc::new(Mutex::new(ScanState::default()));
        let stop = Arc::new(AtomicBool::new(false));

        let thread_state = Arc::clone(&state);
        let thread_stop = Arc::clone(&stop);
        let (announce, opened) = std::sync::mpsc::channel::<Result<String, ScanError>>();

        let handle = std::thread::Builder::new()
            .name("camera-scan".into())
            .spawn(move || {
                let camera = match open_camera(index) {
                    Ok(camera) => {
                        let _ = announce.send(Ok(camera.info().human_name()));
                        camera
                    }
                    Err(e) => {
                        let _ = announce.send(Err(e));
                        return;
                    }
                };
                capture_loop(camera, decoder, thread_state, thread_stop);
            })
            .map_err(|e| ScanError::Camera(e.to_string()))?;

        match opened.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(Ok(description)) => Ok(Self {
                state,
                stop,
                handle: Some(handle),
                description,
            }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                Err(ScanError::Camera(
                    "the camera did not respond within 10s — is another application using it, \
                     or is camera permission denied?"
                        .into(),
                ))
            }
        }
    }

    /// Snapshot of the current state, cheap enough to call every frame.
    pub fn snapshot(&self) -> ScanState {
        let guard = self.state.lock().expect("scan state");
        ScanState {
            preview: guard.preview.clone(),
            serial: guard.serial,
            last_error: guard.last_error.clone(),
            frames: guard.frames,
        }
    }

    /// The serial found so far, if any.
    pub fn serial(&self) -> Option<u32> {
        self.state.lock().expect("scan state").serial
    }

    /// Clear a found serial so scanning continues with the next label.
    pub fn clear_serial(&self) {
        let mut guard = self.state.lock().expect("scan state");
        guard.serial = None;
        guard.last_error = None;
    }

    pub fn describe(&self) -> &str {
        &self.description
    }

    /// Cameras available for capture.
    ///
    /// Enumeration is safe to call before authorisation: it lists devices without
    /// opening one. An error here means the backend itself is unavailable, which is
    /// reported as "no cameras" rather than propagated — the caller's preflight
    /// turns that into an actionable message.
    pub fn available_cameras() -> Vec<(u32, String)> {
        match nokhwa::query(nokhwa::utils::ApiBackend::Auto) {
            Ok(cameras) => cameras
                .into_iter()
                .enumerate()
                .map(|(position, info)| (position as u32, info.human_name()))
                .collect(),
            Err(e) => {
                tracing::warn!(event = "camera.query.failed", reason = %e);
                Vec::new()
            }
        }
    }
}

impl Drop for CameraScanner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            // The loop checks the flag every frame, so this is a short wait.
            let _ = handle.join();
        }
    }
}

/// Open a camera and start its stream. Runs on the capture thread.
///
/// Two format requests are tried: highest frame rate (best for aiming at a barcode
/// by hand), then whatever the device offers. A camera that matches no format at all
/// is a real error, not a reason to guess.
fn open_camera(index: u32) -> Result<Camera, ScanError> {
    let attempts = [
        RequestedFormatType::AbsoluteHighestFrameRate,
        RequestedFormatType::AbsoluteHighestResolution,
        RequestedFormatType::None,
    ];

    let mut last: Option<String> = None;
    for requested in attempts {
        match Camera::new(
            CameraIndex::Index(index),
            RequestedFormat::new::<RgbFormat>(requested),
        ) {
            Ok(mut camera) => {
                return match camera.open_stream() {
                    Ok(()) => Ok(camera),
                    Err(e) => Err(ScanError::Camera(format!(
                        "could not start the stream: {e}"
                    ))),
                };
            }
            Err(e) => {
                tracing::warn!(
                    event = "camera.format.rejected",
                    requested = format!("{requested:?}"),
                    reason = %e
                );
                last = Some(e.to_string());
            }
        }
    }

    Err(ScanError::Camera(last.unwrap_or_else(|| {
        "the camera offered no usable format".into()
    })))
}

fn capture_loop(
    mut camera: Camera,
    decoder: Box<dyn BarcodeDecoder + Send>,
    state: Arc<Mutex<ScanState>>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Relaxed) {
        let frame = match camera.frame() {
            Ok(frame) => frame,
            Err(e) => {
                record_error(&state, format!("capture failed: {e}"));
                break;
            }
        };

        let decoded = match frame.decode_image::<RgbFormat>() {
            Ok(buffer) => buffer,
            Err(e) => {
                record_error(&state, format!("frame decode failed: {e}"));
                continue;
            }
        };

        let (width, height) = (decoded.width(), decoded.height());
        let rgb = decoded.into_raw();

        let luma = LumaFrame::from_rgb(width, height, &rgb);

        // Publish the preview even when the decode finds nothing, so the operator
        // can see what the camera sees and aim.
        {
            let mut guard = state.lock().expect("scan state");
            guard.preview = Some((width, height, rgb));
            guard.frames += 1;
        }

        let Some(luma) = luma else { continue };

        match super::scan_frame(decoder.as_ref(), &luma) {
            Ok(serial) => {
                let mut guard = state.lock().expect("scan state");
                guard.serial = Some(serial);
                guard.last_error = None;
                // Stop decoding until the operator accepts or clears it, so the
                // number cannot change under them while they read it.
                drop(guard);
                while !stop.load(Ordering::Relaxed) {
                    if state.lock().expect("scan state").serial.is_none() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(80));
                }
            }
            Err(ScanError::NoBarcode) => {
                // The common case while aiming; not worth reporting.
            }
            Err(e) => record_error(&state, e.to_string()),
        }
    }

    let _ = camera.stop_stream();
}

fn record_error(state: &Arc<Mutex<ScanState>>, message: String) {
    tracing::warn!(event = "camera.scan.problem", reason = message.as_str());
    state.lock().expect("scan state").last_error = Some(message);
}
