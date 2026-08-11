//! Bundled Agent Skill selection, installation, and status workflows.

mod install;
mod selection;

pub use install::{
    force_install_command, install, status, SkillInstallOutcome, SkillStatusOutcome,
};
pub use selection::{
    complete_picker_selection, plan_install_selection, status_selection, SkillInstallSelectionPlan,
    SkillPickerOption, SkillPickerPrompt, SkillSelectionRequest,
};

#[cfg(test)]
mod tests;
