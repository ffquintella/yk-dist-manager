//! Unit tests for consignment terms: rendering, optional-field omission, and
//! language selection.

use yk_dist_manager::device::DeviceInfo;
use yk_dist_manager::domain::{Holder, SerialSource, YubiKeyRecord};
use yk_dist_manager::term::{
    BUILTIN_LANGUAGES, DEFAULT_LANGUAGE, MAX_BODY, TermContext, TermError, TermTemplate,
    choose_template, is_edited, languages_of, latest_in_language, next_version, render_term,
};

/// A template in one language and version, for the version-selection tests.
fn versioned(language: &str, version: &str, body: &str) -> TermTemplate {
    TermTemplate {
        id: "consignment".into(),
        language: language.into(),
        version: version.into(),
        title: format!("Term {version}"),
        body: body.into(),
    }
}

fn holder_full() -> Holder {
    Holder::new("Ana Silva", "ana.silva@fgv.br", "ESI", "12345")
        .unwrap()
        .with_optional(
            "123.456.789-00",
            "+55 21 99999-0000",
            "Praia de Botafogo 190",
        )
        .unwrap()
}

fn holder_minimal() -> Holder {
    Holder::new("Bruno Costa", "bruno.costa@fgv.br", "ESI", "").unwrap()
}

fn key() -> YubiKeyRecord {
    YubiKeyRecord::from_device(&DeviceInfo {
        serial: 20_423_633,
        model: "YubiKey 5 NFC".into(),
        firmware: "5.4.3".into(),
        form_factor: "Keychain (USB-A)".into(),
        nfc: true,
        usb_applications: vec!["FIDO2".into(), "PIV".into()],
    })
}

fn ctx(holder: &Holder) -> TermContext {
    TermContext::from_records(
        holder,
        &key(),
        None,
        "fgv-standard 1 — FIDO2 PIN, PIV certificate import",
        "Transport secret, holder must change it on first use",
        "felipe",
        "FGV",
    )
}

#[test]
fn the_builtin_terms_are_valid_and_cover_both_languages() {
    let templates = TermTemplate::builtin();
    assert_eq!(templates.len(), BUILTIN_LANGUAGES.len());
    for template in &templates {
        template
            .validate()
            .unwrap_or_else(|e| panic!("{} is invalid: {e}", template.language));
        assert!(BUILTIN_LANGUAGES.contains(&template.language.as_str()));
        assert_eq!(template.id, "consignment");
    }
}

#[test]
fn a_term_carries_the_name_and_identification_number_from_the_record() {
    let holder = holder_full();
    let text = render_term(&TermTemplate::consignment_pt_br(), &ctx(&holder)).unwrap();

    assert!(text.contains("Ana Silva"), "{text}");
    assert!(text.contains("123.456.789-00"), "{text}");
    assert!(text.contains("Número de identificação"), "{text}");
    assert!(text.contains("20423633"), "the key serial must appear");
    assert!(text.contains("ana.silva@fgv.br"));
}

#[test]
fn optional_fields_that_are_filled_in_appear() {
    let holder = holder_full();
    let text = render_term(&TermTemplate::consignment_pt_br(), &ctx(&holder)).unwrap();
    assert!(text.contains("+55 21 99999-0000"));
    assert!(text.contains("Praia de Botafogo 190"));
    assert!(text.contains("Telefone:"));
    assert!(text.contains("Endereço:"));
}

#[test]
fn optional_fields_that_are_empty_take_their_whole_line_with_them() {
    let holder = holder_minimal();
    let text = render_term(&TermTemplate::consignment_pt_br(), &ctx(&holder)).unwrap();

    // No stray labels with nothing after them.
    assert!(!text.contains("Telefone:"), "{text}");
    assert!(!text.contains("Endereço:"), "{text}");
    assert!(!text.contains("Matrícula:"), "{text}");
    assert!(!text.contains("Número de identificação:"), "{text}");
    // The mandatory content is still there.
    assert!(text.contains("Bruno Costa"));
    assert!(text.contains("20423633"));
    assert!(text.contains("RESPONSABILIDADES DO PORTADOR"));
}

#[test]
fn a_line_without_variables_is_always_kept() {
    let template = TermTemplate {
        id: "t".into(),
        language: "en".into(),
        version: "1".into(),
        title: "Title".into(),
        body: "A fixed sentence.\nName: {{holder.name}}\nPhone: {{holder.phone}}\n".into(),
    };
    let holder = holder_minimal();
    let text = render_term(&template, &ctx(&holder)).unwrap();

    assert!(text.contains("A fixed sentence."));
    assert!(text.contains("Name: Bruno Costa"));
    assert!(!text.contains("Phone:"));
}

#[test]
fn the_english_term_says_identification_number_too() {
    let holder = holder_full();
    let text = render_term(&TermTemplate::consignment_en(), &ctx(&holder)).unwrap();
    assert!(text.contains("Identification number: 123.456.789-00"));
    assert!(text.contains("Security Key Consignment Term"));
    assert!(text.contains("temporary"), "custody model B must be stated");
}

#[test]
fn the_term_states_what_was_applied_and_the_custody_model() {
    let holder = holder_full();
    let text = render_term(&TermTemplate::consignment_pt_br(), &ctx(&holder)).unwrap();
    assert!(text.contains("fgv-standard 1"));
    assert!(text.contains("Transport secret"));
    assert!(
        text.contains("provisório"),
        "the holder must be told the PIN is temporary"
    );
}

#[test]
fn an_unknown_variable_is_refused_before_anything_is_printed() {
    let template = TermTemplate {
        id: "t".into(),
        language: "en".into(),
        version: "1".into(),
        title: "Title".into(),
        body: "{{holder.shoe_size}}".into(),
    };
    assert_eq!(
        template.validate().unwrap_err(),
        TermError::UnknownVariable("holder.shoe_size".into())
    );
    let holder = holder_full();
    assert!(matches!(
        render_term(&template, &ctx(&holder)).unwrap_err(),
        TermError::UnknownVariable(_)
    ));
}

#[test]
fn an_unterminated_placeholder_is_an_error() {
    let template = TermTemplate {
        id: "t".into(),
        language: "en".into(),
        version: "1".into(),
        title: "Title".into(),
        body: "Name: {{holder.name".into(),
    };
    let holder = holder_full();
    assert_eq!(
        render_term(&template, &ctx(&holder)).unwrap_err(),
        TermError::Unterminated
    );
}

#[test]
fn a_term_cannot_be_issued_without_a_name_or_a_serial() {
    let holder = holder_full();
    let mut context = ctx(&holder);
    context.holder_name.clear();
    assert!(matches!(
        render_term(&TermTemplate::consignment_en(), &context).unwrap_err(),
        TermError::Incomplete("the holder's name")
    ));

    let mut context = ctx(&holder);
    context.key_serial.clear();
    assert!(matches!(
        render_term(&TermTemplate::consignment_en(), &context).unwrap_err(),
        TermError::Incomplete("the key serial")
    ));
}

#[test]
fn every_documented_variable_resolves() {
    let holder = holder_full();
    let map = ctx(&holder).as_map();
    for name in TermContext::VARIABLES {
        assert!(map.contains_key(name), "`{name}` is not resolvable");
    }
}

#[test]
fn language_selection_prefers_an_exact_match() {
    let templates = TermTemplate::builtin();
    let chosen = choose_template(&templates, "consignment", "en").unwrap();
    assert_eq!(chosen.language, "en");
    let chosen = choose_template(&templates, "consignment", "pt-BR").unwrap();
    assert_eq!(chosen.language, "pt-BR");
}

#[test]
fn language_selection_falls_back_to_the_base_language() {
    let templates = TermTemplate::builtin();
    // `pt` is not a template language; `pt-BR` is the same base language.
    let chosen = choose_template(&templates, "consignment", "pt").unwrap();
    assert_eq!(chosen.language, "pt-BR");
}

#[test]
fn an_unknown_language_falls_back_to_the_default_rather_than_failing() {
    let templates = TermTemplate::builtin();
    let chosen = choose_template(&templates, "consignment", "de").unwrap();
    assert_eq!(
        chosen.language, DEFAULT_LANGUAGE,
        "a term in the wrong language beats no term, and the caller is told"
    );
}

#[test]
fn an_unknown_template_id_selects_nothing() {
    let templates = TermTemplate::builtin();
    assert!(choose_template(&templates, "nonexistent", "en").is_none());
}

#[test]
fn referenced_variables_are_listed_in_order_without_duplicates() {
    let template = TermTemplate {
        id: "t".into(),
        language: "en".into(),
        version: "1".into(),
        title: "T".into(),
        body: "{{holder.name}} {{key.serial}} {{holder.name}}".into(),
    };
    assert_eq!(
        template.referenced_variables(),
        vec!["holder.name".to_owned(), "key.serial".to_owned()]
    );
}

#[test]
fn a_key_known_only_by_serial_still_produces_a_term() {
    // A key recorded from a scanned label has no model or firmware yet; those
    // lines drop out rather than printing empty labels.
    let holder = holder_full();
    let scanned = YubiKeyRecord::from_serial(31_415_926, SerialSource::ScannedLabel);
    let context = TermContext::from_records(
        &holder,
        &scanned,
        None,
        "nothing recorded",
        "Transport secret, holder must change it on first use",
        "felipe",
        "FGV",
    );

    let text = render_term(&TermTemplate::consignment_pt_br(), &context).unwrap();
    assert!(text.contains("31415926"));
    assert!(
        !text.contains("Modelo:"),
        "unknown model must not print a label"
    );
    assert!(!text.contains("Versão de firmware:"));
}

// ------------------------------------------------------ editing a term template

#[test]
fn the_next_version_is_one_past_the_highest_number_on_record() {
    assert_eq!(next_version(&[]), "1");
    assert_eq!(next_version(&["1".to_owned()]), "2");
    // Text ordering would put `10` before `9`; the numbering must not.
    let many: Vec<String> = ["1", "2", "9", "10"]
        .iter()
        .map(|v| v.to_string())
        .collect();
    assert_eq!(next_version(&many), "11");
}

#[test]
fn a_hand_named_version_does_not_block_the_numbering() {
    // A template whose version is not a number counts as 0, so the first edit of
    // it is version 1 rather than an error.
    assert_eq!(next_version(&["draft".to_owned()]), "1");
}

#[test]
fn generating_a_term_takes_the_newest_version_of_the_language() {
    // Given: an edited pt-BR term stored beside the version it replaced.
    let templates = vec![
        versioned("pt-BR", "1", "original {{holder.name}}\n"),
        versioned("pt-BR", "2", "edited {{holder.name}}\n"),
        versioned("en", "1", "english {{holder.name}}\n"),
    ];

    // When / Then: the newest version is the one a term is generated from, and
    // the older one is still there to read.
    let chosen = choose_template(&templates, "consignment", "pt-BR").unwrap();
    assert_eq!(chosen.version, "2");
    assert!(chosen.body.starts_with("edited"));
    assert_eq!(
        latest_in_language(&templates, "consignment", "pt-BR")
            .unwrap()
            .version,
        "2"
    );
    assert_eq!(templates.len(), 3, "nothing was replaced");
}

#[test]
fn version_ten_wins_over_version_nine() {
    let templates = vec![
        versioned("pt-BR", "9", "nine\n"),
        versioned("pt-BR", "10", "ten\n"),
    ];
    let chosen = choose_template(&templates, "consignment", "pt-BR").unwrap();
    assert_eq!(chosen.version, "10");
}

#[test]
fn the_base_language_fallback_also_takes_the_newest_version() {
    let templates = vec![
        versioned("pt-BR", "1", "one\n"),
        versioned("pt-BR", "3", "three\n"),
    ];
    // `pt` is satisfied by `pt-BR`, and by its newest version.
    let chosen = choose_template(&templates, "consignment", "pt").unwrap();
    assert_eq!(chosen.version, "3");
}

#[test]
fn each_language_is_listed_once_at_its_newest_version() {
    let templates = vec![
        versioned("pt-BR", "1", "one\n"),
        versioned("pt-BR", "2", "two\n"),
        versioned("en", "1", "one\n"),
        versioned("es", "4", "cuatro\n"),
    ];
    let listed = languages_of(&templates, "consignment");
    let pairs: Vec<(&str, &str)> = listed
        .iter()
        .map(|t| (t.language.as_str(), t.version.as_str()))
        .collect();
    assert_eq!(pairs, vec![("en", "1"), ("es", "4"), ("pt-BR", "2")]);
}

#[test]
fn a_template_with_an_unknown_variable_is_refused_before_it_is_stored() {
    let template = versioned("pt-BR", "1", "Nome: {{holder.nome}}\n");
    assert_eq!(
        template.check().unwrap_err(),
        TermError::UnknownVariable("holder.nome".into()),
        "a term that cannot render must be refused at the editor, not at the counter"
    );
}

#[test]
fn a_term_template_needs_a_title_and_a_body() {
    let mut template = TermTemplate::consignment_pt_br();
    template.title = "   ".into();
    assert_eq!(template.check().unwrap_err(), TermError::Missing("a title"));

    let mut template = TermTemplate::consignment_pt_br();
    template.body = String::new();
    assert_eq!(template.check().unwrap_err(), TermError::Missing("a body"));

    let mut template = TermTemplate::consignment_pt_br();
    template.language = String::new();
    assert_eq!(
        template.check().unwrap_err(),
        TermError::Missing("a language")
    );
}

#[test]
fn a_term_body_is_length_bound_like_every_other_input() {
    let mut template = TermTemplate::consignment_pt_br();
    template.body = "a".repeat(MAX_BODY + 1);
    assert_eq!(
        template.check().unwrap_err(),
        TermError::TooLong {
            field: "body",
            max: MAX_BODY
        }
    );
}

#[test]
fn the_builtin_wording_passes_the_editor_check() {
    for template in TermTemplate::builtin() {
        template
            .check()
            .unwrap_or_else(|e| panic!("{} is not storable: {e}", template.language));
    }
}

#[test]
fn the_builtin_wording_can_be_recovered_for_a_language_the_build_ships() {
    assert!(TermTemplate::builtin_for("consignment", "pt-BR").is_some());
    assert!(TermTemplate::builtin_for("consignment", "en").is_some());
    assert!(TermTemplate::builtin_for("consignment", "es").is_none());
    let blank = TermTemplate::blank("consignment", "es");
    assert_eq!(blank.language, "es");
    assert!(blank.body.is_empty());
}

#[test]
fn an_unsaved_edit_is_recognised_as_one() {
    let stored = versioned("pt-BR", "1", "Nome: {{holder.name}}\n");

    // The buffers as loaded: not an edit. Surrounding space in the title is not
    // an edit either, because that is what is stored.
    assert!(!is_edited(
        Some(&stored),
        "Term 1",
        "Nome: {{holder.name}}\n"
    ));
    assert!(!is_edited(
        Some(&stored),
        "  Term 1  ",
        "Nome: {{holder.name}}\n"
    ));

    // A changed word in either field is.
    assert!(is_edited(
        Some(&stored),
        "Termo 1",
        "Nome: {{holder.name}}\n"
    ));
    assert!(is_edited(
        Some(&stored),
        "Term 1",
        "Nome: {{holder.email}}\n"
    ));

    // A language with nothing stored: an edit as soon as anything is typed.
    assert!(!is_edited(None, "", "   "));
    assert!(is_edited(None, "Término", ""));
}

#[test]
fn the_sample_context_fills_every_variable_so_no_preview_line_is_hidden() {
    let sample = TermContext::sample();
    for (name, value) in sample.as_map() {
        assert!(
            !value.trim().is_empty(),
            "`{name}` is empty in the sample, so its line would vanish from the preview"
        );
    }
    // And it renders both shipped languages.
    for template in TermTemplate::builtin() {
        let text = render_term(&template, &sample).unwrap();
        assert!(text.contains("Ana Exemplo da Silva"));
    }
}
