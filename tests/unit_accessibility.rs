//! The accessibility invariant, expressed as a test.
//!
//! `features/gui-shell.md` phase 10 asks for "no colour-only meaning". The paint
//! code that *applies* the colour is outside the coverage gate, so the rule
//! cannot be tested where it is used — but the thing that makes it satisfiable
//! can be: **every state the UI gives a colour to also has distinct, non-empty
//! text.**
//!
//! That is the half worth pinning. A screen can be reviewed for whether it
//! displays the label; it cannot be reviewed into existence if two states share
//! a label, or one returns an empty string, because then no amount of careful
//! painting distinguishes them without hue.
//!
//! Who this is actually for: an operator with a monochrome or failing display, a
//! colour-blind operator (about one man in twelve), and anyone reading a
//! screenshot pasted into a ticket after the colours were flattened.

use std::collections::HashSet;

/// Every label in a set must be non-empty and unique.
fn distinct(what: &str, labels: &[&str]) {
    for label in labels {
        assert!(
            !label.trim().is_empty(),
            "{what}: a state with no text can only be told apart by colour"
        );
    }
    let unique: HashSet<&&str> = labels.iter().collect();
    assert_eq!(
        unique.len(),
        labels.len(),
        "{what}: two states share a label, so colour is the only thing separating them: {labels:?}"
    );
}

#[test]
fn every_key_status_has_its_own_words() {
    use yk_dist_manager::domain::KeyStatus;

    let labels: Vec<&str> = KeyStatus::ALL.iter().map(|s| s.label()).collect();
    distinct("KeyStatus", &labels);
    assert_eq!(
        KeyStatus::ALL.len(),
        6,
        "a new lifecycle state needs a label before it needs a colour"
    );
}

#[test]
fn every_transport_has_its_own_words() {
    // The plan table colours this column, and it is the one an operator most
    // needs to read correctly: whether a step goes native, through `ykman`, or
    // has to be done by hand.
    use yk_dist_manager::template::Transport;

    let labels: Vec<&str> = [Transport::Native, Transport::Ykman, Transport::Manual]
        .iter()
        .map(|t| t.label())
        .collect();
    distinct("Transport", &labels);
}

#[test]
fn every_preflight_severity_has_its_own_words() {
    use yk_dist_manager::bootstrap::Severity;

    let labels: Vec<&str> = [Severity::Skip, Severity::Warning, Severity::Blocking]
        .iter()
        .map(|s| s.label())
        .collect();
    distinct("Severity", &labels);
}

#[test]
fn the_sort_direction_is_an_arrow_rather_than_a_highlight() {
    use yk_dist_manager::browse::Direction;

    let labels: Vec<&str> = [Direction::Ascending, Direction::Descending]
        .iter()
        .map(|d| d.arrow())
        .collect();
    distinct("Direction", &labels);
}

#[test]
fn every_log_level_has_its_own_words() {
    use yk_dist_manager::logbuf::Level;

    let labels: Vec<&str> = [Level::Debug, Level::Info, Level::Warn, Level::Error]
        .iter()
        .map(|l| l.label())
        .collect();
    distinct("Level", &labels);
    assert_eq!(
        Level::Error.label(),
        "ERROR",
        "an error must not read like the rest of the list"
    );
}

#[test]
fn every_delivery_method_has_its_own_words() {
    use yk_dist_manager::domain::DeliveryMethod;

    let labels: Vec<&str> = DeliveryMethod::ALL.iter().map(|m| m.label()).collect();
    distinct("DeliveryMethod", &labels);
}

#[test]
fn every_custody_model_has_its_own_words() {
    // This one reaches a hand-over record, so it has to be readable in a
    // printed report as well as on a themed screen.
    use yk_dist_manager::domain::CustodyModel;

    let labels: Vec<&str> = CustodyModel::ALL.iter().map(|m| m.label()).collect();
    distinct("CustodyModel", &labels);
}

#[test]
fn every_password_strength_has_its_own_words() {
    // The meter is a coloured bar, which is the shape most likely to be read by
    // hue alone — so every step it can paint has to say what it is, and "too
    // weak" has to be distinguishable from "weak" in words rather than in shade.
    use yk_dist_manager::password::Strength;

    let labels: Vec<&str> = Strength::ALL.iter().map(|s| s.label()).collect();
    distinct("Strength", &labels);
    assert!(
        Strength::TooWeak.label().contains("refused"),
        "the refused step must say it is refused, not merely look redder: {}",
        Strength::TooWeak.label()
    );
}

#[test]
fn every_template_trust_state_has_its_own_words() {
    // The catalogue paints this as a badge, and two of the states are opposite
    // operational situations that must never be told apart by hue alone:
    // *unsigned* is a deployment that has not started signing, *signature does not
    // match* is a procedure that has been altered since it was signed.
    use yk_dist_manager::template::Trust;

    let states = [
        Trust::Unsigned,
        Trust::Signed { key_id: "k".into() },
        Trust::UnknownKey { key_id: "k".into() },
        Trust::Invalid {
            key_id: "k".into(),
            reason: "r".into(),
        },
        Trust::UnknownAlgorithm {
            key_id: "k".into(),
            algorithm: "a".into(),
        },
    ];
    let labels: Vec<&str> = states.iter().map(|s| s.label()).collect();
    distinct("Trust", &labels);
    assert!(
        states.iter().filter(|s| s.is_verified()).count() == 1,
        "exactly one state may run where a signature is required"
    );
}

#[test]
fn every_kind_of_template_change_has_its_own_words() {
    // The diff table colours its rows, and "moved" against "changed" is the
    // distinction that matters most: a reordered step is what made org-standard v1
    // unable to complete on hardware, and a reader who cannot separate the colours
    // still has to see it.
    use yk_dist_manager::template::Change;

    let labels: Vec<&str> = Change::ALL.iter().map(|c| c.label()).collect();
    distinct("Change", &labels);
    assert_eq!(
        Change::ALL.len(),
        5,
        "a new kind of change needs a word before it needs a colour"
    );
}

#[test]
fn what_is_attached_reads_as_a_sentence_and_not_as_an_indicator_colour() {
    // The status bar shows this beside an amber-or-green dot, and the amber one
    // means "something is attached and the application is waiting to be told which".
    // A dot cannot say that, so the words have to — and the two states an operator
    // must never confuse are "one key, get on with it" and "several, choose one".
    use yk_dist_manager::device::{Attached, DeviceInfo};

    let key = |serial: u32| DeviceInfo {
        serial,
        model: "YubiKey 5 NFC".into(),
        ..DeviceInfo::default()
    };

    let watching = |keys: Vec<DeviceInfo>| Attached {
        keys,
        polls: 1,
        ..Attached::default()
    };

    let none = watching(Vec::new());
    let one = watching(vec![key(20_423_633)]);
    let two = watching(vec![key(1), key(2)]);

    let sentences = [none.describe(), one.describe(), two.describe()];
    distinct(
        "Attached",
        &sentences.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    assert!(one.describe().contains("20423633"), "{}", one.describe());
    assert!(
        two.describe().contains("choose one"),
        "the ambiguous case has to ask for the choice in words: {}",
        two.describe()
    );

    // Before the first poll, and after the watch gives up, are their own answers —
    // reporting either as "no key attached" would be a lie the operator acts on.
    let mut looking = Attached::default();
    assert!(
        looking.describe().contains("looking"),
        "{}",
        looking.describe()
    );
    looking.stopped = Some("`ykman` was not found".into());
    assert!(
        looking.describe().contains("not watching"),
        "{}",
        looking.describe()
    );
}

#[test]
fn every_signature_state_has_its_own_words() {
    // The Distribution table paints this as a badge, and two pairs must never be
    // told apart by hue: *awaiting signature* against *overdue* (one is normal, the
    // other is a gap), and *overdue* against *returned unsigned* (one is a chase,
    // the other is permanent and nobody should waste an afternoon on it).
    use yk_dist_manager::receipt::{ReturnState, SignatureState};

    let states = [
        SignatureState::NotRequired,
        SignatureState::Signed {
            reference: String::new(),
        },
        SignatureState::Pending { days: 1 },
        SignatureState::Overdue {
            days: 20,
            threshold: 14,
        },
        SignatureState::MissingOnReturn { days: 5 },
    ];
    let labels: Vec<&str> = states.iter().map(|s| s.label()).collect();
    distinct("SignatureState", &labels);
    assert_eq!(
        states.iter().filter(|s| s.needs_chasing()).count(),
        2,
        "only the two live states are worth chasing"
    );

    let returns = [
        ReturnState::Held,
        ReturnState::Documented,
        ReturnState::Undocumented,
    ];
    let labels: Vec<&str> = returns.iter().map(|s| s.label()).collect();
    distinct("ReturnState", &labels);
}

#[test]
fn the_status_line_severity_is_derived_from_the_text_not_from_the_caller() {
    // `status::classify` reads the message, so the words and the colour cannot
    // disagree — the colour is a function of the text rather than a second,
    // independently-set signal that could contradict it.
    use yk_dist_manager::status::{Severity, classify};

    assert_eq!(classify("AUDIT FAILURE: chain broken"), Severity::Alarm);
    assert_ne!(classify("backup written to /tmp/x"), Severity::Alarm);
}

/// No sentence an operator reads has a hole punched through the middle of it.
///
/// The defect this guards, found in seven shipped strings at once: a Rust string
/// literal continued with a trailing `\` keeps the newline out *and* eats the
/// next line's indentation, so
///
/// ```text
/// "the register is on \
///  the file server"
/// ```
///
/// reads as one space. Any tool that rewrites the file without honouring that
/// escape — a heredoc, a templating pass, a language whose own strings treat
/// `\`-newline differently — turns it into `"the register is on
/// the file server"` with eighteen spaces in the middle, which is what the
/// operator then reads on screen.
///
/// It is invisible in review: the diff looks like a reflow, the code compiles,
/// every test that asserts `contains("the file server")` still passes, and
/// nothing but the rendered sentence is wrong. So it is checked at the source,
/// which is the only place it exists.
///
/// Here rather than in its own binary because it is the same invariant as the
/// rest of this file — text an operator depends on has to say what it means
/// without help — and a forty-fourth test binary is not worth one function.
#[test]
fn no_operator_facing_sentence_has_a_collapsed_line_continuation() {
    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders: Vec<String> = Vec::new();

    fn walk(directory: &std::path::Path, offenders: &mut Vec<String>) {
        for entry in std::fs::read_dir(directory).expect("the source tree is readable") {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, offenders);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("a source file is readable");
            for (at, line) in source.lines().enumerate() {
                if let Some(run) = collapsed_run(line) {
                    offenders.push(format!(
                        "{}:{} — {run}",
                        path.file_name().unwrap().to_string_lossy(),
                        at + 1
                    ));
                }
            }
        }
    }

    walk(&source_root, &mut offenders);
    offenders.sort();

    assert!(
        offenders.is_empty(),
        "these lines have a run of spaces in the middle of a sentence, which is what a \
         collapsed `\\`-continuation looks like — rewrite the literal so each fragment ends \
         in ` \\` and the next begins one column past the opening quote: {offenders:#?}"
    );
}

/// The guard above can fail, and does not fire on what this repository does on
/// purpose.
///
/// A source scan that only ever passes is indistinguishable from no scan, and the
/// cost of a false positive is somebody deleting the test.
///
/// Every fixture here is **assembled rather than written out**, and that is not
/// fussiness: the first draft of this test spelled them as ordinary continued
/// literals, and the tool that wrote the file collapsed them exactly as it had
/// collapsed the seven strings in `src/` — so the fixture for the *legitimate*
/// case arrived containing the defect, and the guard caught its own test. A hole
/// built out of `" ".repeat(n)` cannot be reflowed into or out of existence.
#[test]
fn the_collapsed_continuation_guard_knows_a_hole_from_an_indent() {
    let hole = " ".repeat(18);
    let quote = '"';

    // `src/ui/database.rs` as the defect left it: one sentence, one crater.
    let collapsed = format!(
        "            {quote}The register itself is on the file server and is intact — this \
         workstation simply{hole}cannot reach it. Nothing was written while it was gone.{quote},"
    );
    assert!(
        collapsed_run(&collapsed).is_some(),
        "the guard cannot see the defect it exists for"
    );

    // The same sentence, repaired: long, but every gap is one space.
    let repaired = collapsed.replace(&hole, " ");
    assert_eq!(
        collapsed_run(&repaired),
        None,
        "a repaired sentence must not still be reported: {repaired}"
    );

    // The sealed-envelope slip. Over the limit, with a run of spaces, and
    // correct — because the run is an indent after an explicit newline.
    let slip = format!(
        "            {quote}4. If the key is lost or you think somebody else has used it, report \
         it,\\n   immediately, to {{}}.{quote},"
    );
    assert!(
        slip.chars().count() > 100,
        "the fixture must reach the check"
    );
    assert_eq!(
        collapsed_run(&slip),
        None,
        "an indent after an explicit newline is the point of the indent: {slip}"
    );

    // The `--help` column alignment: a deliberate run, inside the line limit.
    let aligned = "  YKDM_LOG                     Log filter, e.g. `debug`";
    assert_eq!(
        collapsed_run(aligned),
        None,
        "the --help alignment fits the line limit, which is why the limit is a condition"
    );
}

/// The run of spaces that betrays a collapsed continuation, if the line has one.
///
/// Three conditions together, because each on its own has honest counter-examples
/// in this repository:
///
/// * **Over the 100-column limit.** `rustfmt` cannot break a string literal, so a
///   collapsed one is always long; every deliberate run of spaces here — the
///   `--help` and `--diagnose` column alignment, the named-pipe diagram in
///   `device::helper` — fits within the limit like the rest of the file.
/// * **Not a comment.** A doc comment's hanging indents in a list are deliberate
///   and `rustfmt` leaves them alone.
/// * **Not preceded by `\n`.** [`crate`-side] the sealed-envelope slip indents a
///   wrapped clause with `"…,\n   report it"`, which is the one legitimate run of
///   spaces inside a long literal: it is *after* an explicit newline, where an
///   indent is the point.
fn collapsed_run(line: &str) -> Option<String> {
    if line.trim_start().starts_with("//") || line.chars().count() <= 100 {
        return None;
    }
    let bytes = line.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b' ' {
            at += 1;
            continue;
        }
        let start = at;
        while at < bytes.len() && bytes[at] == b' ' {
            at += 1;
        }
        let long_enough = at - start >= 3;
        let after_text = start >= 1 && {
            let before = bytes[start - 1];
            before.is_ascii_alphanumeric() || matches!(before, b',' | b'.' | b':' | b';' | b')')
        };
        let after_newline_escape = start >= 2 && &bytes[start - 2..start] == b"\\n";
        let text_follows = at < bytes.len() && bytes[at].is_ascii_alphanumeric();
        if long_enough && after_text && text_follows && !after_newline_escape {
            let from = start.saturating_sub(24);
            return Some(format!("…{}…", &line[from..(at + 12).min(line.len())]));
        }
    }
    None
}
