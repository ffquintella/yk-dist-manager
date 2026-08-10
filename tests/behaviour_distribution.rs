//! Behaviour tests for the distribution workflow.
//!
//! Each test is one scenario, written Given / When / Then. They exercise the
//! store the way the GUI does, so a regression in the workflow fails here even
//! if every unit test still passes.

use yk_dist_manager::device::DeviceInfo;
use yk_dist_manager::domain::{
    DeliveryMethod, DistributionRecord, Holder, KeyStatus, YubiKeyRecord,
};
use yk_dist_manager::store::{Store, StoreError};

struct World {
    store: Store,
}

impl World {
    /// Given: an empty database.
    fn new() -> Self {
        Self {
            store: Store::open_in_memory().expect("in-memory database"),
        }
    }

    /// Given: a key in stock.
    fn key_in_stock(&self, serial: u32) -> YubiKeyRecord {
        let record = YubiKeyRecord::from_device(&DeviceInfo {
            serial,
            model: "YubiKey 5 NFC".into(),
            firmware: "5.4.3".into(),
            form_factor: "Keychain (USB-A)".into(),
            nfc: true,
            usb_applications: vec!["FIDO2".into(), "PIV".into()],
        });
        self.store.upsert_key(&record).expect("key saved");
        record
    }

    /// Given: a registered holder.
    fn holder(&self, name: &str, email: &str) -> Holder {
        let holder = Holder::new(name, email, "ESI", "").expect("valid holder");
        self.store.insert_holder(&holder).expect("holder saved");
        holder
    }

    /// When: the key is handed over.
    fn distribute(&self, key: &YubiKeyRecord, holder: &Holder, by: &str) -> DistributionRecord {
        let record = DistributionRecord {
            id: uuid::Uuid::new_v4(),
            key_id: key.id,
            key_serial: key.serial,
            holder_id: holder.id,
            holder_display: holder.display(),
            distributed_at: chrono::Utc::now(),
            distributed_by: by.to_owned(),
            method: DeliveryMethod::InPerson,
            receipt_ref: "TERM-2026-001".into(),
            bootstrap_run_id: None,
            returned_at: None,
            returned_to: None,
            notes: String::new(),
        };
        self.store
            .insert_distribution(&record)
            .expect("distribution saved");
        self.store
            .set_key_status(key.serial, KeyStatus::Bootstrapped)
            .expect("bootstrapped");
        self.store
            .set_key_status(key.serial, KeyStatus::Distributed)
            .expect("distributed");
        record
    }
}

#[test]
fn scenario_hand_a_key_to_a_person_and_see_who_holds_it() {
    // Given
    let world = World::new();
    let key = world.key_in_stock(20_423_633);
    let ana = world.holder("Ana Silva", "ana.silva@fgv.br");

    // When
    world.distribute(&key, &ana, "felipe");

    // Then
    let distributions = world.store.distributions().unwrap();
    assert_eq!(distributions.len(), 1);
    let record = &distributions[0];
    assert_eq!(record.key_serial, 20_423_633);
    assert_eq!(record.holder_display, "Ana Silva <ana.silva@fgv.br>");
    assert_eq!(record.distributed_by, "felipe");
    assert!(record.is_open());

    let stored_key = world.store.key_by_serial(20_423_633).unwrap().unwrap();
    assert_eq!(stored_key.status, KeyStatus::Distributed);
}

#[test]
fn scenario_a_key_cannot_be_distributed_straight_from_stock() {
    // Given a key that was never bootstrapped
    let world = World::new();
    world.key_in_stock(20_423_633);

    // When we try to mark it distributed
    let outcome = world
        .store
        .set_key_status(20_423_633, KeyStatus::Distributed);

    // Then the store refuses instead of silently accepting
    assert!(matches!(outcome, Err(StoreError::Transition { .. })));
}

#[test]
fn scenario_returning_a_key_closes_the_record_without_rewriting_history() {
    // Given a distributed key
    let world = World::new();
    let key = world.key_in_stock(20_423_633);
    let ana = world.holder("Ana Silva", "ana.silva@fgv.br");
    let record = world.distribute(&key, &ana, "felipe");

    // When the holder gives it back
    world.store.mark_returned(record.id, "felipe").unwrap();
    world
        .store
        .set_key_status(key.serial, KeyStatus::Returned)
        .unwrap();

    // Then the original hand-over is still there, now closed
    let stored = world.store.distributions().unwrap();
    assert_eq!(stored.len(), 1, "no new row: the same record is closed");
    assert!(!stored[0].is_open());
    assert_eq!(stored[0].returned_to.as_deref(), Some("felipe"));
    assert_eq!(stored[0].distributed_by, "felipe");
    assert_eq!(
        world
            .store
            .key_by_serial(20_423_633)
            .unwrap()
            .unwrap()
            .status,
        KeyStatus::Returned
    );
}

#[test]
fn scenario_a_key_cannot_be_returned_twice() {
    // Given a key already returned
    let world = World::new();
    let key = world.key_in_stock(20_423_633);
    let ana = world.holder("Ana Silva", "ana.silva@fgv.br");
    let record = world.distribute(&key, &ana, "felipe");
    world.store.mark_returned(record.id, "felipe").unwrap();

    // When someone records the return again
    let outcome = world.store.mark_returned(record.id, "someone-else");

    // Then it is rejected, and the first return stands
    assert!(matches!(outcome, Err(StoreError::NotFound(_))));
    let stored = world.store.distributions().unwrap();
    assert_eq!(stored[0].returned_to.as_deref(), Some("felipe"));
}

#[test]
fn scenario_one_key_reissued_to_a_second_holder_keeps_both_records() {
    // Given a key that went out and came back
    let world = World::new();
    let key = world.key_in_stock(20_423_633);
    let ana = world.holder("Ana Silva", "ana.silva@fgv.br");
    let bruno = world.holder("Bruno Costa", "bruno.costa@fgv.br");
    let first = world.distribute(&key, &ana, "felipe");
    world.store.mark_returned(first.id, "felipe").unwrap();
    world
        .store
        .set_key_status(key.serial, KeyStatus::Returned)
        .unwrap();

    // When it is handed to somebody else
    world.distribute(&key, &bruno, "felipe");

    // Then the full history of custody is available
    let stored = world.store.distributions().unwrap();
    assert_eq!(stored.len(), 2);
    assert!(stored.iter().any(|d| d.holder_id == ana.id && !d.is_open()));
    assert!(
        stored
            .iter()
            .any(|d| d.holder_id == bruno.id && d.is_open())
    );
}

#[test]
fn scenario_the_same_person_is_not_duplicated_by_email() {
    // Given a registered holder
    let world = World::new();
    world.holder("Ana Silva", "ana.silva@fgv.br");

    // When the same address is registered again with a corrected name
    world.holder("Ana Silva Souza", "ana.silva@fgv.br");

    // Then there is still one person, with the newer name
    let holders = world.store.holders().unwrap();
    assert_eq!(holders.len(), 1);
    assert_eq!(holders[0].full_name, "Ana Silva Souza");
}

#[test]
fn scenario_reading_the_same_key_twice_does_not_duplicate_inventory() {
    // Given a key already in the inventory
    let world = World::new();
    world.key_in_stock(20_423_633);

    // When it is read again with newer firmware
    let mut refreshed = world.store.key_by_serial(20_423_633).unwrap().unwrap();
    refreshed.firmware = "5.7.1".into();
    world.store.upsert_key(&refreshed).unwrap();

    // Then one inventory row exists, updated
    let keys = world.store.keys().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].firmware, "5.7.1");
}
