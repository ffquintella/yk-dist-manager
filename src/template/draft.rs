//! The **draft** a template is edited as, and the rules for editing it.
//!
//! A [`BootstrapTemplate`] is the stored form: parameters are a `BTreeMap`, ids
//! are settled, and every field is checked. A [`TemplateDraft`] is the same
//! procedure while somebody is typing it: parameters are the text in a text area,
//! a step can be halfway named, and nothing has been stored.
//!
//! The pair exists so the editing rules — add a step, remove one, move one, has
//! this drifted from what is on record — are plain data operations covered by
//! unit tests, instead of living in the paint code where the coverage gate cannot
//! reach them (AGENTS.md §4).

use crate::domain::StepKind;
use crate::template::{
    BootstrapTemplate, MAX_STEPS, TemplateError, TemplateStep, parse_params, unique_step_id,
};

/// One step of a draft. `params_text` is the editor's `name = value` lines; it
/// becomes a `BTreeMap` when the draft is turned back into a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepDraft {
    pub id: String,
    pub kind: StepKind,
    pub description: String,
    pub enabled: bool,
    pub required: bool,
    pub params_text: String,
}

impl StepDraft {
    fn from_step(step: &TemplateStep) -> Self {
        Self {
            id: step.id.clone(),
            kind: step.kind,
            description: step.description.clone(),
            enabled: step.enabled,
            required: step.required,
            params_text: step.params_text(),
        }
    }

    fn to_step(&self) -> Result<TemplateStep, TemplateError> {
        let id = self.id.trim().to_owned();
        Ok(TemplateStep {
            params: parse_params(&id, &self.params_text)?,
            id,
            kind: self.kind,
            description: self.description.trim().to_owned(),
            enabled: self.enabled,
            required: self.required,
        })
    }
}

/// A template being edited.
///
/// `loaded_version` is the version the draft came from, and `None` means the id
/// has nothing on record — which is what makes a save an *addition* rather than
/// an *edit*, in the status line and in the audit entry alike.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TemplateDraft {
    pub id: String,
    pub name: String,
    pub description: String,
    pub loaded_version: Option<String>,
    pub steps: Vec<StepDraft>,
}

impl TemplateDraft {
    /// Open a stored template for editing.
    pub fn from_template(template: &BootstrapTemplate, stored: bool) -> Self {
        Self {
            id: template.id.clone(),
            name: template.name.clone(),
            description: template.description.clone(),
            loaded_version: stored.then(|| template.version.clone()),
            steps: template.steps.iter().map(StepDraft::from_step).collect(),
        }
    }

    /// A new, empty template. Nothing is stored until it is saved.
    pub fn blank() -> Self {
        Self::default()
    }

    /// The draft as a template, ready to be checked and stored.
    ///
    /// The version is deliberately left as `1`: the store numbers the version
    /// from what the database holds, so two workstations editing the same
    /// template cannot both produce "version 2".
    pub fn to_template(&self) -> Result<BootstrapTemplate, TemplateError> {
        let steps = self
            .steps
            .iter()
            .map(StepDraft::to_step)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(BootstrapTemplate {
            id: self.id.trim().to_owned(),
            name: self.name.trim().to_owned(),
            version: "1".into(),
            description: self.description.trim().to_owned(),
            steps,
        })
    }

    /// The refusal a draft would get if it were saved now, for the live verdict
    /// the editor shows next to the buttons.
    pub fn check(&self) -> Result<(), TemplateError> {
        self.to_template()?.check()
    }

    /// Append a step of `kind`, with the parameters that kind reads already
    /// filled in and an id no other step uses.
    pub fn add_step(&mut self, kind: StepKind) -> Result<(), TemplateError> {
        if self.steps.len() >= MAX_STEPS {
            return Err(TemplateError::TooManySteps(MAX_STEPS));
        }
        let taken: Vec<String> = self.steps.iter().map(|s| s.id.trim().to_owned()).collect();
        let id = unique_step_id(&taken, kind);
        self.steps
            .push(StepDraft::from_step(&TemplateStep::for_kind(kind, &id)));
        Ok(())
    }

    /// Drop a step. Out-of-range is ignored rather than panicking: the index
    /// comes from a table that was painted a frame earlier.
    pub fn remove_step(&mut self, index: usize) {
        if index < self.steps.len() {
            self.steps.remove(index);
        }
    }

    /// Move a step one place earlier or later. Order is the order of execution,
    /// so this is a real edit and not a display preference.
    pub fn move_step(&mut self, index: usize, up: bool) {
        if index >= self.steps.len() {
            return;
        }
        let target = if up {
            match index.checked_sub(1) {
                Some(target) => target,
                None => return,
            }
        } else if index + 1 < self.steps.len() {
            index + 1
        } else {
            return;
        };
        self.steps.swap(index, target);
    }

    /// Has the draft moved away from the version it was loaded from?
    ///
    /// The editor asks before it opens another template, so an unsaved edit is
    /// never discarded silently. A draft with nothing on record counts as edited
    /// as soon as anything is typed.
    /// Comparison is on the *stored form*, not on the text in the boxes: writing
    /// `slot=9c` where the stored template says `slot = 9c` is not an edit, and
    /// warning about it would train the operator to ignore the warning. A draft
    /// that cannot even be turned into a template is certainly edited.
    pub fn is_dirty(&self, stored: Option<&BootstrapTemplate>) -> bool {
        match (self.to_template(), stored) {
            (Ok(draft), Some(stored)) => draft.as_version(&stored.version) != *stored,
            (Ok(draft), None) => {
                !draft.id.is_empty()
                    || !draft.name.is_empty()
                    || !draft.description.is_empty()
                    || !draft.steps.is_empty()
            }
            (Err(_), _) => true,
        }
    }
}
