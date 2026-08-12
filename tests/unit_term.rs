//! Unit tests for consignment terms: rendering, optional-field omission,
//! language selection, and the PDF the holder signs.

use yk_dist_manager::device::DeviceInfo;
use yk_dist_manager::domain::{Holder, SerialSource, YubiKeyRecord};
use yk_dist_manager::term::{
    BUILTIN_LANGUAGES, DEFAULT_LANGUAGE, MAX_BODY, TermContext, TermError, TermTemplate,
    choose_template, is_edited, languages_of, latest_in_language, next_version, pdf_footer,
    pdf_subject, render_term, render_term_parts, render_term_pdf,
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
    Holder::new("Ana Silva", "ana.silva@example.org", "ESI", "12345")
        .unwrap()
        .with_optional(
            "123.456.789-00",
            "+55 21 99999-0000",
            "Praia de Botafogo 190",
        )
        .unwrap()
}

fn holder_minimal() -> Holder {
    Holder::new("Bruno Costa", "bruno.costa@example.org", "ESI", "").unwrap()
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
        "org-standard 1 — FIDO2 PIN, PIV certificate import",
        "Transport secret, holder must change it on first use",
        "felipe",
        "Example Organisation",
    )
}

#[test]
fn the_builtin_terms_cover_every_document_in_every_language() {
    // Two documents — the consignment term and the return receipt that closes the
    // custody loop — in both shipped languages. The count is asserted as a product
    // so that adding one and forgetting the other fails here rather than being
    // found by a holder who is handed a receipt in the wrong language.
    use yk_dist_manager::term::BUILTIN_IDS;

    let templates = TermTemplate::builtin();
    assert_eq!(templates.len(), BUILTIN_IDS.len() * BUILTIN_LANGUAGES.len());

    for template in &templates {
        template
            .validate()
            .unwrap_or_else(|e| panic!("{} {} is invalid: {e}", template.id, template.language));
        assert!(BUILTIN_LANGUAGES.contains(&template.language.as_str()));
        assert!(
            BUILTIN_IDS.contains(&template.id.as_str()),
            "unexpected id {}",
            template.id
        );
    }

    for id in BUILTIN_IDS {
        for language in BUILTIN_LANGUAGES {
            assert!(
                templates
                    .iter()
                    .any(|t| t.id == id && t.language == language),
                "no built-in {id} in {language}"
            );
        }
    }
}

#[test]
fn the_return_receipt_names_both_ends_of_the_custody_and_what_happens_next() {
    // The document exists to close the loop, so three things have to survive
    // rendering: when the key went out, when it came back, and that the credentials
    // on it are somebody's job to revoke. A returned key whose certificate is still
    // valid is a credential in a drawer.
    use yk_dist_manager::term::{RETURN_ID, TermContext, choose_template, render_term};

    let templates = TermTemplate::builtin();
    let mut ctx = TermContext::sample();
    ctx.handover_date = "03/02/2026".into();
    ctx.return_date = "11/08/2026".into();
    ctx.return_to = "felipe".into();

    for language in BUILTIN_LANGUAGES {
        let template = choose_template(&templates, RETURN_ID, language).expect("a return receipt");
        let text = render_term(template, &ctx).expect("it renders");

        assert!(text.contains("03/02/2026"), "{language}: {text}");
        assert!(text.contains("11/08/2026"), "{language}: {text}");
        assert!(text.contains("felipe"), "{language}: {text}");
        assert!(text.contains(&ctx.key_serial), "{language}: {text}");
        // The revocation sentence, in either language.
        let revocation = text.to_lowercase();
        assert!(
            revocation.contains("revoga") || revocation.contains("revocation"),
            "{language}: the receipt must say the credentials get revoked: {text}"
        );
        // And no leftover placeholder.
        assert!(!text.contains("{{"), "{language}: {text}");
    }
}

#[test]
fn a_consignment_term_carries_no_return_lines_while_the_key_is_held() {
    // The lines that name a return resolve to empty for a key that has not come
    // back, and `render_term` drops a line whose variable is empty — which is how
    // one context serves both documents without a conditional in the template.
    use yk_dist_manager::term::{CONSIGNMENT_ID, TermContext, choose_template, render_term};

    let templates = TermTemplate::builtin();
    let mut ctx = TermContext::sample();
    ctx.return_date = String::new();
    ctx.return_to = String::new();

    let template = choose_template(&templates, CONSIGNMENT_ID, "pt-BR").unwrap();
    let text = render_term(template, &ctx).unwrap();
    assert!(!text.to_lowercase().contains("devolução"), "{text}");
    assert!(!text.contains("{{"), "{text}");
}

#[test]
fn a_term_carries_the_name_and_identification_number_from_the_record() {
    let holder = holder_full();
    let text = render_term(&TermTemplate::consignment_pt_br(), &ctx(&holder)).unwrap();

    assert!(text.contains("Ana Silva"), "{text}");
    assert!(text.contains("123.456.789-00"), "{text}");
    assert!(text.contains("Número de identificação"), "{text}");
    assert!(text.contains("20423633"), "the key serial must appear");
    assert!(text.contains("ana.silva@example.org"));
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
    assert!(text.contains("org-standard 1"));
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
        "Example Organisation",
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

// ------------------------------------------------------------- the PDF output

#[test]
fn the_pdf_carries_every_line_the_text_carries() {
    // The two outputs go through `render_term_parts` for exactly this reason:
    // the operator reviews the text on screen and signs the PDF, so a document
    // that said different things in each would be found by a holder, not here.
    let holder = holder_full();
    let template = TermTemplate::consignment_pt_br();
    let context = ctx(&holder);

    let text = render_term(&template, &context).unwrap();
    let (heading, lines) = render_term_parts(&template, &context).unwrap();

    assert_eq!(text, format!("{heading}\n\n{}\n", lines.join("\n")));
}

#[test]
fn a_line_omitted_for_a_missing_optional_field_is_absent_from_the_pdf_too() {
    let holder = holder_minimal();
    let (_, lines) = render_term_parts(&TermTemplate::consignment_pt_br(), &ctx(&holder)).unwrap();

    assert!(!lines.iter().any(|line| line.contains("Telefone:")));
    assert!(lines.iter().any(|line| line.contains("Bruno Costa")));
}

#[test]
fn a_pdf_term_cannot_be_issued_without_a_name_or_a_serial() {
    // The same gate as the text output. A PDF is not a way around it.
    let mut context = TermContext::sample();
    context.holder_name.clear();

    assert_eq!(
        render_term_pdf(&TermTemplate::consignment_en(), &context, "").unwrap_err(),
        TermError::Incomplete("the holder's name")
    );
}

#[test]
fn the_pdf_is_a_pdf_and_sets_the_holders_name_on_the_page() {
    let holder = holder_full();
    let bytes = render_term_pdf(
        &TermTemplate::consignment_pt_br(),
        &ctx(&holder),
        "D:20260811093000-03'00'",
    )
    .unwrap();
    let file = String::from_utf8_lossy(&bytes).to_string();

    assert!(bytes.starts_with(b"%PDF-1.7\n"));
    assert!(file.contains("Ana Silva"), "the holder is not on the page");
    assert!(file.contains("20423633"), "the serial is not on the page");
}

#[test]
fn the_pdf_footer_names_the_wording_that_produced_the_sheet() {
    // A signed sheet in a filing cabinet has to be traceable back to the exact
    // template version in the database — which is the reason the wording is
    // versioned at all.
    let holder = holder_full();
    let mut context = ctx(&holder);
    context.receipt_ref = "TERM-2026-001".into();
    let template = TermTemplate::consignment_pt_br();

    assert_eq!(
        pdf_footer(&template, &context),
        "consignment@1 (pt-BR) · #20423633 · TERM-2026-001"
    );
}

#[test]
fn a_footer_with_no_term_reference_stops_after_the_serial() {
    let holder = holder_full();
    let template = TermTemplate::consignment_en();

    assert_eq!(
        pdf_footer(&template, &ctx(&holder)),
        "consignment@1 (en) · #20423633"
    );
}

#[test]
fn a_draft_out_of_the_editor_is_marked_as_one_in_the_footer() {
    // The editor's draft has no version yet — the database assigns it on save —
    // so a PDF exported for review must not look like a stored version.
    let mut template = TermTemplate::consignment_pt_br();
    template.version = String::new();

    assert!(
        pdf_footer(&template, &TermContext::sample()).starts_with("consignment@draft (pt-BR)"),
        "{}",
        pdf_footer(&template, &TermContext::sample())
    );
}

#[test]
fn the_pdf_metadata_carries_no_personal_data() {
    // A PDF's metadata travels with the file into mail clients, previews and
    // search indexes. The body says everything the document needs to say.
    let holder = holder_full();
    let subject = pdf_subject(&TermTemplate::consignment_pt_br(), &ctx(&holder));

    assert!(!subject.contains("Ana"));
    assert!(!subject.contains("123.456.789-00"));
    assert!(!subject.contains("ana.silva@example.org"));
    assert_eq!(subject, "consignment (pt-BR) · #20423633");
}

#[test]
fn both_shipped_languages_produce_a_pdf() {
    for template in TermTemplate::builtin() {
        let bytes = render_term_pdf(&template, &TermContext::sample(), "").unwrap();
        assert!(
            bytes.starts_with(b"%PDF-1.7\n") && bytes.ends_with(b"%%EOF\n"),
            "{} did not produce a PDF",
            template.language
        );
    }
}

// ------------------------------------------------- columns in a term template

#[test]
fn a_signature_block_lines_up_whatever_length_the_name_is() {
    // The shipped wording declares column 41 three times: after the rules, after
    // the name, after the role. Substituting a name of any other length than the
    // `{{holder.name}}` placeholder used to slide the second column along with it.
    for name in [
        "Ana Silva",
        "Bruno Costa",
        "Maria da Conceição Albuquerque Fonseca",
        "Yu",
    ] {
        let holder = Holder::new(name, "h@example.org", "ESI", "").unwrap();
        let text = render_term(&TermTemplate::consignment_pt_br(), &ctx(&holder)).unwrap();

        let rules = text
            .lines()
            .find(|line| line.starts_with("______"))
            .expect("the rules line");
        let names = text
            .lines()
            .find(|line| line.starts_with(name))
            .filter(|line| line.contains("felipe"))
            .expect("the names line");
        let roles = text
            .lines()
            .find(|line| line.starts_with("Portador"))
            .expect("the roles line");

        let column = |line: &str, of: &str| line.find(of).map(|at| line[..at].chars().count());
        assert_eq!(
            column(names, "felipe"),
            column(rules, " _").map(|at| at + 1),
            "the operator's column does not match the rule above it, for `{name}`"
        );
        assert_eq!(
            column(names, "felipe"),
            column(roles, "Responsável"),
            "the operator's column does not match the role below it, for `{name}`"
        );
    }
}

#[test]
fn the_english_signature_block_lines_up_too() {
    let holder = Holder::new("Ana Silva", "h@example.org", "ESI", "").unwrap();
    let text = render_term(&TermTemplate::consignment_en(), &ctx(&holder)).unwrap();

    let names = text
        .lines()
        .find(|line| line.starts_with("Ana Silva") && line.contains("felipe"))
        .expect("the names line");
    let roles = text
        .lines()
        .find(|line| line.starts_with("Holder"))
        .expect("the roles line");

    assert_eq!(
        names.find("felipe").map(|at| names[..at].chars().count()),
        roles.find("Issuing").map(|at| roles[..at].chars().count())
    );
}

#[test]
fn a_name_too_long_for_its_column_keeps_one_space_rather_than_touching_the_next_field() {
    // The column cannot be honoured, so it degrades — but two fields must never
    // be run into one word.
    let holder = Holder::new(&"A".repeat(60), "h@example.org", "ESI", "").unwrap();
    let text = render_term(&TermTemplate::consignment_pt_br(), &ctx(&holder)).unwrap();

    let names = text
        .lines()
        .find(|line| line.contains("felipe") && line.starts_with('A'))
        .expect("the names line");
    assert!(names.contains(&format!("{} felipe", "A".repeat(60))));
}

#[test]
fn a_single_space_is_spacing_and_is_never_touched() {
    // Only a gap of two or more is a column. `{{org}} — {{org.unit}}` must keep
    // its single spaces exactly.
    let holder = holder_full();
    let text = render_term(&TermTemplate::consignment_pt_br(), &ctx(&holder)).unwrap();

    assert!(text.contains("Example Organisation — ESI"), "{text}");
}

#[test]
fn the_indentation_of_a_clause_is_left_alone() {
    // A leading gap comes before any substitution on its line, so nothing has
    // moved and there is nothing to correct. The clause continuations in the
    // shipped wording depend on that.
    let holder = holder_full();
    let text = render_term(&TermTemplate::consignment_pt_br(), &ctx(&holder)).unwrap();

    assert!(
        text.contains("\n     equivale à assinatura do portador."),
        "{text}"
    );
}

#[test]
fn a_column_is_kept_when_the_value_is_shorter_than_the_placeholder_too() {
    // Padding grows as well as shrinks: a two-character name must not pull the
    // second column left.
    let template = TermTemplate {
        id: "consignment".into(),
        language: "pt-BR".into(),
        version: "1".into(),
        title: "T".into(),
        // `{{holder.name}}` is 15 characters, so the gap targets column 25.
        body: "{{holder.name}}          {{operator}}\n0123456789012345678901234 ok\n".into(),
    };
    let holder = Holder::new("Yu", "yu@example.org", "ESI", "").unwrap();
    let text = render_term(&template, &ctx(&holder)).unwrap();

    let line = text.lines().find(|l| l.contains("felipe")).unwrap();
    assert_eq!(line.find("felipe"), Some(25));
}
