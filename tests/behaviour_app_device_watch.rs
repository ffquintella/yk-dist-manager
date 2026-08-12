//! Behaviour test for the application's half of hot-plug detection
//! (`features/device-detection.md` phases 2 and 3): the watch runs on the screens
//! that need it and nowhere else, a key arriving fills the field, two keys are
//! refused until one is chosen, and nothing polls the hardware while a run writes
//! to a key.
//!
//! The watch thread itself is unit-tested in `src/device/watch.rs`. What is tested
//! here is the wiring, which is where the interesting mistakes are: *when* it runs,
//! *what* it is allowed to decide on the operator's behalf, and whether the
//! interlock around a run actually holds.
//!
//! **One test in this file, deliberately** — it drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared by
//! every test in a binary.

use std::time::Duration;

use yk_dist_manager::YkDistApp;
use yk_dist_manager::app::Tab;
use yk_dist_manager::device::{DeviceInfo, DeviceWatch, MockBackend, watch};

fn device(serial: u32, model: &str) -> DeviceInfo {
    DeviceInfo {
        serial,
        model: model.into(),
        firmware: "5.7.4".into(),
        ..DeviceInfo::default()
    }
}

/// Drive the app's per-frame watch handling until `done`, or fail.
///
/// The app normally does this from its paint pass; a test cannot paint, so it calls
/// the same two methods the frame loop calls.
fn until(app: &mut YkDistApp, what: &str, done: impl Fn(&YkDistApp) -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_device_watch();
        if done(app) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}; attached: {:?}",
            app.attached
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn scenario_keys_are_noticed_as_they_are_plugged_in_and_never_chosen_for_the_operator() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
    }

    let mut app = YkDistApp::new(Some(home.path().join("keys.sqlite3")));
    assert!(app.store.is_some(), "{:?}", app.db_form.error);

    // The watch runs on the screens that show attached keys, and nowhere else: in
    // the default build every poll is a `ykman` subprocess, and paying that while
    // somebody reads the audit trail buys nothing.
    app.tab = Tab::Audit;
    app.sync_device_watch();
    assert!(app.watch.is_none(), "no watch on the audit screen");

    app.tab = Tab::Inventory;
    app.sync_device_watch();
    assert!(app.watch.is_some(), "the inventory screen watches");
    let again = app.watch.is_some();
    app.sync_device_watch();
    assert_eq!(
        app.watch.is_some(),
        again,
        "syncing every frame must not restart the thread every frame"
    );

    // Swap in a watch driven by a mock, so the test can plug keys in and out. The
    // real one is started by `sync_device_watch`; this is the same type with a
    // backend the test controls.
    let hardware = std::sync::Arc::new(MockBackend::new(Vec::new()));
    app.watch = Some(DeviceWatch::start(
        Box::new(Shared(std::sync::Arc::clone(&hardware))),
        Duration::from_millis(10),
    ));

    // Given nothing attached
    until(&mut app, "the first poll", |app| app.attached.polls > 0);
    assert!(app.target_serial().is_none());
    assert_eq!(app.attached.describe(), "no key attached");

    // When one key is plugged in
    hardware.set_devices(vec![device(20_423_633, "YubiKey 5 NFC")]);
    until(&mut app, "the key to appear", |app| {
        app.attached.keys.len() == 1
    });

    // Then it is the target and the wizard's serial field is filled — a field, and
    // nothing else: plugging a key in must not add an inventory row nobody asked
    // for
    assert_eq!(app.target_serial(), Some(20_423_633));
    assert_eq!(app.wizard.serial, "20423633");
    assert!(
        app.keys.is_empty(),
        "detection must not record anything by itself"
    );
    assert!(
        !recorded(&app).contains(&"key.added".to_owned()),
        "nothing is added to the register because something was plugged in"
    );

    // When a second key is plugged in
    hardware.set_devices(vec![
        device(20_423_633, "YubiKey 5 NFC"),
        device(31_000_001, "YubiKey 5C"),
    ]);
    until(&mut app, "the second key", |app| {
        app.attached.keys.len() == 2
    });

    // Then the arrangement is ambiguous, and the previous selection stands rather
    // than being silently dropped — the operator chose that key while it was the
    // only one, and it is still attached
    assert!(app.attached.is_ambiguous());
    assert_eq!(app.target_serial(), Some(20_423_633));

    // And when the chosen key is the one pulled out, the selection goes with it:
    // leaving the wizard aimed at a serial nobody can see is how the wrong key gets
    // written to
    hardware.set_devices(vec![
        device(31_000_001, "YubiKey 5C"),
        device(41_000_002, "YubiKey 5 Nano"),
    ]);
    until(&mut app, "the chosen key to be unplugged", |app| {
        app.target_serial().is_none()
    });
    assert!(app.attached.is_ambiguous());
    assert!(
        app.status.contains("unplugged"),
        "the operator has to be told: {}",
        app.status
    );

    // And with two attached and none chosen, that is on the record — which key was
    // picked, out of what, is part of the story of whatever is written next.
    //
    // Read from the register rather than from `audit_view`: the cached view is
    // refreshed after the operations that reload everything, and a key being plugged
    // in deliberately reloads nothing. The trail is what matters here.
    assert!(
        recorded(&app).contains(&"device.ambiguous".to_owned()),
        "{:?}",
        recorded(&app)
    );

    // When the operator chooses one
    app.select_key(31_000_001);

    // Then that is the target, the wizard field follows, and the choice is audited
    assert_eq!(app.target_serial(), Some(31_000_001));
    assert_eq!(app.wizard.serial, "31000001");
    let entries = app
        .store
        .as_ref()
        .unwrap()
        .audit_entries(50)
        .expect("the trail reads back");
    let chosen = entries
        .iter()
        .find(|e| e.event == "device.selected")
        .expect("choosing between keys is a decision worth recording");
    assert_eq!(chosen.target, "serial:31000001");
    assert!(chosen.details.contains("YubiKey 5C"), "{}", chosen.details);

    // And a run stops the watch before it writes: enumerating readers while another
    // handle holds an exclusive transaction is not a thing to discover halfway
    // through setting a PIN. This build has no write transport, so the run refuses —
    // *after* the watch has been stopped, which is the ordering being tested.
    assert!(app.watch.is_some());
    app.execute_run(yk_dist_manager::bootstrap::Confirmation::given(
        31_000_001, 0,
    ));
    assert!(
        app.watch.is_none(),
        "the watch must be stopped before a run writes to a key"
    );

    // And the next frame brings it back, because the sync compares what should be
    // running against what is
    app.sync_device_watch();
    assert!(app.watch.is_some());

    // Leaving the screen stops it, and clears what was attached: a stale "2 keys
    // attached" is worse than nothing, because it invites a choice based on nothing.
    app.tab = Tab::Holders;
    app.sync_device_watch();
    assert!(app.watch.is_none());
    assert!(app.attached.keys.is_empty());
    assert_eq!(app.attached.polls, 0);

    // The interval is the subprocess one, because this build has no native
    // transport — the trade documented in `device::watch`.
    assert_eq!(
        watch::interval_for(false),
        watch::POLL_INTERVAL_SUBPROCESS,
        "a fork per poll costs more than a PC/SC enumeration"
    );
}

/// Every event in the register, in the order it was written.
fn recorded(app: &YkDistApp) -> Vec<String> {
    app.store
        .as_ref()
        .expect("a register is open")
        .audit_entries(100)
        .expect("the trail reads back")
        .into_iter()
        .map(|entry| entry.event)
        .collect()
}

/// The watch owns its backend, and this test keeps poking the same one.
struct Shared(std::sync::Arc<MockBackend>);

impl yk_dist_manager::device::YubiKeyBackend for Shared {
    fn list_serials(&self) -> yk_dist_manager::device::Result<Vec<u32>> {
        self.0.list_serials()
    }
    fn info(&self, serial: Option<u32>) -> yk_dist_manager::device::Result<DeviceInfo> {
        self.0.info(serial)
    }
    fn describe(&self) -> String {
        self.0.describe()
    }
}
