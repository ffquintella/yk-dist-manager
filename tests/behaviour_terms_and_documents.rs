//! Behaviour tests for the paperwork half of a hand-over: generate the
//! consignment term, then file the signed copy against the distribution.

use yk_dist_manager::device::DeviceInfo;
use yk_dist_manager::domain::{
    AttachedDocument, DeliveryMethod, DistributionRecord, DocumentError, DocumentKind, Holder,
    KeyStatus, YubiKeyRecord,
};
use yk_dist_manager::store::{Store, StoreConfig};
use yk_dist_manager::term::{
    TermContext, TermTemplate, choose_template, edit_audit_entry, render_term,
};

struct World {
    store: Store,
}

impl World {
    fn new() -> Self {
        Self {
            store: Store::open_in_memory().unwrap(),
        }
    }

    /// Given: a key, a holder with their identification number, and a hand-over.
    fn handed_over(&self) -> (YubiKeyRecord, Holder, DistributionRecord) {
        let key = YubiKeyRecord::from_device(&DeviceInfo {
            serial: 20_423_633,
            model: "YubiKey 5 NFC".into(),
            firmware: "5.4.3".into(),
            form_factor: "Keychain (USB-A)".into(),
            nfc: true,
            usb_applications: vec!["FIDO2".into()],
        });
        self.store.upsert_key(&key).unwrap();

        let holder = Holder::new("Ana Silva", "ana.silva@example.org", "ESI", "12345")
            .unwrap()
            .with_optional("123.456.789-00", "+55 21 99999-0000", "")
            .unwrap();
        self.store.insert_holder(&holder).unwrap();

        let record = DistributionRecord {
            id: uuid::Uuid::new_v4(),
            key_id: key.id,
            key_serial: key.serial,
            holder_id: holder.id,
            holder_display: holder.display(),
            distributed_at: chrono::Utc::now(),
            distributed_by: "felipe".into(),
            method: DeliveryMethod::InPerson,
            receipt_ref: "TERM-2026-001".into(),
            bootstrap_run_id: None,
            returned_at: None,
            returned_to: None,
            notes: String::new(),
        };
        self.store.insert_distribution(&record).unwrap();
        self.store
            .set_key_status(key.serial, KeyStatus::Bootstrapped)
            .unwrap();
        self.store
            .set_key_status(key.serial, KeyStatus::Distributed)
            .unwrap();

        (key, holder, record)
    }
}

#[test]
fn scenario_generate_the_term_for_a_hand_over_in_portuguese() {
    // Given
    let world = World::new();
    world.store.seed_builtin_terms().unwrap();
    let (key, holder, record) = world.handed_over();

    // When
    let templates = world.store.term_templates().unwrap();
    let template = choose_template(&templates, "consignment", "pt-BR").expect("template");
    let ctx = TermContext::from_records(
        &holder,
        &key,
        Some(&record),
        "org-standard 1 — FIDO2 PIN",
        "Transport secret, holder must change it on first use",
        "felipe",
        "Example Organisation",
    );
    let text = render_term(template, &ctx).unwrap();

    // Then: the term names the person, their identification number, the key, and
    // the hand-over details — none of it retyped.
    assert!(text.contains("Ana Silva"));
    assert!(text.contains("123.456.789-00"));
    assert!(text.contains("20423633"));
    assert!(text.contains("TERM-2026-001"));
    assert!(text.contains("In person"));
    assert!(text.contains("felipe"));
}

#[test]
fn scenario_the_same_hand_over_in_english() {
    let world = World::new();
    world.store.seed_builtin_terms().unwrap();
    let (key, holder, record) = world.handed_over();

    let templates = world.store.term_templates().unwrap();
    let template = choose_template(&templates, "consignment", "en").expect("template");
    let ctx = TermContext::from_records(
        &holder,
        &key,
        Some(&record),
        "org-standard 1",
        "custody",
        "felipe",
        "Example Organisation",
    );
    let text = render_term(template, &ctx).unwrap();

    assert!(text.contains("Identification number: 123.456.789-00"));
    assert!(text.contains("Serial number: 20423633"));
    assert!(!text.contains("Address:"), "the holder gave no address");
}

#[test]
fn scenario_term_templates_are_seeded_once_and_survive_an_edit() {
    let world = World::new();
    assert_eq!(world.store.seed_builtin_terms().unwrap(), 2);
    // Seeding again must not duplicate or overwrite.
    assert_eq!(world.store.seed_builtin_terms().unwrap(), 0);

    // An edited template of the same id/language/version stays edited.
    let mut edited = TermTemplate::consignment_pt_br();
    edited.body = "Nome: {{holder.name}}\n".into();
    world.store.upsert_term_template(&edited).unwrap();
    assert_eq!(world.store.seed_builtin_terms().unwrap(), 0);

    let stored = world.store.term_templates().unwrap();
    let pt = stored
        .iter()
        .find(|t| t.language == "pt-BR")
        .expect("stored");
    assert_eq!(pt.body, "Nome: {{holder.name}}\n");
}

#[test]
fn scenario_a_unit_adds_its_own_language() {
    let world = World::new();
    world.store.seed_builtin_terms().unwrap();

    let spanish = TermTemplate {
        id: "consignment".into(),
        language: "es".into(),
        version: "1".into(),
        title: "Término de Consignación".into(),
        body: "Nombre: {{holder.name}}\nSerie: {{key.serial}}\n".into(),
    };
    spanish.validate().unwrap();
    world.store.upsert_term_template(&spanish).unwrap();

    let templates = world.store.term_templates().unwrap();
    let chosen = choose_template(&templates, "consignment", "es").unwrap();
    assert_eq!(chosen.language, "es");
    assert_eq!(templates.len(), 3);
}

#[test]
fn scenario_the_operator_edits_the_wording_and_the_next_term_uses_it() {
    // Given: the shipped pt-BR term, and a hand-over waiting for its document.
    let world = World::new();
    world.store.seed_builtin_terms().unwrap();
    let (key, holder, record) = world.handed_over();
    let before = world.store.term_templates().unwrap();
    let original = choose_template(&before, "consignment", "pt-BR").unwrap();
    assert_eq!(original.version, "1");

    // When: the wording is edited in the GUI and saved.
    let mut draft = original.clone();
    draft.body = draft.body.replace(
        "4.5. O portador declara ter recebido orientação sobre o uso da chave e sobre a",
        "4.5. O portador declara ter recebido treinamento presencial sobre o uso da chave e",
    );
    let stored = world.store.save_term_template_version(&draft).unwrap();

    // Then: it is a new version, the signed one is untouched, and the term
    // generated next carries the new wording.
    assert_eq!(stored.version, "2");
    let after = world.store.term_templates().unwrap();
    assert_eq!(after.len(), 3, "version 1 is kept, not overwritten");
    assert!(
        after
            .iter()
            .any(|t| t.language == "pt-BR" && t.version == "1" && t.body.contains("orientação")),
        "the version somebody may have signed stays readable"
    );

    let chosen = choose_template(&after, "consignment", "pt-BR").unwrap();
    assert_eq!(chosen.version, "2");
    let ctx = TermContext::from_records(
        &holder,
        &key,
        Some(&record),
        "org-standard 1",
        "Transport secret, holder must change it on first use",
        "felipe",
        "Example Organisation",
    );
    let text = render_term(chosen, &ctx).unwrap();
    assert!(text.contains("treinamento presencial"));
    assert!(
        text.contains("Ana Silva"),
        "still generated from the record"
    );
}

#[test]
fn scenario_the_editor_refuses_wording_that_could_not_render() {
    // Given a draft with a variable the context cannot supply — a typo in a
    // legal document that would otherwise only surface at the counter.
    let world = World::new();
    world.store.seed_builtin_terms().unwrap();
    let mut draft = TermTemplate::consignment_pt_br();
    draft.body = "Nome: {{holder.nome}}\n".into();

    // When it is saved
    let refused = world
        .store
        .save_term_template_version(&draft)
        .expect_err("must be refused");

    // Then nothing was stored, and the refusal names the variable.
    assert!(refused.to_string().contains("holder.nome"));
    assert_eq!(
        world
            .store
            .term_template_versions("consignment", "pt-BR")
            .unwrap(),
        vec!["1".to_owned()]
    );
}

#[test]
fn scenario_a_unit_adds_a_language_through_the_editor() {
    // Given the shipped languages only
    let world = World::new();
    world.store.seed_builtin_terms().unwrap();
    assert!(
        world
            .store
            .term_template_versions("consignment", "es")
            .unwrap()
            .is_empty()
    );

    // When a Spanish term is written in the editor and saved
    let draft = TermTemplate {
        id: "consignment".into(),
        language: "es".into(),
        // The editor leaves the version to the store, which numbers it.
        version: String::new(),
        title: "Término de Consignación de Llave de Seguridad".into(),
        body: "Nombre: {{holder.name}}\nSerie: {{key.serial}}\nTeléfono: {{holder.phone}}\n".into(),
    };
    let stored = world.store.save_term_template_version(&draft).unwrap();

    // Then it is version 1 of that language, and it is what a Spanish-speaking
    // holder gets — with the phone line dropped for a holder who gave none.
    assert_eq!(
        (stored.language.as_str(), stored.version.as_str()),
        ("es", "1")
    );
    let templates = world.store.term_templates().unwrap();
    let chosen = choose_template(&templates, "consignment", "es").unwrap();
    assert_eq!(chosen.language, "es");

    let holder = Holder::new("Carlos Ruiz", "carlos.ruiz@example.org", "ESI", "").unwrap();
    let key = YubiKeyRecord::from_device(&DeviceInfo {
        serial: 12_345_678,
        model: "YubiKey 5C".into(),
        firmware: "5.7.1".into(),
        form_factor: "Keychain (USB-C)".into(),
        nfc: false,
        usb_applications: vec!["FIDO2".into()],
    });
    let text = render_term(
        chosen,
        &TermContext::from_records(
            &holder,
            &key,
            None,
            "none",
            "custody",
            "felipe",
            "Example Organisation",
        ),
    )
    .unwrap();
    assert!(text.contains("Nombre: Carlos Ruiz"));
    assert!(!text.contains("Teléfono:"));
}

#[test]
fn scenario_editing_the_wording_is_audited() {
    // Given a term on record
    let world = World::new();
    world.store.seed_builtin_terms().unwrap();
    let mut draft = TermTemplate::consignment_pt_br();
    draft.title = "Termo de Consignação — revisão jurídica".into();

    // When it is saved and audited, the way the Terms screen does it
    let stored = world.store.save_term_template_version(&draft).unwrap();
    let (event, target, details) = edit_audit_entry(&stored, Some("1"));
    world
        .store
        .append_audit("felipe", event, &target, &details)
        .unwrap();

    // Then the trail says who changed which wording, from which version to which,
    // and the chain still verifies.
    let entries = world.store.audit_entries(10).unwrap();
    let entry = entries
        .iter()
        .find(|e| e.event == "term.template_edited")
        .expect("the edit must be audited");
    assert_eq!(entry.target, "term:consignment@pt-BR");
    assert_eq!(
        entry.details,
        "id=consignment language=pt-BR version=2 previous=1"
    );
    assert_eq!(entry.actor, "felipe");
    world.store.verify_audit().unwrap();

    // And a language that had nothing on record is an addition, not an edit.
    let spanish = TermTemplate {
        id: "consignment".into(),
        language: "es".into(),
        version: String::new(),
        title: "Término de Consignación".into(),
        body: "Nombre: {{holder.name}}\nSerie: {{key.serial}}\n".into(),
    };
    let stored = world.store.save_term_template_version(&spanish).unwrap();
    let (event, target, details) = edit_audit_entry(&stored, None);
    assert_eq!(event, "term.template_added");
    assert_eq!(target, "term:consignment@es");
    assert!(details.ends_with("previous=none"));
    world
        .store
        .append_audit("felipe", event, &target, &details)
        .unwrap();
    world.store.verify_audit().unwrap();
}

#[test]
fn scenario_two_edits_in_a_row_number_themselves() {
    // Given a term already edited once
    let world = World::new();
    world.store.seed_builtin_terms().unwrap();
    let mut draft = TermTemplate::consignment_pt_br();
    draft.title = "Termo de Consignação — revisão jurídica".into();
    let second = world.store.save_term_template_version(&draft).unwrap();
    assert_eq!(second.version, "2");

    // When it is edited again — the draft still carrying an old version number,
    // because the number is the store's to assign, not the editor's.
    draft.title = "Termo de Consignação — revisão do DPO".into();
    let third = world.store.save_term_template_version(&draft).unwrap();

    // Then the versions run 1, 2, 3 and the newest is the one in use.
    assert_eq!(third.version, "3");
    assert_eq!(
        world
            .store
            .term_template_versions("consignment", "pt-BR")
            .unwrap()
            .len(),
        3
    );
    let templates = world.store.term_templates().unwrap();
    assert_eq!(
        choose_template(&templates, "consignment", "pt-BR")
            .unwrap()
            .title,
        "Termo de Consignação — revisão do DPO"
    );
}

#[test]
fn scenario_file_the_signed_term_against_the_hand_over() {
    // Given a hand-over with nothing filed
    let world = World::new();
    let (_key, _holder, record) = world.handed_over();
    assert!(world.store.documents_for(record.id).unwrap().is_empty());

    // When the signed scan comes back and is uploaded
    let scan = b"%PDF-1.7 signed by Ana".to_vec();
    let document = AttachedDocument::new(
        record.id,
        DocumentKind::SignedTerm,
        "termo-assinado.pdf",
        scan.clone(),
        "felipe",
    )
    .unwrap();
    world.store.insert_document(&document).unwrap();

    // Then it is listed against that hand-over, without its bytes...
    let filed = world.store.documents_for(record.id).unwrap();
    assert_eq!(filed.len(), 1);
    assert_eq!(filed[0].kind, DocumentKind::SignedTerm);
    assert_eq!(filed[0].filename, "termo-assinado.pdf");
    assert_eq!(filed[0].media_type, "application/pdf");
    assert_eq!(filed[0].uploaded_by, "felipe");
    assert!(
        filed[0].content.is_none(),
        "a listing must not carry document bytes"
    );

    // ...and the content comes back byte-identical, verified against its digest.
    let loaded = world.store.document_content(filed[0].id).unwrap();
    assert_eq!(loaded.content.as_deref(), Some(scan.as_slice()));
    assert_eq!(loaded.verify(), Some(true));
    assert_eq!(loaded.sha256, document.sha256);
}

#[test]
fn scenario_the_signed_term_survives_a_restart_and_a_backup() {
    let dir = tempfile::tempdir().unwrap();
    let config = StoreConfig::new(dir.path().join("keys.sqlite3"));
    let scan = b"%PDF-1.7 signed".to_vec();
    let backup = dir.path().join("backup.sqlite3");

    let document_id = {
        let store = Store::open(&config).unwrap();
        let world = World { store };
        let (_key, _holder, record) = world.handed_over();
        let document = AttachedDocument::new(
            record.id,
            DocumentKind::SignedTerm,
            "termo.pdf",
            scan.clone(),
            "felipe",
        )
        .unwrap();
        world.store.insert_document(&document).unwrap();
        world.store.backup_to(&backup).unwrap();
        document.id
    };

    // Reopened: the document is still there.
    let reopened = Store::open(&config).unwrap();
    assert_eq!(
        reopened.document_content(document_id).unwrap().content,
        Some(scan.clone())
    );

    // And the backup is a complete copy — the whole point of one file.
    let restored = Store::open(&StoreConfig::new(&backup)).unwrap();
    let from_backup = restored.document_content(document_id).unwrap();
    assert_eq!(from_backup.content, Some(scan));
    assert_eq!(from_backup.verify(), Some(true));
}

#[test]
fn scenario_a_document_for_an_unknown_hand_over_is_refused() {
    let world = World::new();
    let orphan = AttachedDocument::new(
        uuid::Uuid::new_v4(), // no such distribution
        DocumentKind::SignedTerm,
        "termo.pdf",
        b"content".to_vec(),
        "felipe",
    )
    .unwrap();
    assert!(
        world.store.insert_document(&orphan).is_err(),
        "the foreign key must tie a document to a real hand-over"
    );
}

#[test]
fn scenario_an_unsupported_upload_is_refused_before_it_reaches_the_database() {
    let world = World::new();
    let (_key, _holder, record) = world.handed_over();

    let outcome = AttachedDocument::new(
        record.id,
        DocumentKind::SignedTerm,
        "malware.exe",
        b"MZ".to_vec(),
        "felipe",
    );
    assert!(matches!(outcome, Err(DocumentError::UnsupportedType(_))));
    assert!(world.store.documents_for(record.id).unwrap().is_empty());
}

#[test]
fn scenario_document_counts_drive_the_missing_term_badge() {
    let world = World::new();
    let (_key, _holder, record) = world.handed_over();

    // Nothing filed: the badge must be able to say so.
    assert!(
        !world
            .store
            .document_counts()
            .unwrap()
            .contains_key(&record.id)
    );

    let document = AttachedDocument::new(
        record.id,
        DocumentKind::SignedTerm,
        "termo.pdf",
        b"signed".to_vec(),
        "felipe",
    )
    .unwrap();
    world.store.insert_document(&document).unwrap();

    assert_eq!(
        world.store.document_counts().unwrap().get(&record.id),
        Some(&1)
    );
}

#[test]
fn scenario_optional_holder_fields_are_filled_in_not_blanked_by_a_later_edit() {
    // Given a holder whose identification number is on file
    let world = World::new();
    let holder = Holder::new("Ana Silva", "ana.silva@example.org", "ESI", "")
        .unwrap()
        .with_optional("123.456.789-00", "+55 21 99999-0000", "Rua A, 1")
        .unwrap();
    world.store.insert_holder(&holder).unwrap();

    // When the same person is registered again with only the required fields
    let again = Holder::new("Ana Silva Souza", "ana.silva@example.org", "ESI", "").unwrap();
    world.store.insert_holder(&again).unwrap();

    // Then the optional data is not lost — a term generated later still has it
    let stored = world.store.holders().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].full_name, "Ana Silva Souza", "the name updates");
    assert_eq!(stored[0].identification_number, "123.456.789-00");
    assert_eq!(stored[0].phone, "+55 21 99999-0000");
    assert_eq!(stored[0].address, "Rua A, 1");
}
