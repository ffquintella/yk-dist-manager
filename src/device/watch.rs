//! Watching for keys being plugged in and pulled out
//! (`features/device-detection.md` phase 2).
//!
//! egui repaints continuously, so detection cannot run per frame: reading a key
//! costs tens of milliseconds over PC/SC and around a second through the `ykman`
//! subprocess, and either would be paid on every one of sixty frames a second. So
//! a background thread does the looking and publishes what it found; the GUI reads
//! the latest snapshot, which is a mutex and a clone.
//!
//! ## What the thread does, and how little of it
//!
//! Every tick it calls [`YubiKeyBackend::list_serials`] — the cheap question. Only
//! when the **set of serials changes** does it ask the expensive one
//! ([`YubiKeyBackend::info`], per key), so a key sitting in a port costs one
//! enumeration per tick and nothing else. That is the design the spec asks for, and
//! it is also what keeps a `ykman`-backed build tolerable: one subprocess per tick
//! rather than one per key per tick.
//!
//! ## Three things it deliberately does not do
//!
//! * **It does not write to the register.** Plugging a key in fills the "attached"
//!   list and nothing else; recording it stays an operator's click. A tool that
//!   added inventory rows because somebody plugged something in would be making
//!   records nobody asked for, and `docs/security-and-compliance.md` is explicit
//!   that nothing mutates as a side effect of a screen being open.
//! * **It does not choose between keys.** With two attached it reports two, and the
//!   picker (phase 3) is how one gets chosen. Picking one at random and writing a
//!   PIN to it is the worst outcome this feature has.
//! * **It does not touch the hardware while a bootstrap is running.** The watch is
//!   stopped for the duration of a run — see [`crate::app::YkDistApp::execute_run`].
//!   Enumerating PC/SC readers while another handle holds an exclusive transaction
//!   is not something to find out about halfway through writing a PIN.
//!
//! ## Why it is not always running
//!
//! It runs while the operator is on a screen that shows attached keys, and stops
//! when they leave. In the default build the only read transport is the `ykman`
//! subprocess, so "always polling" would mean spawning a process every couple of
//! seconds for the whole life of the application — for a screen nobody is looking
//! at. See [`interval_for`] for the other half of that trade.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{DeviceError, DeviceInfo, YubiKeyBackend};

/// How often to look, with a transport that talks to the hardware directly.
///
/// The 1.5s the spec asks for: fast enough that plugging a key in feels immediate,
/// slow enough to be invisible next to a PC/SC enumeration.
pub const POLL_INTERVAL: Duration = Duration::from_millis(1_500);

/// How often to look when every poll is a **subprocess**.
///
/// `ykman list --serials` forks a Python process, which is three orders of
/// magnitude more expensive than a PC/SC enumeration. At 1.5s that is 40 processes
/// a minute for the whole time a screen is open; at 4s it is 15, and the operator
/// cannot tell the difference between the two while walking a key from a box to a
/// port. When the native transport is selectable
/// (`features/native-device-transport.md` phase 6) this stops applying to most
/// builds.
pub const POLL_INTERVAL_SUBPROCESS: Duration = Duration::from_millis(4_000);

/// The tick used to notice a stop request, so dropping the watch is prompt rather
/// than up to one interval long.
const STOP_CHECK: Duration = Duration::from_millis(50);

/// Which interval suits the transport in use.
pub fn interval_for(native_transport: bool) -> Duration {
    if native_transport {
        POLL_INTERVAL
    } else {
        POLL_INTERVAL_SUBPROCESS
    }
}

/// What the GUI reads between frames.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attached {
    /// Every key the last successful enumeration found, identified.
    ///
    /// Ordered by serial rather than by the order the transport happened to
    /// return: the picker is a list an operator reads, and a list that reshuffles
    /// itself between polls is one where they click the wrong row.
    pub keys: Vec<DeviceInfo>,
    /// Bumped whenever the set of attached serials changes.
    ///
    /// This is what the GUI compares against to notice a change without diffing
    /// the list itself — and what makes "the operator has already been told about
    /// this arrangement of keys" expressible.
    pub generation: u64,
    /// Serials seen by the last enumeration but *not* identified, with why.
    ///
    /// A key that enumerates and then refuses to describe itself is worth showing:
    /// it is usually a driver or permission problem, and reporting it as "no keys
    /// attached" would send the operator looking for the wrong thing.
    pub unreadable: Vec<(u32, String)>,
    /// The last failure, kept until a poll succeeds.
    pub last_error: Option<String>,
    /// Polls completed, so the UI can tell "watching, nothing there" from "not
    /// watching yet".
    pub polls: u64,
    /// Set when the thread has given up: the transport is missing, so looking
    /// again will not help.
    pub stopped: Option<String>,
}

impl Attached {
    /// The serials, in the order shown.
    pub fn serials(&self) -> Vec<u32> {
        self.keys.iter().map(|k| k.serial).collect()
    }

    /// Exactly one key attached and identified — the unambiguous case.
    pub fn only_key(&self) -> Option<&DeviceInfo> {
        match self.keys.as_slice() {
            [one] if self.unreadable.is_empty() => Some(one),
            _ => None,
        }
    }

    /// More than one thing attached, so nothing may be assumed.
    pub fn is_ambiguous(&self) -> bool {
        self.keys.len() + self.unreadable.len() > 1
    }

    /// One line for the status bar.
    pub fn describe(&self) -> String {
        if let Some(reason) = &self.stopped {
            return format!("not watching for keys: {reason}");
        }
        if self.polls == 0 {
            return "looking for keys…".into();
        }
        match (self.keys.len(), self.unreadable.len()) {
            (0, 0) => "no key attached".into(),
            (1, 0) => {
                let key = &self.keys[0];
                format!("{} {} attached", key.model, key.serial)
            }
            (n, 0) => format!("{n} keys attached — choose one"),
            (0, u) => format!("{u} device(s) attached that could not be read"),
            (n, u) => format!("{n} key(s) attached, {u} that could not be read"),
        }
    }
}

/// A running watch. Dropping it stops the thread.
pub struct DeviceWatch {
    state: Arc<Mutex<Attached>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    interval: Duration,
}

impl DeviceWatch {
    /// Start watching, with `backend` moved onto the thread.
    ///
    /// Moved rather than shared: [`YubiKeyBackend`] is `Send` but says nothing
    /// about `Sync`, and a backend the GUI could call at the same moment as the
    /// watch is a second handle on the same reader for no benefit. The GUI keeps
    /// its own for read-on-demand.
    pub fn start(backend: Box<dyn YubiKeyBackend>, interval: Duration) -> Self {
        let state = Arc::new(Mutex::new(Attached::default()));
        let stop = Arc::new(AtomicBool::new(false));

        let thread_state = Arc::clone(&state);
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("device-watch".into())
            .spawn(move || poll_loop(backend, interval, thread_state, thread_stop))
            .ok();

        if handle.is_none() {
            // A machine that cannot spawn a thread is in trouble for other
            // reasons, but the watch failing must not be silent — and read-on-demand
            // still works, which is what the message says.
            tracing::error!(
                event = "device.watch.spawn_failed",
                detail = "detection falls back to reading on demand"
            );
            state.lock().expect("watch state").stopped =
                Some("this workstation could not start the watch thread".into());
        }

        Self {
            state,
            stop,
            handle,
            interval,
        }
    }

    /// Snapshot of what is attached, cheap enough to call every frame.
    pub fn snapshot(&self) -> Attached {
        self.state.lock().expect("watch state").clone()
    }

    /// How often this watch looks.
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Has the thread given up?
    pub fn has_stopped(&self) -> bool {
        self.state.lock().expect("watch state").stopped.is_some()
    }
}

impl Drop for DeviceWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            // Joined rather than detached: the thread holds the only backend
            // handle, and a run that starts the moment the watch is dropped must
            // not race a poll still in flight. It checks the flag every 50ms.
            let _ = handle.join();
        }
    }
}

/// Look, publish, sleep — until asked to stop.
fn poll_loop(
    backend: Box<dyn YubiKeyBackend>,
    interval: Duration,
    state: Arc<Mutex<Attached>>,
    stop: Arc<AtomicBool>,
) {
    let mut known: Option<Vec<u32>> = None;

    while !stop.load(Ordering::Relaxed) {
        match backend.list_serials() {
            Ok(mut serials) => {
                serials.sort_unstable();
                serials.dedup();

                let changed = known.as_ref() != Some(&serials);
                if changed {
                    // The expensive question, asked only when the answer can have
                    // changed. Each key is identified explicitly by serial: `info(None)`
                    // means "the only attached key" and would refuse the very case
                    // the picker exists for.
                    let mut keys = Vec::new();
                    let mut unreadable = Vec::new();
                    for serial in &serials {
                        match backend.info(Some(*serial)) {
                            Ok(info) => keys.push(info),
                            Err(e) => unreadable.push((*serial, e.to_string())),
                        }
                    }

                    let mut guard = state.lock().expect("watch state");
                    guard.generation += 1;
                    guard.keys = keys;
                    guard.unreadable = unreadable;
                    guard.last_error = None;
                    guard.polls += 1;
                    known = Some(serials);
                } else {
                    let mut guard = state.lock().expect("watch state");
                    guard.last_error = None;
                    guard.polls += 1;
                }
            }
            Err(e) => {
                // A missing transport will not appear by being asked again, and
                // spawning a doomed subprocess every few seconds for the rest of
                // the session is worse than saying so once.
                let fatal = matches!(e, DeviceError::ToolMissing { .. });
                let mut guard = state.lock().expect("watch state");
                guard.polls += 1;
                guard.last_error = Some(e.to_string());
                if fatal {
                    guard.keys.clear();
                    guard.unreadable.clear();
                    guard.stopped = Some(e.to_string());
                    drop(guard);
                    tracing::warn!(event = "device.watch.stopped", reason = %e);
                    return;
                }
            }
        }

        // Sleep in slices, so dropping the watch is prompt.
        let mut slept = Duration::ZERO;
        while slept < interval && !stop.load(Ordering::Relaxed) {
            let slice = STOP_CHECK.min(interval - slept);
            std::thread::sleep(slice);
            slept += slice;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::MockBackend;

    fn device(serial: u32, model: &str) -> DeviceInfo {
        DeviceInfo {
            serial,
            model: model.into(),
            firmware: "5.7.4".into(),
            ..DeviceInfo::default()
        }
    }

    /// Wait for a snapshot to satisfy `done`, or fail the test.
    ///
    /// Polling the watch rather than sleeping a fixed time: the interval is short
    /// in these tests, and a fixed sleep is either flaky or slow.
    fn eventually(watch: &DeviceWatch, what: &str, done: impl Fn(&Attached) -> bool) -> Attached {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = watch.snapshot();
            if done(&snapshot) {
                return snapshot;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}; last snapshot: {snapshot:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_key_plugged_in_appears_and_one_pulled_out_disappears() {
        // The whole point of the phase: the operator does not press anything.
        let backend = Arc::new(MockBackend::new(Vec::new()));
        let watch = DeviceWatch::start(
            Box::new(SharedMock(Arc::clone(&backend))),
            Duration::from_millis(10),
        );

        let empty = eventually(&watch, "the first poll", |a| a.polls > 0);
        assert!(empty.keys.is_empty());
        assert_eq!(empty.describe(), "no key attached");

        backend.set_devices(vec![device(20_423_633, "YubiKey 5 NFC")]);
        let one = eventually(&watch, "a key to appear", |a| a.keys.len() == 1);
        assert_eq!(one.serials(), vec![20_423_633]);
        assert_eq!(
            one.only_key().map(|k| k.model.as_str()),
            Some("YubiKey 5 NFC")
        );
        assert!(one.describe().contains("20423633"), "{}", one.describe());

        backend.set_devices(Vec::new());
        let gone = eventually(&watch, "the key to be pulled out", |a| a.keys.is_empty());
        assert!(gone.only_key().is_none());
        assert!(gone.generation >= 2, "each change bumps it: {gone:?}");
    }

    #[test]
    fn identification_runs_only_when_the_set_of_serials_changes() {
        // The cost control. A key sitting in a port must not be re-identified sixty
        // times a second, nor once per poll — with the subprocess transport that is
        // a fork per key per poll.
        let backend = Arc::new(CountingMock::new(vec![device(1, "A")]));
        let watch = DeviceWatch::start(
            Box::new(CountingHandle(Arc::clone(&backend))),
            Duration::from_millis(5),
        );

        eventually(&watch, "several polls of a steady key", |a| a.polls >= 5);
        let (lists, infos) = backend.counts();
        assert!(
            lists >= 5,
            "the cheap question is asked every tick: {lists}"
        );
        assert_eq!(infos, 1, "the expensive one only on change, got {infos}");

        // And a change asks again — once.
        backend.set(vec![device(1, "A"), device(2, "B")]);
        eventually(&watch, "the second key", |a| a.keys.len() == 2);
        let (_, infos) = backend.counts();
        assert_eq!(infos, 3, "one for the first key, two for the new pair");
    }

    #[test]
    fn two_keys_are_reported_as_two_and_nothing_is_chosen() {
        // Never auto-pick: writing a PIN to whichever key the transport happened to
        // list first is the worst outcome this feature has.
        let backend = Arc::new(MockBackend::new(vec![
            device(2, "YubiKey 5C"),
            device(1, "YubiKey 5 NFC"),
        ]));
        let watch = DeviceWatch::start(
            Box::new(SharedMock(Arc::clone(&backend))),
            Duration::from_millis(10),
        );

        let both = eventually(&watch, "both keys", |a| a.keys.len() == 2);
        assert!(both.is_ambiguous());
        assert_eq!(
            both.only_key(),
            None,
            "two attached means no obvious choice"
        );
        // Sorted by serial, not by whatever order the transport returned: a list
        // that reshuffles between polls is one where the operator clicks the wrong row.
        assert_eq!(both.serials(), vec![1, 2]);
        assert!(
            both.describe().contains("choose one"),
            "{}",
            both.describe()
        );
    }

    #[test]
    fn a_key_that_enumerates_but_cannot_be_read_is_reported_as_itself() {
        // Reporting this as "no keys attached" would send the operator looking for a
        // cable when the answer is a driver or a permission.
        let watch = DeviceWatch::start(Box::new(HalfBroken), Duration::from_millis(10));

        let snapshot = eventually(&watch, "the unreadable key", |a| !a.unreadable.is_empty());
        assert!(snapshot.keys.is_empty());
        assert_eq!(snapshot.unreadable[0].0, 42);
        assert!(!snapshot.is_ambiguous(), "one device, one problem");
        assert!(
            snapshot.describe().contains("could not be read"),
            "{}",
            snapshot.describe()
        );
    }

    #[test]
    fn a_transient_failure_keeps_the_watch_running_and_shows_the_reason() {
        let backend = Arc::new(MockBackend::failing("the reader is busy"));
        let watch = DeviceWatch::start(
            Box::new(SharedMock(Arc::clone(&backend))),
            Duration::from_millis(5),
        );

        let snapshot = eventually(&watch, "a failing poll", |a| a.last_error.is_some());
        assert!(
            snapshot
                .last_error
                .as_ref()
                .unwrap()
                .contains("the reader is busy"),
            "{snapshot:?}"
        );
        assert!(snapshot.stopped.is_none(), "a busy reader may free up");
        eventually(&watch, "it to keep trying", |a| a.polls >= 3);
    }

    #[test]
    fn a_missing_transport_stops_the_watch_instead_of_forking_for_ever() {
        // The failure this guards: `ykman` is not installed, and the watch spawns a
        // doomed subprocess every few seconds for the rest of the session.
        let watch = DeviceWatch::start(Box::new(MissingTool), Duration::from_millis(5));

        let snapshot = eventually(&watch, "the watch to give up", |a| a.stopped.is_some());
        assert!(
            snapshot.stopped.as_ref().unwrap().contains("ykman"),
            "the reason has to name what is missing: {snapshot:?}"
        );
        assert!(watch.has_stopped());
        assert!(
            snapshot.describe().starts_with("not watching"),
            "{}",
            snapshot.describe()
        );

        // And it really stopped: the poll count does not move.
        let polls = watch.snapshot().polls;
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(watch.snapshot().polls, polls);
    }

    #[test]
    fn dropping_the_watch_stops_the_thread_promptly() {
        // Load-bearing: the watch is dropped to get the hardware out of the way
        // before a run writes to a key, and a poll still in flight afterwards is
        // exactly what that is meant to prevent.
        let backend = Arc::new(CountingMock::new(vec![device(1, "A")]));
        let watch = DeviceWatch::start(
            Box::new(CountingHandle(Arc::clone(&backend))),
            // Longer than the test is willing to wait: dropping must not block for
            // an interval.
            Duration::from_secs(30),
        );
        eventually(&watch, "the first poll", |a| a.polls > 0);

        let start = std::time::Instant::now();
        drop(watch);
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "dropping took {:?} — it must not wait out the interval",
            start.elapsed()
        );

        let (lists, _) = backend.counts();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(backend.counts().0, lists, "nothing polls after the drop");
    }

    #[test]
    fn the_interval_is_slower_when_every_poll_is_a_subprocess() {
        assert_eq!(interval_for(true), POLL_INTERVAL);
        assert_eq!(interval_for(false), POLL_INTERVAL_SUBPROCESS);
        assert!(
            interval_for(false) > interval_for(true),
            "a fork costs more than a PC/SC enumeration"
        );
    }

    // ---------------------------------------------------------------- helpers
    //
    // The watch takes ownership of its backend, and these tests need to keep
    // poking the same one from the outside, so each handle is a thin `Arc`
    // forwarder.

    struct SharedMock(Arc<MockBackend>);

    impl YubiKeyBackend for SharedMock {
        fn list_serials(&self) -> crate::device::Result<Vec<u32>> {
            self.0.list_serials()
        }
        fn info(&self, serial: Option<u32>) -> crate::device::Result<DeviceInfo> {
            self.0.info(serial)
        }
        fn describe(&self) -> String {
            self.0.describe()
        }
    }

    /// Counts what was asked, which is how the "only on change" rule is tested.
    struct CountingMock {
        devices: Mutex<Vec<DeviceInfo>>,
        lists: Mutex<usize>,
        infos: Mutex<usize>,
    }

    impl CountingMock {
        fn new(devices: Vec<DeviceInfo>) -> Self {
            Self {
                devices: Mutex::new(devices),
                lists: Mutex::new(0),
                infos: Mutex::new(0),
            }
        }
        fn set(&self, devices: Vec<DeviceInfo>) {
            *self.devices.lock().unwrap() = devices;
        }
        fn counts(&self) -> (usize, usize) {
            (*self.lists.lock().unwrap(), *self.infos.lock().unwrap())
        }
    }

    struct CountingHandle(Arc<CountingMock>);

    impl YubiKeyBackend for CountingHandle {
        fn list_serials(&self) -> crate::device::Result<Vec<u32>> {
            *self.0.lists.lock().unwrap() += 1;
            Ok(self
                .0
                .devices
                .lock()
                .unwrap()
                .iter()
                .map(|d| d.serial)
                .collect())
        }
        fn info(&self, serial: Option<u32>) -> crate::device::Result<DeviceInfo> {
            *self.0.infos.lock().unwrap() += 1;
            let devices = self.0.devices.lock().unwrap();
            match serial {
                Some(wanted) => devices
                    .iter()
                    .find(|d| d.serial == wanted)
                    .cloned()
                    .ok_or(DeviceError::NoDevice),
                None => devices.first().cloned().ok_or(DeviceError::NoDevice),
            }
        }
        fn describe(&self) -> String {
            "counting mock".into()
        }
    }

    /// Enumerates a key that then refuses to describe itself.
    struct HalfBroken;

    impl YubiKeyBackend for HalfBroken {
        fn list_serials(&self) -> crate::device::Result<Vec<u32>> {
            Ok(vec![42])
        }
        fn info(&self, _serial: Option<u32>) -> crate::device::Result<DeviceInfo> {
            Err(DeviceError::Command {
                command: "info".into(),
                message: "the applet did not answer".into(),
            })
        }
        fn describe(&self) -> String {
            "half-broken mock".into()
        }
    }

    struct MissingTool;

    impl YubiKeyBackend for MissingTool {
        fn list_serials(&self) -> crate::device::Result<Vec<u32>> {
            Err(DeviceError::ToolMissing {
                binary: "ykman".into(),
            })
        }
        fn info(&self, _serial: Option<u32>) -> crate::device::Result<DeviceInfo> {
            Err(DeviceError::ToolMissing {
                binary: "ykman".into(),
            })
        }
        fn describe(&self) -> String {
            "no transport".into()
        }
    }
}
