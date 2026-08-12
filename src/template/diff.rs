//! What changed between two versions of a template
//! (`features/bootstrap-templates.md` phase 6).
//!
//! The question this answers is the spec's own: *"what changed since the batch we
//! shipped in June?"* — asked by somebody holding a key that was prepared months
//! ago, with a register that faithfully records `org-standard v2` and no way to
//! see how v2 differs from the v4 the wizard offers today.
//!
//! ## Why a purpose-built diff and not a text diff
//!
//! A line diff of two JSON bodies would answer a different question. It would
//! report the version number as a change (it always is), it would report a
//! reordered `BTreeMap` as a change (it never is), and it would say nothing about
//! the thing that matters most here: **a step that moved**. Order is the order of
//! execution in this feature — `org-standard` v1 could not complete on hardware
//! precisely because two steps were in the wrong order — so "step `fido-credential`
//! now runs before `fido-force-pin-change`" is the single most important sentence a
//! diff of two procedures can produce, and it is one a text diff renders as four
//! unrelated added and removed lines.
//!
//! So the comparison is structural: steps are matched **by id**, and each is
//! reported as added, removed, moved, or changed field by field.
//!
//! ## Every line says what kind of change it is, in words
//!
//! `gui-shell` phase 10: no colour-only meaning. [`Change::label`] is the word,
//! the colour is decoration, and `tests/unit_accessibility.rs` asserts the words
//! are distinct — a diff read from a screenshot with the colours flattened, or by
//! an operator who cannot separate red from green, has to say the same thing.

use std::collections::BTreeSet;

use crate::template::BootstrapTemplate;

/// What kind of change a line reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    /// Present in the newer version only.
    Added,
    /// Present in the older version only.
    Removed,
    /// The same step, a different position in the order of execution.
    Moved,
    /// The same step, a different field.
    Changed,
    /// Neither version differs here; shown for orientation.
    Same,
}

impl Change {
    /// The word. Never only a colour — see the module documentation.
    pub fn label(&self) -> &'static str {
        match self {
            Change::Added => "added",
            Change::Removed => "removed",
            Change::Moved => "moved",
            Change::Changed => "changed",
            Change::Same => "unchanged",
        }
    }

    pub const ALL: [Change; 5] = [
        Change::Added,
        Change::Removed,
        Change::Moved,
        Change::Changed,
        Change::Same,
    ];
}

/// One row of a rendered diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub change: Change,
    /// What changed: `step fido-pin`, `description`, `parameter min_length`.
    pub what: String,
    /// The older value, empty when there was none.
    pub before: String,
    /// The newer value, empty when there is none.
    pub after: String,
}

impl DiffLine {
    /// One line of text, for a status message, a log line or a copy-paste into a
    /// ticket. The screen paints the same content as a table.
    pub fn to_text(&self) -> String {
        match (self.before.is_empty(), self.after.is_empty()) {
            (true, true) => format!("{}: {}", self.change.label(), self.what),
            (true, false) => format!("{}: {} = {}", self.change.label(), self.what, self.after),
            (false, true) => format!(
                "{}: {} (was {})",
                self.change.label(),
                self.what,
                self.before
            ),
            (false, false) => format!(
                "{}: {} — {} -> {}",
                self.change.label(),
                self.what,
                self.before,
                self.after
            ),
        }
    }
}

/// The difference between two versions of a procedure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateDiff {
    /// The versions compared, older first — as given, not sorted here.
    pub from: String,
    pub to: String,
    pub lines: Vec<DiffLine>,
}

impl TemplateDiff {
    /// Do the two versions describe the same procedure?
    ///
    /// True for two versions that differ only in their number, which happens
    /// legitimately: a template saved again with no edit, or the same procedure
    /// imported into a database that numbered it differently.
    pub fn is_identical(&self) -> bool {
        self.lines.iter().all(|line| line.change == Change::Same)
    }

    /// Lines that report an actual change.
    pub fn changes(&self) -> impl Iterator<Item = &DiffLine> {
        self.lines.iter().filter(|line| line.change != Change::Same)
    }

    /// How many of each, for the one-line summary.
    pub fn count(&self, change: Change) -> usize {
        self.lines
            .iter()
            .filter(|line| line.change == change)
            .count()
    }

    /// "3 changed, 1 added, 1 moved" — or that they are the same procedure.
    pub fn summary(&self) -> String {
        if self.is_identical() {
            return format!(
                "versions {} and {} describe the same procedure — only the version number differs",
                self.from, self.to
            );
        }
        let parts: Vec<String> = [
            Change::Added,
            Change::Removed,
            Change::Moved,
            Change::Changed,
        ]
        .iter()
        .filter_map(|change| {
            let n = self.count(*change);
            (n > 0).then(|| format!("{n} {}", change.label()))
        })
        .collect();
        format!("{} -> {}: {}", self.from, self.to, parts.join(", "))
    }
}

/// Compare two versions of a procedure, step by step.
///
/// `before` and `after` are whichever two versions the operator picked; nothing
/// here assumes one is newer, because the numbers are the only ordering available
/// and comparing v4 against v2 is a perfectly reasonable thing to ask for.
///
/// The version number itself is never reported as a change: it is the *question*,
/// not part of the answer.
pub fn diff(before: &BootstrapTemplate, after: &BootstrapTemplate) -> TemplateDiff {
    let mut lines = Vec::new();

    field(&mut lines, "name", &before.name, &after.name);
    field(
        &mut lines,
        "description",
        &before.description,
        &after.description,
    );
    // The id is compared because comparing across ids is possible (an operator
    // picks two templates rather than two versions of one) and a diff that hid it
    // would be describing something other than what was asked for.
    field(&mut lines, "id", &before.id, &after.id);

    let before_ids: Vec<&str> = before.steps.iter().map(|s| s.id.as_str()).collect();
    let after_ids: Vec<&str> = after.steps.iter().map(|s| s.id.as_str()).collect();

    // Every step id in either version, in the newer version's order first so the
    // diff reads in the order the procedure will run.
    let mut order: Vec<&str> = after_ids.clone();
    for id in &before_ids {
        if !order.contains(id) {
            order.push(id);
        }
    }

    for id in order {
        let old = before.steps.iter().find(|s| s.id == id);
        let new = after.steps.iter().find(|s| s.id == id);
        match (old, new) {
            (None, Some(step)) => lines.push(DiffLine {
                change: Change::Added,
                what: format!("step `{id}`"),
                before: String::new(),
                after: format!("{} — {}", step.kind.slug(), step.description),
            }),
            (Some(step), None) => lines.push(DiffLine {
                change: Change::Removed,
                what: format!("step `{id}`"),
                before: format!("{} — {}", step.kind.slug(), step.description),
                after: String::new(),
            }),
            (Some(old), Some(new)) => {
                let old_at = before_ids.iter().position(|s| *s == id);
                let new_at = after_ids.iter().position(|s| *s == id);
                // Position is reported in the operator's counting, from 1, and only
                // when it changed *and* the step is otherwise recognisable — a step
                // that moved because something before it was removed has still
                // moved, which is what a reader needs to know.
                if old_at != new_at
                    && let (Some(old_at), Some(new_at)) = (old_at, new_at)
                {
                    lines.push(DiffLine {
                        change: Change::Moved,
                        what: format!("step `{id}`"),
                        before: format!("position {}", old_at + 1),
                        after: format!("position {}", new_at + 1),
                    });
                }

                if old.kind != new.kind {
                    lines.push(step_change(id, "kind", old.kind.slug(), new.kind.slug()));
                }
                if old.description != new.description {
                    lines.push(step_change(
                        id,
                        "description",
                        &old.description,
                        &new.description,
                    ));
                }
                if old.enabled != new.enabled {
                    lines.push(step_change(
                        id,
                        "enabled",
                        yes(old.enabled),
                        yes(new.enabled),
                    ));
                }
                if old.required != new.required {
                    lines.push(step_change(
                        id,
                        "required",
                        yes(old.required),
                        yes(new.required),
                    ));
                }

                let keys: BTreeSet<&String> = old.params.keys().chain(new.params.keys()).collect();
                for key in keys {
                    let was = old.params.get(key).map(String::as_str);
                    let now = new.params.get(key).map(String::as_str);
                    match (was, now) {
                        (Some(was), Some(now)) if was != now => {
                            lines.push(step_change(id, &format!("parameter `{key}`"), was, now))
                        }
                        (None, Some(now)) => lines.push(DiffLine {
                            change: Change::Added,
                            what: format!("step `{id}` parameter `{key}`"),
                            before: String::new(),
                            after: now.to_owned(),
                        }),
                        (Some(was), None) => lines.push(DiffLine {
                            change: Change::Removed,
                            what: format!("step `{id}` parameter `{key}`"),
                            before: was.to_owned(),
                            after: String::new(),
                        }),
                        _ => {}
                    }
                }
            }
            (None, None) => unreachable!("the id came from one of the two step lists"),
        }
    }

    if lines.is_empty() {
        lines.push(DiffLine {
            change: Change::Same,
            what: format!("{} step(s), unchanged", after.steps.len()),
            before: String::new(),
            after: String::new(),
        });
    }

    TemplateDiff {
        from: before.version.clone(),
        to: after.version.clone(),
        lines,
    }
}

fn field(lines: &mut Vec<DiffLine>, what: &str, before: &str, after: &str) {
    if before.trim() != after.trim() {
        lines.push(DiffLine {
            change: Change::Changed,
            what: what.to_owned(),
            before: before.trim().to_owned(),
            after: after.trim().to_owned(),
        });
    }
}

fn step_change(step: &str, what: &str, before: &str, after: &str) -> DiffLine {
    DiffLine {
        change: Change::Changed,
        what: format!("step `{step}` {what}"),
        before: before.to_owned(),
        after: after.to_owned(),
    }
}

fn yes(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::StepKind;
    use crate::template::TemplateStep;

    fn base() -> BootstrapTemplate {
        BootstrapTemplate {
            id: "t".into(),
            name: "T".into(),
            version: "1".into(),
            description: "First".into(),
            steps: vec![
                TemplateStep::new("pin", StepKind::Fido2Pin, "Set the PIN")
                    .with_param("min_length", "6"),
                TemplateStep::new("cred", StepKind::Fido2Credential, "Register")
                    .with_param("rp_id", "{{org}}"),
            ],
            signature: None,
        }
    }

    #[test]
    fn the_same_procedure_under_two_numbers_is_reported_as_identical() {
        // Not a curiosity: an import renumbers, and a save with no edit is
        // legitimate. "Nothing changed" has to be a first-class answer, or the
        // screen invents differences to justify itself.
        let a = base();
        let b = a.as_version("4");
        let d = diff(&a, &b);
        assert!(d.is_identical());
        assert_eq!(d.changes().count(), 0);
        assert!(d.summary().contains("same procedure"), "{}", d.summary());
    }

    #[test]
    fn a_reordered_step_is_reported_as_moved_not_as_a_removal_and_an_addition() {
        // The failure this feature exists to make visible: org-standard v1 could
        // not complete on hardware because two FIDO2 steps ran in the wrong order.
        // A diff that reported that as "removed, added" would bury it.
        let a = base();
        let mut b = a.as_version("2");
        b.steps.swap(0, 1);

        let d = diff(&a, &b);
        assert_eq!(d.count(Change::Moved), 2, "{:#?}", d.lines);
        assert_eq!(d.count(Change::Added), 0);
        assert_eq!(d.count(Change::Removed), 0);

        let moved = d
            .lines
            .iter()
            .find(|l| l.what.contains("cred"))
            .expect("the credential step moved");
        assert_eq!(moved.before, "position 2");
        assert_eq!(moved.after, "position 1");
        assert!(moved.to_text().contains("moved"), "{}", moved.to_text());
    }

    #[test]
    fn a_changed_parameter_names_the_step_the_parameter_and_both_values() {
        // What an operator actually needs: not "the template changed" but "the
        // minimum PIN length went from 6 to 8 in step pin".
        let a = base();
        let mut b = a.as_version("2");
        b.steps[0].params.insert("min_length".into(), "8".into());

        let d = diff(&a, &b);
        let line = d.changes().next().expect("one change");
        assert_eq!(line.change, Change::Changed);
        assert!(line.what.contains("pin"), "{}", line.what);
        assert!(line.what.contains("min_length"), "{}", line.what);
        assert_eq!(line.before, "6");
        assert_eq!(line.after, "8");
    }

    #[test]
    fn an_added_and_a_removed_parameter_are_told_apart() {
        let a = base();
        let mut b = a.as_version("2");
        b.steps[0].params.remove("min_length");
        b.steps[0]
            .params
            .insert("source".into(), "generated".into());

        let d = diff(&a, &b);
        assert_eq!(d.count(Change::Added), 1, "{:#?}", d.lines);
        assert_eq!(d.count(Change::Removed), 1, "{:#?}", d.lines);
        let removed = d
            .lines
            .iter()
            .find(|l| l.change == Change::Removed)
            .unwrap();
        assert!(removed.to_text().contains("was 6"), "{}", removed.to_text());
    }

    #[test]
    fn an_added_step_and_a_removed_step_are_each_reported_once() {
        let a = base();
        let mut b = a.as_version("2");
        b.steps.remove(1);
        b.steps.push(TemplateStep::new(
            "verify",
            StepKind::Verify,
            "Read it back",
        ));

        let d = diff(&a, &b);
        assert_eq!(d.count(Change::Added), 1, "{:#?}", d.lines);
        assert_eq!(d.count(Change::Removed), 1, "{:#?}", d.lines);
        assert!(
            d.lines
                .iter()
                .any(|l| l.change == Change::Added && l.after.contains("verify")),
            "{:#?}",
            d.lines
        );
    }

    #[test]
    fn a_disabled_step_is_a_change_and_reads_as_words() {
        let a = base();
        let mut b = a.as_version("2");
        b.steps[1].enabled = false;
        b.steps[1].required = false;

        let d = diff(&a, &b);
        let texts: Vec<String> = d.changes().map(DiffLine::to_text).collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains("enabled") && t.contains("yes -> no")),
            "{texts:#?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t.contains("required") && t.contains("yes -> no")),
            "{texts:#?}"
        );
    }

    #[test]
    fn the_header_fields_are_compared_but_the_version_never_is() {
        let a = base();
        let mut b = a.as_version("9");
        b.name = "Renamed".into();
        b.description = "Second".into();

        let d = diff(&a, &b);
        assert_eq!(d.count(Change::Changed), 2, "{:#?}", d.lines);
        assert!(
            !d.lines.iter().any(|l| l.what == "version"),
            "the version is the question, not part of the answer"
        );
        assert_eq!(d.from, "1");
        assert_eq!(d.to, "9");
    }

    #[test]
    fn comparing_two_different_templates_reports_the_id() {
        // Allowed on purpose: "how does our fido-only procedure differ from the
        // standard one" is a real question, and an id change that went unmentioned
        // would make the answer misleading.
        let a = base();
        let mut b = a.clone();
        b.id = "other".into();
        let d = diff(&a, &b);
        assert!(d.lines.iter().any(|l| l.what == "id"), "{:#?}", d.lines);
    }

    #[test]
    fn the_summary_counts_every_kind_of_change() {
        let a = base();
        let mut b = a.as_version("2");
        b.name = "Renamed".into();
        b.steps.swap(0, 1);
        b.steps.push(TemplateStep::new(
            "verify",
            StepKind::Verify,
            "Read it back",
        ));

        let summary = diff(&a, &b).summary();
        assert!(summary.starts_with("1 -> 2:"), "{summary}");
        assert!(summary.contains("added"), "{summary}");
        assert!(summary.contains("moved"), "{summary}");
        assert!(summary.contains("changed"), "{summary}");
    }

    #[test]
    fn every_change_kind_has_its_own_word() {
        let labels: BTreeSet<&str> = Change::ALL.iter().map(|c| c.label()).collect();
        assert_eq!(labels.len(), Change::ALL.len(), "{labels:?}");
    }
}
