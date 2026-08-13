//! Behaviour test for the transport the application reads hardware through
//! (`features/native-device-transport.md` phase 6).
//!
//! The decision itself is unit-tested in `src/device/select.rs`, where every branch
//! is reachable because the availability is handed in. What is tested here is the
//! wiring, and the wiring is where this feature was broken for four waves: the
//! decision existed nowhere, `YkDistApp::new` said `YkmanBackend::default()`, and no
//! build configuration or setting could change it. So the assertions below are
//! about reachability and honesty:
//!
//! * a session decides a transport at startup and can say which one;
//! * changing the setting is persisted, audited, and takes effect;
//! * a device watch already running is stopped, so the status bar cannot name one
//!   transport while a thread polls another;
//! * choosing a transport this build cannot provide is reported, not pretended.
//!
//! **One test in this file, deliberately** — it drives `YkDistApp`, which reads
//! `$YKDM_SETTINGS` and `$YKDM_DATA_DIR`, and the process environment is shared by
//! every test in a binary.

use yk_dist_manager::YkDistApp;
use yk_dist_manager::app::Tab;
use yk_dist_manager::device::Transport;

fn recorded(app: &YkDistApp) -> Vec<String> {
    let store = app.store.as_ref().expect("a register");
    store
        .audit_entries(200)
        .unwrap_or_default()
        .into_iter()
        .map(|entry| entry.event)
        .collect()
}

#[test]
fn scenario_the_operator_can_choose_the_transport_and_is_told_which_one_is_in_use() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded, single test in this binary; the variables exist to
    // make exactly this redirection possible.
    unsafe {
        std::env::set_var("YKDM_DATA_DIR", home.path());
        std::env::set_var("YKDM_SETTINGS", home.path().join("settings.json"));
    }

    let mut app = YkDistApp::new(Some(home.path().join("keys.sqlite3")));
    assert!(app.store.is_some(), "{:?}", app.db_form.error);

    // A fresh install decides for itself, and the decision resolves to a concrete
    // transport — never `Automatic`, which is a request and not an answer.
    assert_eq!(app.settings.transport, Transport::Automatic);
    assert_ne!(
        app.transport.transport,
        Transport::Automatic,
        "`decide` must resolve the request into something that can be built"
    );
    // Whatever it picked, it can say why in words an operator could paste into a
    // ticket. A status bar reading just "native" does not distinguish an honoured
    // override from a lucky probe.
    let said = app.transport.describe();
    assert!(said.contains(" — "), "{said}");
    assert!(said.len() > 20, "the reason has to be a sentence: {said}");

    // Given a watch running, as it is on the screens that show attached keys
    app.tab = Tab::Inventory;
    app.sync_device_watch();
    assert!(app.watch.is_some(), "the inventory screen watches");

    // When the operator asks for the subprocess transport explicitly
    app.set_transport(Transport::Ykman);

    // Then it is honoured, persisted, and recorded
    assert_eq!(app.settings.transport, Transport::Ykman);
    assert_eq!(app.transport.transport, Transport::Ykman);
    assert!(
        recorded(&app).contains(&"device.transport.selected".to_owned()),
        "which transport wrote to a key is part of that key's story: {:?}",
        recorded(&app)
    );
    let reloaded = yk_dist_manager::settings::AppSettings::load();
    assert_eq!(
        reloaded.transport,
        Transport::Ykman,
        "a transport chosen in Settings has to survive a restart"
    );

    // And the watch is stopped, so the next frame rebuilds it through the new
    // choice. A thread polling the old transport while the status bar names the new
    // one is the failure this feature is supposed to prevent.
    assert!(
        app.watch.is_none(),
        "changing the transport must not leave a thread polling the old one"
    );
    app.sync_device_watch();
    assert!(app.watch.is_some(), "and the next frame starts it again");

    // When the same transport is asked for again
    let before = recorded(&app).len();
    app.set_transport(Transport::Ykman);
    assert_eq!(
        recorded(&app).len(),
        before,
        "a no-op must not fill the trail or restart detection"
    );

    // When native is asked for
    app.set_transport(Transport::Native);
    if cfg!(feature = "native-piv") {
        assert_eq!(app.transport.transport, Transport::Native);
        assert!(
            app.transport.reason.contains("Settings"),
            "an override says it was one: {}",
            app.transport.reason
        );
    } else {
        // The default build cannot provide it. It says which flag is missing rather
        // than silently reporting a transport it does not have — the setting is
        // still stored, because the operator's intent survives installing a build
        // that can honour it.
        assert_eq!(app.settings.transport, Transport::Native);
        assert_eq!(app.transport.transport, Transport::Ykman);
        assert!(
            app.transport.reason.contains("native-device"),
            "name the feature to rebuild with: {}",
            app.transport.reason
        );
    }

    // Whatever happened, the operator was told. The status line is the only place a
    // fallback becomes visible without opening Settings again.
    assert!(
        app.status.starts_with("transport: "),
        "the change has to be visible: {}",
        app.status
    );
}
