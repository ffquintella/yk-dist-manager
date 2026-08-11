//! Unit tests for the PDF writer: the file structure a viewer needs, the
//! encoding, the wrapping and the pagination.
//!
//! The point of these is that the writer is hand-rolled — there is no library
//! between this code and the bytes a viewer parses, so the structural invariants
//! (a cross-reference entry per object, pointing at the object it claims to) are
//! asserted here rather than trusted.

use yk_dist_manager::pdf::{
    self, REPLACEMENT, TextDocument, columns, escape, lines_per_page, render, unrepresentable,
    winansi, wrap,
};

fn doc(lines: &[&str]) -> TextDocument {
    TextDocument {
        heading: "Termo de Consignação".into(),
        lines: lines.iter().map(|l| (*l).to_owned()).collect(),
        author: "Organização de Exemplo".into(),
        subject: "consignment (pt-BR) · #20423633".into(),
        footer: "consignment@1 (pt-BR) · #20423633".into(),
        created: "D:20260811093000-03'00'".into(),
    }
}

/// The file as a string, for the tests that read its structure.
///
/// The only non-ASCII in a rendered document is the four-byte binary marker on
/// line 2. It is substituted byte for byte, so every offset in the string is
/// still the offset in the file — which is what the cross-reference tests check.
fn text(bytes: &[u8]) -> String {
    let ascii: Vec<u8> = bytes
        .iter()
        .map(|b| if b.is_ascii() { *b } else { b'.' })
        .collect();
    String::from_utf8(ascii).unwrap()
}

/// The content stream of each page, in page order.
fn pages(bytes: &[u8]) -> Vec<String> {
    let file = text(bytes);
    let mut out = Vec::new();
    let mut rest = file.as_str();
    // `endstream` ends with `stream`, so the delimiter has to carry the newline
    // that precedes the real one.
    while let Some(at) = rest.find("\nstream\n") {
        let start = at + "\nstream\n".len();
        let end = rest[start..].find("endstream").unwrap();
        out.push(rest[start..start + end].to_owned());
        rest = &rest[start + end..];
    }
    out
}

// ------------------------------------------------------------ file structure

#[test]
fn a_document_is_a_pdf_file() {
    let bytes = render(&doc(&["Nome: Ana Silva"]));

    assert!(bytes.starts_with(b"%PDF-1.7\n"));
    assert!(bytes.ends_with(b"%%EOF\n"));
}

#[test]
fn the_whole_file_is_ascii_so_it_stays_readable() {
    // Every byte outside printable ASCII is written as an escape. An archival
    // artefact that `grep` can read is worth more than a smaller one.
    let bytes = render(&doc(&["Endereço: Praia de Botafogo — 190"]));

    let non_ascii: Vec<u8> = bytes
        .iter()
        .copied()
        .filter(|b| !b.is_ascii() && *b != 0xE2 && *b != 0xE3 && *b != 0xCF && *b != 0xD3)
        .collect();
    assert!(
        non_ascii.is_empty(),
        "unexpected non-ASCII bytes: {non_ascii:?}"
    );
}

#[test]
fn the_cross_reference_table_points_at_every_object() {
    let bytes = render(&doc(&["Nome: Ana Silva"]));
    let file = text(&bytes);

    // `startxref` names the offset of the table; the table names the offset of
    // each object. Both have to be true or a viewer refuses the file.
    let startxref: usize = file
        .rsplit("startxref\n")
        .next()
        .unwrap()
        .split('\n')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(&file[startxref..startxref + 4], "xref");

    let table = &file[startxref..];
    let size: usize = table
        .lines()
        .nth(1)
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();

    // Lines 0 and 1 are `xref` and the range; line 2 is the free head, so the
    // entry for object n is line n + 2.
    for number in 1..size {
        let entry = table.lines().nth(number + 2).unwrap();
        let offset: usize = entry.split_whitespace().next().unwrap().parse().unwrap();
        assert!(
            file[offset..].starts_with(&format!("{number} 0 obj")),
            "entry {number} points at {offset}, which is not `{number} 0 obj`"
        );
    }
}

#[test]
fn every_cross_reference_entry_is_the_twenty_bytes_the_format_requires() {
    // A fixed-width table is the whole reason a viewer can seek into it.
    let file = text(&render(&doc(&["Nome: Ana Silva"])));
    let table = &file[file.find("xref\n").unwrap()..];
    let size: usize = table
        .lines()
        .nth(1)
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();

    let head = table.find(" 65535 f").unwrap() - 10;
    let entries = &table[head..table.find("trailer").unwrap()];
    assert_eq!(entries.len(), size * 20);
    for entry in entries.as_bytes().chunks(20) {
        assert!(
            entry.ends_with(b" n\r\n") || entry.ends_with(b" f\r\n"),
            "malformed entry `{}`",
            String::from_utf8_lossy(entry)
        );
    }
}

#[test]
fn the_declared_stream_length_is_the_real_one() {
    // A viewer reads exactly /Length bytes of the stream. One byte out and the
    // page is either truncated or fed the object trailer as content.
    let file = text(&render(&doc(&["Nome: Ana Silva", "Série: 20423633"])));

    let declared: usize = file
        .split("<< /Length ")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let start = file.find("\nstream\n").unwrap() + "\nstream\n".len();
    let actual = file[start..].find("endstream").unwrap();
    assert_eq!(declared, actual);
}

#[test]
fn the_fonts_are_the_two_standard_ones_and_nothing_is_embedded() {
    // Not embedding is what keeps this writer small — and Courier is one of the
    // fourteen fonts every viewer must have, so nothing is missing at the far end.
    let file = text(&render(&doc(&["Nome: Ana Silva"])));

    assert!(file.contains("/BaseFont /Courier /Encoding /WinAnsiEncoding"));
    assert!(file.contains("/BaseFont /Courier-Bold /Encoding /WinAnsiEncoding"));
    assert!(!file.contains("/FontFile"));
}

#[test]
fn the_heading_is_set_in_the_bold_font_and_the_body_in_the_regular_one() {
    let file = text(&render(&doc(&["Nome: Ana Silva"])));

    assert!(file.contains("/F2 11.00 Tf"), "heading is not bold");
    assert!(file.contains("/F1 9.00 Tf"), "body is not the regular font");
}

#[test]
fn a_document_with_no_body_still_produces_a_page() {
    let mut document = doc(&[]);
    document.lines.clear();
    let file = text(&render(&document));

    assert!(file.contains("/Count 1"));
}

#[test]
fn a_document_with_nothing_at_all_is_still_an_openable_file() {
    let bytes = render(&TextDocument::default());

    assert!(bytes.starts_with(b"%PDF-1.7\n"));
    assert!(bytes.ends_with(b"%%EOF\n"));
    assert!(text(&bytes).contains("/Count 1"));
}

// ------------------------------------------------------------------ metadata

#[test]
fn the_creation_date_comes_from_the_caller_not_from_the_clock() {
    // `render` has to be a pure function of its input, or none of these tests
    // could assert on the bytes it produces.
    let file = text(&render(&doc(&["Nome: Ana Silva"])));

    assert!(file.contains("/CreationDate (D:20260811093000-03'00')"));
}

#[test]
fn a_creation_date_is_formatted_the_way_the_format_wants_it() {
    let when = chrono::DateTime::parse_from_rfc3339("2026-08-11T09:30:00-03:00").unwrap();

    assert_eq!(pdf::pdf_date(&when), "D:20260811093000-03'00'");
}

#[test]
fn a_creation_date_east_of_greenwich_keeps_its_sign() {
    let when = chrono::DateTime::parse_from_rfc3339("2026-08-11T09:30:00+05:30").unwrap();

    assert_eq!(pdf::pdf_date(&when), "D:20260811093000+05'30'");
}

#[test]
fn metadata_strings_survive_any_character_because_they_are_utf16() {
    // The CP1252 restriction is about what Courier can set on a page. There is
    // no reason to inflict it on the title in a mail client's preview as well.
    let mut document = doc(&["Nome: Ana Silva"]);
    document.heading = "T".into();
    let file = text(&render(&document));

    assert!(file.contains("/Title <FEFF0054>"));
}

#[test]
fn an_empty_metadata_field_is_left_out_rather_than_written_blank() {
    let mut document = doc(&["Nome: Ana Silva"]);
    document.author.clear();
    document.subject = "   ".into();
    let file = text(&render(&document));

    assert!(!file.contains("/Author"));
    assert!(!file.contains("/Subject"));
}

// ------------------------------------------------------------------ encoding

#[test]
fn an_accented_name_is_written_in_the_encoding_the_font_declares() {
    // ç is 0xE7 in WinAnsi, written as the octal escape \347.
    let file = text(&render(&doc(&["Nome: Ana Gonçalves"])));

    assert!(file.contains("Gon\\347alves"), "ç was not encoded");
}

#[test]
fn an_em_dash_survives_because_the_encoding_is_cp1252_not_latin1() {
    // The em dash is the one character in the built-in wording that ISO 8859-1
    // does not have and CP1252 does, at 0x97.
    assert_eq!(winansi("a — b"), vec![b'a', b' ', 0x97, b' ', b'b']);
}

#[test]
fn the_characters_the_built_in_wording_uses_are_all_representable() {
    // If this fails, the shipped pt-BR or en term would print with `?` in it.
    let template = yk_dist_manager::term::TermTemplate::consignment_pt_br();
    let missing = unrepresentable(&format!("{} {}", template.title, template.body));

    assert!(missing.is_empty(), "cannot be set in the PDF: {missing:?}");
}

#[test]
fn a_character_the_encoding_cannot_carry_becomes_a_question_mark() {
    let encoded = winansi("秘密鍵");

    assert_eq!(encoded, vec![REPLACEMENT, REPLACEMENT, REPLACEMENT]);
}

#[test]
fn a_character_the_encoding_cannot_carry_is_reported_before_it_is_printed() {
    // The operator has to be told, not handed a document full of `?`.
    let missing = unrepresentable("Nome: 山田 太郎 — ESI");

    assert_eq!(missing, vec!['山', '田', '太', '郎']);
}

#[test]
fn a_question_mark_the_document_really_contains_is_not_reported_as_missing() {
    assert!(unrepresentable("Perdeu a chave? Comunique imediatamente.").is_empty());
}

#[test]
fn line_breaks_are_not_reported_as_unrepresentable() {
    assert!(unrepresentable("Nome: Ana\nUnidade: ESI\r\n").is_empty());
}

#[test]
fn parentheses_and_backslashes_are_escaped_so_the_stream_stays_valid() {
    // An unescaped `)` in a name would terminate the string early and corrupt
    // every operator after it on the page.
    assert_eq!(escape("Ana (Silva) \\ Costa"), "Ana \\(Silva\\) \\\\ Costa");
}

#[test]
fn a_control_character_never_reaches_the_content_stream() {
    let escaped = escape("Ana\u{7}Silva");

    assert_eq!(escaped, "Ana?Silva");
}

#[test]
fn a_stray_line_break_inside_a_line_becomes_a_space() {
    assert_eq!(escape("Ana\tSilva"), "Ana Silva");
}

// ------------------------------------------------------------------ wrapping

#[test]
fn a_line_that_fits_is_left_exactly_as_it_is() {
    assert_eq!(wrap("Nome: Ana Silva", 40), vec!["Nome: Ana Silva"]);
}

#[test]
fn a_long_line_wraps_at_the_last_space_that_fits() {
    let wrapped = wrap("aaaa bbbb cccc dddd", 12);

    assert_eq!(wrapped, vec!["aaaa bbbb", "cccc dddd"]);
}

#[test]
fn a_continuation_line_keeps_the_indentation_of_the_line_it_came_from() {
    // The numbered clauses of the term are indented by hand in the template; a
    // continuation that started at the margin would read as a new clause.
    let wrapped = wrap("     alpha beta gamma delta", 16);

    assert_eq!(wrapped, vec!["     alpha beta", "     gamma delta"]);
}

#[test]
fn a_word_longer_than_the_line_is_split_rather_than_overflowing_the_page() {
    // Overflow loses text off the edge of the paper; a split does not.
    let wrapped = wrap("aaaaaaaaaaaaaaa", 6);

    assert_eq!(wrapped, vec!["aaaaaa", "aaaaaa", "aaa"]);
}

#[test]
fn an_indent_too_deep_to_wrap_into_is_dropped_rather_than_squeezing_the_text() {
    // 20 spaces of a 22-column line would leave a two-character column. The
    // term's own clauses indent by five, so this is a guard, not a behaviour.
    let wrapped = wrap("                    alpha beta gamma delta epsilon", 22);

    assert!(wrapped.len() > 1);
    assert!(
        wrapped.iter().all(|line| !line.starts_with(' ')),
        "the deep indent was kept: {wrapped:?}"
    );
}

#[test]
fn wrapping_never_loses_a_word_and_never_exceeds_the_line() {
    let line = "4.3. A perda, o furto, o extravio ou a suspeita de comprometimento devem ser \
                comunicados imediatamente.";
    let words: Vec<&str> = line.split_whitespace().collect();

    // From the longest word in the line up to the real page width. Below that a
    // word has to be hard-split, which is its own test.
    for columns in 16..=columns() {
        let wrapped = wrap(line, columns);
        assert_eq!(
            wrapped.join(" ").split_whitespace().collect::<Vec<_>>(),
            words,
            "words were lost at {columns} columns"
        );
        for wrapped_line in &wrapped {
            assert!(
                wrapped_line.chars().count() <= columns,
                "`{wrapped_line}` is longer than {columns} columns"
            );
        }
    }
}

#[test]
fn trailing_whitespace_is_trimmed_so_it_cannot_force_a_wrap() {
    assert_eq!(wrap("Nome: Ana Silva      ", 16), vec!["Nome: Ana Silva"]);
}

// ---------------------------------------------------------------- pagination

#[test]
fn an_a4_page_holds_the_term_at_its_natural_width() {
    // The built-in wording is written to about 80 columns. If the page held
    // fewer, every clause of a shipped term would wrap.
    assert!(
        columns() >= 80,
        "only {} columns fit — the term would wrap",
        columns()
    );
    assert!(lines_per_page() >= 50);
}

#[test]
fn a_short_term_is_one_page() {
    let file = text(&render(&doc(&["Nome: Ana Silva"])));

    assert!(file.contains("/Count 1"));
}

#[test]
fn a_term_longer_than_a_page_paginates() {
    let long: Vec<String> = (0..lines_per_page() * 2 + 5)
        .map(|n| format!("linha {n}"))
        .collect();
    let mut document = doc(&[]);
    document.lines = long;

    let file = text(&render(&document));
    assert!(file.contains("/Count 3"), "expected three pages");
    // One page object and one content stream each, plus catalog, pages, two
    // fonts and the info dictionary.
    assert!(file.contains("/Size 12"));
}

#[test]
fn every_page_carries_the_footer_and_says_which_page_it_is() {
    let long: Vec<String> = (0..lines_per_page() + 5)
        .map(|n| format!("linha {n}"))
        .collect();
    let mut document = doc(&[]);
    document.lines = long;

    let file = text(&render(&document));
    assert_eq!(file.matches("consignment@1 \\(pt-BR\\)").count(), 2);
    assert!(file.contains("(1 / 2) Tj"));
    assert!(file.contains("(2 / 2) Tj"));
}

#[test]
fn a_page_break_never_drops_a_line() {
    let lines: Vec<String> = (0..lines_per_page() * 2)
        .map(|n| format!("linha{n}"))
        .collect();
    let mut document = doc(&[]);
    document.lines = lines.clone();

    let file = text(&render(&document));
    for line in &lines {
        assert!(file.contains(&format!("({line}) Tj")), "{line} is missing");
    }
}

#[test]
fn the_heading_takes_room_on_the_first_page_and_is_not_repeated() {
    let long: Vec<String> = (0..lines_per_page() * 2)
        .map(|n| format!("linha {n}"))
        .collect();
    let mut document = doc(&[]);
    document.lines = long;

    let file = text(&render(&document));
    assert_eq!(file.matches("Consigna\\347\\343o) Tj").count(), 1);
}

#[test]
fn nothing_is_drawn_below_the_bottom_margin() {
    let long: Vec<String> = (0..lines_per_page())
        .map(|n| format!("linha {n}"))
        .collect();
    let mut document = doc(&[]);
    document.lines = long;
    let file = text(&render(&document));

    // Every text matrix on the page, and none of the body ones may sit in the
    // footer band.
    let baselines: Vec<f64> = file
        .split("1 0 0 1 ")
        .skip(1)
        .map(|rest| {
            rest.split(" Tm")
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .parse()
                .unwrap()
        })
        .collect();
    assert!(baselines.iter().all(|y| *y >= 30.0), "{baselines:?}");
}

#[test]
fn the_last_page_is_never_a_stub_carrying_a_line_or_two() {
    // A term one line too long for a page would otherwise put the signature
    // rules on page 1 and the names under them on page 2.
    let lines: Vec<String> = (0..lines_per_page() + 1)
        .map(|n| format!("linha{n}"))
        .collect();
    let mut document = doc(&[]);
    document.heading.clear();
    document.lines = lines.clone();

    let rendered = render(&document);
    assert!(text(&rendered).contains("/Count 2"));

    // The last six lines of the document all sit on the second page.
    let page_two = pages(&rendered).pop().unwrap();
    for line in lines.iter().rev().take(6) {
        assert!(
            page_two.contains(&format!("({line}) Tj")),
            "{line} was left on the page before"
        );
    }
}

#[test]
fn a_page_before_the_last_is_never_robbed_into_a_stub_itself() {
    // Two rows in total: there is nothing to move, and moving anyway would
    // produce an empty page.
    let mut document = doc(&[]);
    document.heading.clear();
    document.lines = vec!["um".into(), "dois".into()];

    let file = text(&render(&document));
    assert!(file.contains("/Count 1"));
}

#[test]
fn the_shipped_term_keeps_its_signature_block_together() {
    // The regression this rule exists for, on the wording that actually ships.
    let template = yk_dist_manager::term::TermTemplate::consignment_pt_br();
    let sample = yk_dist_manager::term::TermContext::sample();
    let bytes = yk_dist_manager::term::render_term_pdf(&template, &sample, "").unwrap();

    let last_page = pages(&bytes).pop().unwrap();
    assert!(
        last_page.contains("Ana Exemplo da Silva"),
        "the names are not on the page that carries the rules"
    );
    assert!(
        last_page.contains("_______"),
        "the rules are not on the page that carries the names"
    );
    assert!(last_page.contains("Portador"));
}
