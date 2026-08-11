//! A minimal PDF writer for the documents this tool hands to a person to sign.
//!
//! The consignment term is the artefact that survives an audit
//! (`features/consignment-terms.md`). It has to be printable, signable and
//! filable, which means a PDF — and it has to be producible on a workstation
//! that has nothing installed but this application.
//!
//! So: **no TeX, no Typst, no subprocess, and no new dependency.** This module
//! writes the PDF itself, and it can be this small because of one deliberate
//! restriction:
//!
//! * The document is **monospaced text**, set in **Courier** — one of the
//!   fourteen fonts every PDF viewer is required to have. Nothing is embedded,
//!   which is what removes the font-parsing half of a PDF writer.
//! * Courier is also the *right* font here rather than a compromise: the term's
//!   layout — the numbered clauses, the two side-by-side signature rules — is
//!   built out of spaces in the template, and only a fixed-width font keeps it.
//!
//! What that restriction costs, stated plainly:
//!
//! * **Encoding.** Courier is used with `WinAnsiEncoding` (CP1252), which covers
//!   Portuguese, Spanish, English, French, German and Italian completely,
//!   including the em dash. A character outside it becomes `?` — see
//!   [`unrepresentable`], which the GUI calls so the operator is *told* rather
//!   than handed a document full of question marks. A term in a language CP1252
//!   cannot set needs an embedded font, which is a separate piece of work.
//! * **Typography.** One size, one weight for the heading, no justification.
//!   A term that says the correct things beats a beautifully typeset one that
//!   does not.
//!
//! The output is also **uncompressed and entirely ASCII**: bytes outside
//! printable ASCII are written as octal escapes. A term is a few kilobytes
//! either way, and an archival artefact that `grep` can read is worth more than
//! one that is 3 KB smaller.
//!
//! Nothing here reads the clock. [`TextDocument::created`] is filled by the
//! caller from [`pdf_date`], so [`render`] is a pure function of its input and
//! a test can assert on every byte it produces.

use std::fmt::Write as _;

/// A4 width in points (72 per inch).
pub const PAGE_WIDTH: f64 = 595.28;
/// A4 height in points.
pub const PAGE_HEIGHT: f64 = 841.89;
/// Margin on all four sides — 2 cm, which is what a filing punch needs.
pub const MARGIN: f64 = 56.7;
/// Body size. Small enough that the term's 80-odd column lines fit A4 without
/// wrapping, which is the whole reason the text layout survives.
pub const FONT_SIZE: f64 = 9.0;
/// Heading size.
pub const HEADING_SIZE: f64 = 11.0;
/// Baseline-to-baseline distance.
pub const LEADING: f64 = 11.7;
/// Footer size.
pub const FOOTER_SIZE: f64 = 7.0;
/// Every glyph in Courier advances 600/1000 of the em. That single fact is what
/// lets this module compute a column count and centre a heading without reading
/// a font file.
const ADVANCE: f64 = 0.6;
/// Space kept above the bottom margin so the last body line cannot land on the
/// footer.
const FOOTER_GAP: f64 = 12.0;
/// Baseline of the footer, inside the bottom margin.
const FOOTER_BASELINE: f64 = 30.0;
/// Grey of the footer. The footer is metadata, not part of the undertaking.
const FOOTER_GREY: f64 = 0.45;
/// Rows the last page must carry, or the page break above it is moved up.
///
/// A term ends with its signature block: two rules, the names under them, the
/// roles under those. A break through that block leaves a holder signing a sheet
/// that does not say what they are signing, and it is what a term one line too
/// long for a page does by default. Rather than ask a template to mark the block
/// — which the wording's owner would have to remember — the last page is simply
/// never allowed to be nearly empty.
const MIN_LAST_PAGE: usize = 6;

/// What a character the encoding cannot carry is printed as.
pub const REPLACEMENT: u8 = b'?';

/// How many characters of body text fit on a line.
pub fn columns() -> usize {
    ((PAGE_WIDTH - 2.0 * MARGIN) / (FONT_SIZE * ADVANCE)) as usize
}

/// How many characters of the heading fit on a line.
pub fn heading_columns() -> usize {
    ((PAGE_WIDTH - 2.0 * MARGIN) / (HEADING_SIZE * ADVANCE)) as usize
}

/// How many lines fit on a page, footer excluded.
pub fn lines_per_page() -> usize {
    let first = PAGE_HEIGHT - MARGIN - FONT_SIZE;
    let last = MARGIN + FOOTER_GAP;
    ((first - last) / LEADING) as usize + 1
}

/// A document to write: a heading, body lines, and the metadata a filed PDF
/// should carry.
///
/// `lines` are already rendered — line omission for an absent optional field has
/// happened before this point ([`crate::term::render_term_parts`]), so this
/// module never decides what a term says, only how it is set on a page.
#[derive(Debug, Clone, Default)]
pub struct TextDocument {
    /// Printed bold and centred at the top of the first page, and used as the
    /// PDF `/Title`.
    pub heading: String,
    /// The body, one entry per line. Long lines are wrapped by [`wrap`].
    pub lines: Vec<String>,
    /// PDF `/Author` — the organisation issuing the document.
    pub author: String,
    /// PDF `/Subject`. Keep personal data out of it: the metadata of a file that
    /// gets mailed around is not the place for the holder's name, which the body
    /// already carries.
    pub subject: String,
    /// Printed on every page, left-hand side, with `n / total` on the right.
    pub footer: String,
    /// PDF `/CreationDate`, from [`pdf_date`]. Empty omits the key.
    pub created: String,
}

/// A `/CreationDate` value: `D:YYYYMMDDHHmmSS+HH'mm'`.
///
/// Takes the instant rather than reading the clock, so that [`render`] stays a
/// pure function and the tests do not depend on when they run.
pub fn pdf_date<Tz: chrono::TimeZone>(when: &chrono::DateTime<Tz>) -> String {
    use chrono::Offset as _;

    let offset = when.offset().fix().local_minus_utc();
    let sign = if offset < 0 { '-' } else { '+' };
    let (hours, minutes) = (offset.abs() / 3600, offset.abs() % 3600 / 60);
    format!(
        "D:{}{sign}{hours:02}'{minutes:02}'",
        when.naive_local().format("%Y%m%d%H%M%S")
    )
}

/// One line of a laid-out page.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    /// The document heading: bold, centred.
    Heading(String),
    /// A line of the body, or a blank one.
    Body(String),
}

/// Write the document. Infallible: an over-long line wraps and a character the
/// encoding cannot carry becomes [`REPLACEMENT`], so there is no input for which
/// this cannot produce a page.
pub fn render(doc: &TextDocument) -> Vec<u8> {
    let pages = layout(doc);

    // Object numbers are fixed by the layout, so every reference can be written
    // before the object it points at exists.
    const CATALOG: usize = 1;
    const PAGES: usize = 2;
    const FONT_BODY: usize = 3;
    const FONT_HEADING: usize = 4;
    const INFO: usize = 5;
    let page_object = |index: usize| 6 + index * 2;
    let content_object = |index: usize| 7 + index * 2;
    let size = 6 + pages.len() * 2;

    let mut out: Vec<u8> = Vec::new();
    // The binary comment marks the file as binary for tools that transfer it.
    out.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");

    // Byte offset of each object, indexed by object number — the cross-reference
    // table at the end is built from this.
    let mut offsets: Vec<usize> = vec![0; size];
    fn object(out: &mut Vec<u8>, offsets: &mut [usize], number: usize, body: &[u8]) {
        offsets[number] = out.len();
        out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }

    let kids: Vec<String> = (0..pages.len())
        .map(|index| format!("{} 0 R", page_object(index)))
        .collect();

    object(
        &mut out,
        &mut offsets,
        CATALOG,
        format!("<< /Type /Catalog /Pages {PAGES} 0 R >>").as_bytes(),
    );
    object(
        &mut out,
        &mut offsets,
        PAGES,
        format!(
            "<< /Type /Pages /Count {} /Kids [{}] >>",
            pages.len(),
            kids.join(" ")
        )
        .as_bytes(),
    );
    object(&mut out, &mut offsets, FONT_BODY, font_object("Courier"));
    object(
        &mut out,
        &mut offsets,
        FONT_HEADING,
        font_object("Courier-Bold"),
    );
    object(&mut out, &mut offsets, INFO, info_object(doc).as_bytes());

    for (index, rows) in pages.iter().enumerate() {
        object(
            &mut out,
            &mut offsets,
            page_object(index),
            format!(
                "<< /Type /Page /Parent {PAGES} 0 R /MediaBox [0 0 {PAGE_WIDTH:.2} \
                 {PAGE_HEIGHT:.2}] /Resources << /Font << /F1 {FONT_BODY} 0 R /F2 \
                 {FONT_HEADING} 0 R >> >> /Contents {} 0 R >>",
                content_object(index)
            )
            .as_bytes(),
        );

        let stream = content_stream(rows, &doc.footer, index + 1, pages.len());
        let mut body = format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes();
        body.extend_from_slice(stream.as_bytes());
        body.extend_from_slice(b"endstream");
        object(&mut out, &mut offsets, content_object(index), &body);
    }

    // Cross-reference table: one 20-byte entry per object, the free entry first.
    let startxref = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f\r\n");
    for offset in offsets.iter().skip(1) {
        out.extend_from_slice(format!("{offset:010} 00000 n\r\n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {size} /Root {CATALOG} 0 R /Info {INFO} 0 R >>\n\
             startxref\n{startxref}\n%%EOF\n"
        )
        .as_bytes(),
    );

    out
}

fn font_object(base: &str) -> &[u8] {
    match base {
        "Courier-Bold" => {
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier-Bold /Encoding /WinAnsiEncoding >>"
        }
        _ => b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding >>",
    }
}

/// The document information dictionary.
///
/// Its strings are written as UTF-16BE hex, which any character survives — the
/// CP1252 restriction applies to what Courier can *set on the page*, and there
/// is no reason to inflict it on the metadata as well.
fn info_object(doc: &TextDocument) -> String {
    let mut out = String::from("<<");
    for (key, value) in [
        ("Title", doc.heading.trim()),
        ("Author", doc.author.trim()),
        ("Subject", doc.subject.trim()),
        (
            "Creator",
            concat!("yk-dist-manager ", env!("CARGO_PKG_VERSION")),
        ),
        (
            "Producer",
            concat!("yk-dist-manager ", env!("CARGO_PKG_VERSION")),
        ),
    ] {
        if !value.is_empty() {
            let _ = write!(out, " /{key} {}", utf16_string(value));
        }
    }
    if !doc.created.trim().is_empty() {
        let _ = write!(out, " /CreationDate ({})", escape(doc.created.trim()));
    }
    out.push_str(" >>");
    out
}

/// A PDF text string as UTF-16BE hex with a byte-order mark.
fn utf16_string(text: &str) -> String {
    let mut out = String::from("<FEFF");
    for unit in text.encode_utf16() {
        let _ = write!(out, "{unit:04X}");
    }
    out.push('>');
    out
}

/// Wrap the document into pages of [`Row`]s. The heading takes its own row (or
/// rows, if it is long) plus two blank ones, so page 1 holds correspondingly
/// fewer body lines.
fn layout(doc: &TextDocument) -> Vec<Vec<Row>> {
    let mut rows: Vec<Row> = Vec::new();
    if !doc.heading.trim().is_empty() {
        rows.extend(
            wrap(doc.heading.trim(), heading_columns())
                .into_iter()
                .map(Row::Heading),
        );
        rows.push(Row::Body(String::new()));
        rows.push(Row::Body(String::new()));
    }
    for line in &doc.lines {
        rows.extend(wrap(line, columns()).into_iter().map(Row::Body));
    }

    let mut pages: Vec<Vec<Row>> = rows.chunks(lines_per_page()).map(<[Row]>::to_vec).collect();
    // A document with nothing in it is still a page, not an unopenable file.
    if pages.is_empty() {
        pages.push(Vec::new());
    }
    keep_the_last_page_from_being_a_stub(&mut pages);
    pages
}

/// Move the final page break up until the last page carries [`MIN_LAST_PAGE`]
/// rows — see the constant for why a term makes this necessary.
///
/// Only the last break moves. The signature block is at the end of the document,
/// which is where a stub page does real harm; a break in the middle of clause 4
/// is merely a page break.
fn keep_the_last_page_from_being_a_stub(pages: &mut [Vec<Row>]) {
    if pages.len() < 2 {
        return;
    }
    let last = pages.len() - 1;
    let wanted = MIN_LAST_PAGE.saturating_sub(pages[last].len());
    // Never rob the page above to the point of making *it* the stub.
    let spare = pages[last - 1].len().saturating_sub(MIN_LAST_PAGE);
    let moved = wanted.min(spare);
    if moved == 0 {
        return;
    }

    let tail = {
        let previous = &mut pages[last - 1];
        previous.split_off(previous.len() - moved)
    };
    pages[last].splice(0..0, tail);
}

/// The content stream of one page.
fn content_stream(rows: &[Row], footer: &str, page: usize, total: usize) -> String {
    let mut out = String::new();
    let mut y = PAGE_HEIGHT - MARGIN - FONT_SIZE;

    for row in rows {
        match row {
            Row::Heading(text) => {
                let width = text.chars().count() as f64 * HEADING_SIZE * ADVANCE;
                let x = ((PAGE_WIDTH - width) / 2.0).max(MARGIN);
                out.push_str(&text_at("F2", HEADING_SIZE, x, y, text, None));
            }
            // A blank line costs a row of height and no operators.
            Row::Body(text) if text.trim().is_empty() => {}
            Row::Body(text) => out.push_str(&text_at("F1", FONT_SIZE, MARGIN, y, text, None)),
        }
        y -= LEADING;
    }

    if !footer.trim().is_empty() {
        out.push_str(&text_at(
            "F1",
            FOOTER_SIZE,
            MARGIN,
            FOOTER_BASELINE,
            footer.trim(),
            Some(FOOTER_GREY),
        ));
    }
    // Page numbers as `n / total` with no word in front: the term is in the
    // holder's language, and "Page" would be in the wrong one.
    let number = format!("{page} / {total}");
    let width = number.chars().count() as f64 * FOOTER_SIZE * ADVANCE;
    out.push_str(&text_at(
        "F1",
        FOOTER_SIZE,
        PAGE_WIDTH - MARGIN - width,
        FOOTER_BASELINE,
        &number,
        Some(FOOTER_GREY),
    ));

    out
}

/// One text-showing operator sequence, positioned absolutely.
fn text_at(font: &str, size: f64, x: f64, y: f64, text: &str, grey: Option<f64>) -> String {
    let mut out = String::from("BT\n");
    if let Some(grey) = grey {
        let _ = writeln!(out, "{grey:.2} {grey:.2} {grey:.2} rg");
    }
    let _ = writeln!(out, "/{font} {size:.2} Tf");
    let _ = writeln!(out, "1 0 0 1 {x:.2} {y:.2} Tm");
    let _ = writeln!(out, "({}) Tj", escape(text));
    out.push_str("ET\n");
    out
}

/// Wrap one line to `columns`, breaking at the last space that fits.
///
/// A continuation line keeps the indentation of the line it came from, so a
/// wrapped clause stays aligned under its own number rather than under the
/// margin. A word longer than the line is split, because a line that overflows
/// the page loses text where a split one does not.
pub fn wrap(line: &str, columns: usize) -> Vec<String> {
    // Tabs would break every width calculation in this module, and a term
    // template that contains one meant a space.
    let line = line.replace('\t', "    ");
    let line = line.trim_end();
    if columns == 0 || line.chars().count() <= columns {
        return vec![line.to_owned()];
    }

    let indent: String = line.chars().take_while(|c| *c == ' ').collect();
    // An indent that takes half the line leaves nowhere to wrap into, so the
    // line is set flush left rather than in a two-character column. The term's
    // own clauses indent by five, so this is a guard, not a behaviour.
    let indent = if indent.chars().count() * 2 >= columns {
        String::new()
    } else {
        indent
    };
    let budget = columns - indent.chars().count();

    let mut out = Vec::new();
    let mut rest = line.trim_start();
    while !rest.is_empty() {
        let chars: Vec<(usize, char)> = rest.char_indices().collect();
        if chars.len() <= budget {
            out.push(format!("{indent}{rest}"));
            break;
        }
        // The last space that still fits; a hard split when there is none.
        let cut = chars[..=budget]
            .iter()
            .rposition(|(_, c)| *c == ' ')
            .filter(|&at| at > 0)
            .unwrap_or(budget);
        out.push(format!("{indent}{}", rest[..chars[cut].0].trim_end()));
        rest = rest[chars[cut].0..].trim_start();
    }
    out
}

/// A string as it appears inside a PDF content stream: the CP1252 bytes, with
/// `\`, `(`, `)` and everything outside printable ASCII written as an escape, so
/// the file stays readable in a text editor.
pub fn escape(text: &str) -> String {
    let mut out = String::new();
    for byte in winansi(text) {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'(' => out.push_str("\\("),
            b')' => out.push_str("\\)"),
            0x20..=0x7E => out.push(byte as char),
            other => {
                let _ = write!(out, "\\{other:03o}");
            }
        }
    }
    out
}

/// The text in `WinAnsiEncoding` — the encoding the fonts in this module
/// declare. A character it cannot carry becomes [`REPLACEMENT`]; ask
/// [`unrepresentable`] first if you need to warn about that.
pub fn winansi(text: &str) -> Vec<u8> {
    text.chars().map(winansi_byte).collect()
}

/// Characters in `text` that [`winansi`] cannot carry, in order of first
/// appearance and without repeats.
///
/// The GUI calls this before offering a PDF, so that a term in a language
/// CP1252 cannot set is *reported* rather than silently exported full of
/// question marks.
pub fn unrepresentable(text: &str) -> Vec<char> {
    let mut found: Vec<char> = Vec::new();
    for c in text.chars() {
        // A newline is a line break, not a character to set.
        if c == '\n' || c == '\r' || c == '\t' {
            continue;
        }
        if winansi_byte(c) == REPLACEMENT && c != '?' && !found.contains(&c) {
            found.push(c);
        }
    }
    found
}

fn winansi_byte(c: char) -> u8 {
    let code = c as u32;
    match c {
        // A line of a document holds no control characters; whitespace that got
        // this far becomes a space rather than a corrupt stream.
        '\t' | '\n' | '\r' => b' ',
        // CP1252 fills 0x80–0x9F, where ISO 8859-1 has controls, with the
        // typographic characters a term actually uses — the em dash above all.
        '\u{20AC}' => 0x80,
        '\u{201A}' => 0x82,
        '\u{0192}' => 0x83,
        '\u{201E}' => 0x84,
        '\u{2026}' => 0x85,
        '\u{2020}' => 0x86,
        '\u{2021}' => 0x87,
        '\u{02C6}' => 0x88,
        '\u{2030}' => 0x89,
        '\u{0160}' => 0x8A,
        '\u{2039}' => 0x8B,
        '\u{0152}' => 0x8C,
        '\u{017D}' => 0x8E,
        '\u{2018}' => 0x91,
        '\u{2019}' => 0x92,
        '\u{201C}' => 0x93,
        '\u{201D}' => 0x94,
        '\u{2022}' => 0x95,
        '\u{2013}' => 0x96,
        '\u{2014}' => 0x97,
        '\u{02DC}' => 0x98,
        '\u{2122}' => 0x99,
        '\u{0161}' => 0x9A,
        '\u{203A}' => 0x9B,
        '\u{0153}' => 0x9C,
        '\u{017E}' => 0x9E,
        '\u{0178}' => 0x9F,
        // Printable ASCII, then Latin-1, both one to one.
        _ if (0x20..0x7F).contains(&code) => code as u8,
        _ if (0xA0..=0xFF).contains(&code) => code as u8,
        // Anything else needs an embedded font, not a substitution.
        _ => REPLACEMENT,
    }
}
