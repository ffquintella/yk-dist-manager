//! Behaviour tests for returning a key to factory default
//! (`features/key-lifecycle-and-revocation.md` phase 5).
//!
//! Every scenario drives the whole operation against `MockResetter` and a real
//! `Store`, the way the Inventory screen does. **No test here touches a key**,
//! and none can: the engine only ever calls `device::reset::Resetter`, and the
//! only implementation a test binary links to is the mock.
//!
//! The store is real on purpose. The unit tests in `device::reset` already prove
//! the sequencing against an in-memory recorder; what these add is the half that
//! only a database can show — that the entries survive into an append-only trail,
//! in order, naming the applet, and that a register which refuses to write stops
//! the reset before it starts.

use yk_dist_manager::device::reset::{
    self, Applet, Confirmation, MockResetter, Recorder, Request, ResetError, Status,
};
use yk_dist_manager::device::write::WriteError;
use yk_dist_manager::device::{DeviceInfo, MockBackend, YubiKeyBackend};
use yk_dist_manager::domain::YubiKeyRecord;
use yk_dist_manager::store::Store;

const SERIAL: u32 = 20_423_633;
const OPERATOR: &str = "ana.silva";

/// The register, and the recorder the screen would use.
struct World {
    store: Store,
    failure: Option<String>,
}

impl Recorder for World {
    fn audit(&mut self, event: &str, target: &str, detail: &str) -> Result<(), String> {
        self.store
            .append_audit(OPERATOR, event, target, detail)
            .map(|_| ())
            .map_err(|e| {
                let message = e.to_string();
                self.failure.get_or_insert(message.clone());
                message
            })
    }
}

impl World {
    /// Given: a register holding one key that has been out and come back.
    fn new() -> Self {
        let store = Store::open_in_memory().expect("in-memory database");
        let info: DeviceInfo = MockBackend::single_5nfc().info(None).expect("a mock key");
        let mut record = YubiKeyRecord::from_device(&DeviceInfo {
            serial: SERIAL,
            ..info
        });
        record.notes = "returned by the previous holder".into();
        store.upsert_key(&record).expect("key saved");
        Self {
            store,
            failure: None,
        }
    }

    fn events(&self) -> Vec<String> {
        self.store
            .audit_entries(200)
            .expect("the trail is readable")
            .into_iter()
            .map(|entry| entry.event)
            // `audit_entries` is newest-first, and a scenario reads forwards.
            .rev()
            .collect()
    }

    fn details_for(&self, event: &str) -> Vec<String> {
        self.store
            .audit_entries(200)
            .expect("the trail is readable")
            .into_iter()
            .filter(|entry| entry.event == event)
            .map(|entry| entry.details)
            .collect()
    }
}

/// A register that will not accept a write — a share that went away between the
/// operator opening the panel and using the button.
struct RefusesToRecord;

impl Recorder for RefusesToRecord {
    fn audit(&mut self, _: &str, _: &str, _: &str) -> Result<(), String> {
        Err("the share went away".into())
    }
}

#[test]
fn scenario_an_operator_returns_a_whole_key_to_factory_default() {
    // Given: a returned key, and an operator who has confirmed all three applets.
    let mut world = World::new();
    let mut resetter = MockResetter::attached(SERIAL);
    let request = Request::new(SERIAL, &Applet::ALL, OPERATOR);
    let confirmation = Confirmation::given(SERIAL, &Applet::ALL);

    // When: the reset runs.
    let outcomes = reset::perform(&request, &confirmation, &mut resetter, &mut world)
        .expect("a confirmed reset runs");

    // Then: every applet came back, in the order the hardware needs — FIDO2
    // first, because the authenticator only accepts a reset seconds after
    // power-up and the other two will wait.
    assert_eq!(resetter.calls(), &[Applet::Fido2, Applet::Piv, Applet::Otp]);
    assert!(reset::all_done(&outcomes));
    assert!(outcomes.iter().all(|o| o.status == Status::Done));

    // And: the trail says so, once per applet, with the operation bracketed by
    // an opening and a closing entry.
    assert_eq!(
        world.events(),
        vec![
            "key.reset.started",
            "key.applet_reset",
            "key.applet_reset",
            "key.applet_reset",
            "key.reset.finished",
        ]
    );
    let resets = world.details_for("key.applet_reset");
    for applet in Applet::ALL {
        assert!(
            resets
                .iter()
                .any(|d| d.contains(&format!("applet={}", applet.slug()))),
            "{} left no entry naming it: {resets:?}",
            applet.label()
        );
    }
    // And: which transport ran is on the record. Two of the three are `ykman`,
    // and a trail that did not say which would leave nobody able to answer "how
    // was this key reset" a year later.
    assert!(
        resets.iter().all(|d| d.contains("transport=")),
        "{resets:?}"
    );
    assert!(world.failure.is_none());
}

#[test]
fn scenario_the_key_is_left_alone_when_the_register_cannot_record_the_reset() {
    // Given: a register that refuses every write — the share dropped.
    let mut resetter = MockResetter::attached(SERIAL);
    let request = Request::new(SERIAL, &Applet::ALL, OPERATOR);
    let confirmation = Confirmation::given(SERIAL, &Applet::ALL);

    // When: the operator confirms the reset.
    let refused = reset::perform(&request, &confirmation, &mut resetter, &mut RefusesToRecord);

    // Then: nothing reached the key. A key wiped with no record of who did it is
    // precisely the event this register exists to hold, so the rule is the same
    // as the executor's: no record, no write.
    assert!(matches!(refused, Err(ResetError::NotRecordable(_))));
    assert!(resetter.calls().is_empty(), "nothing may reach the key");
}

#[test]
fn scenario_a_confirmation_does_not_carry_over_to_a_selection_the_operator_changed() {
    // Given: an operator who confirmed FIDO2 only …
    let mut world = World::new();
    let mut resetter = MockResetter::attached(SERIAL);
    let confirmation = Confirmation::given(SERIAL, &[Applet::Fido2]);

    // … and a request that has since grown a PIV reset.
    let request = Request::new(SERIAL, &[Applet::Fido2, Applet::Piv], OPERATOR);

    // When: it is run.
    let refused = reset::perform(&request, &confirmation, &mut resetter, &mut world);

    // Then: refused, before anything is written or recorded. The signing key in
    // 9c is not something an earlier agreement about FIDO2 authorises destroying.
    let error = refused.expect_err("a stale confirmation is not a confirmation");
    assert!(matches!(error, ResetError::ConfirmationMismatch { .. }));
    assert!(error.to_string().contains("fido2"), "{error}");
    assert!(resetter.calls().is_empty());
    assert!(world.events().is_empty(), "and nothing is on the record");
}

#[test]
fn scenario_a_key_that_refuses_one_applet_still_has_the_others_reset() {
    // Given: a key whose FIDO2 applet refuses — the operator did not re-insert
    // it inside the window CTAP allows.
    let mut world = World::new();
    let mut resetter = MockResetter::attached(SERIAL).fail(
        Applet::Fido2,
        WriteError::Failed {
            operation: "fido2.reset",
            reason: "the key was not re-inserted in time".into(),
        },
    );
    let request = Request::new(SERIAL, &Applet::ALL, OPERATOR);
    let confirmation = Confirmation::given(SERIAL, &Applet::ALL);

    // When: the reset runs.
    let outcomes = reset::perform(&request, &confirmation, &mut resetter, &mut world)
        .expect("a refusal is an outcome, not an error");

    // Then: PIV and OTP were still done. Stopping at the first refusal would
    // leave the operator with a key that is neither configured nor clean.
    assert_eq!(outcomes[0].status, Status::Failed);
    assert_eq!(outcomes[1].status, Status::Done);
    assert_eq!(outcomes[2].status, Status::Done);
    assert!(!reset::all_done(&outcomes));

    // And: the failure is on the record beside the successes, naming the applet
    // that refused and keeping the transport's own words.
    assert_eq!(
        world.events(),
        vec![
            "key.reset.started",
            "key.reset.failed",
            "key.applet_reset",
            "key.applet_reset",
            "key.reset.finished",
        ]
    );
    let failed = world.details_for("key.reset.failed");
    assert!(failed[0].contains("applet=fido2"), "{failed:?}");
    assert!(failed[0].contains("re-inserted"), "{failed:?}");
    assert_eq!(
        world.details_for("key.reset.finished")[0],
        "reset=2 failed=1 skipped=0"
    );
}

#[test]
fn scenario_a_key_pulled_out_half_way_reports_what_never_ran() {
    // Given: a key that is unplugged during the first applet.
    let mut world = World::new();
    let mut resetter = MockResetter::attached(SERIAL).fail(
        Applet::Fido2,
        WriteError::Detached {
            operation: "fido2.reset",
        },
    );
    let request = Request::new(SERIAL, &Applet::ALL, OPERATOR);
    let confirmation = Confirmation::given(SERIAL, &Applet::ALL);

    // When: the reset runs.
    let outcomes = reset::perform(&request, &confirmation, &mut resetter, &mut world)
        .expect("an interrupted reset still returns its account");

    // Then: nothing further was attempted, and the two applets that never ran
    // say so rather than being left out of the answer — an operator has to know
    // that this key is now part-reset.
    assert_eq!(resetter.calls(), &[Applet::Fido2]);
    assert_eq!(outcomes.len(), 3);
    assert!(outcomes.iter().all(|o| o.status == Status::Failed));
    assert!(outcomes[2].detail.contains("no longer attached"));
    assert_eq!(world.details_for("key.reset.failed").len(), 3);
}

#[test]
fn scenario_the_inventory_record_outlives_the_reset() {
    // Given: a key with an observation on it.
    let mut world = World::new();
    let mut resetter = MockResetter::attached(SERIAL);
    let request = Request::new(SERIAL, &Applet::ALL, OPERATOR);
    let confirmation = Confirmation::given(SERIAL, &Applet::ALL);

    // When: it is returned to factory default.
    reset::perform(&request, &confirmation, &mut resetter, &mut world).expect("the reset runs");

    // Then: the register still holds the key, its observation and its history.
    // A reset destroys what is on the hardware and nothing that is on file —
    // the record of a key is what makes the reset auditable at all.
    let key = world
        .store
        .keys()
        .expect("the inventory is readable")
        .into_iter()
        .find(|k| k.serial == SERIAL)
        .expect("the key is still in the inventory");
    assert_eq!(key.notes, "returned by the previous holder");

    // And: the trail is intact and verifiable — the reset is recorded in the
    // same append-only chain as everything else.
    assert_eq!(
        world.store.verify_audit().expect("the chain verifies"),
        5,
        "one opening entry, one per applet, one closing"
    );
}
