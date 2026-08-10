use anyhow::{anyhow, Result};

use super::{
    agents::{agent_from_name, picker_agents, SkillAgentArg},
    paths::PathContext,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSelectionSource {
    Explicit,
    All,
    Picker,
    Detected,
    Fallback,
}

#[derive(Debug, Clone)]
pub struct SkillAgentSelection {
    pub agents: Vec<SkillAgentArg>,
    pub source: SkillSelectionSource,
}

pub fn explicit_agent_selection(
    agents: &[SkillAgentArg],
    all_agents: bool,
) -> Option<SkillAgentSelection> {
    if all_agents {
        Some(SkillAgentSelection {
            agents: SkillAgentArg::ALL.to_vec(),
            source: SkillSelectionSource::All,
        })
    } else if agents.is_empty() {
        None
    } else {
        Some(SkillAgentSelection {
            agents: dedupe_agents(agents.iter().copied()),
            source: SkillSelectionSource::Explicit,
        })
    }
}

pub fn detected_agents(context: &PathContext) -> Vec<SkillAgentArg> {
    picker_agents()
        .iter()
        .copied()
        .filter(|agent| context.agent_detected(*agent))
        .collect()
}

pub fn default_noninteractive_agents(
    context: &PathContext,
) -> (Vec<SkillAgentArg>, SkillSelectionSource) {
    let mut agents = vec![SkillAgentArg::Universal];
    let detected_specific = detected_agents(context)
        .into_iter()
        .filter(|agent| agent.needs_agent_specific_default())
        .collect::<Vec<_>>();
    let source = if detected_specific.is_empty() {
        SkillSelectionSource::Fallback
    } else {
        agents.extend(detected_specific);
        SkillSelectionSource::Detected
    };
    (agents, source)
}

pub fn default_agent_selection(context: &PathContext) -> SkillAgentSelection {
    let (agents, source) = default_noninteractive_agents(context);
    SkillAgentSelection { agents, source }
}

pub fn picker_agent_selection(agents: Vec<SkillAgentArg>) -> SkillAgentSelection {
    SkillAgentSelection {
        agents: dedupe_agents(agents),
        source: SkillSelectionSource::Picker,
    }
}

pub fn parse_picker_selection(
    input: &str,
    options: &[SkillAgentArg],
) -> Result<Vec<SkillAgentArg>> {
    let input = input.trim();
    if input.eq_ignore_ascii_case("all") {
        return Ok(options.to_vec());
    }
    let mut selected = Vec::new();
    for raw in input
        .split([',', ' ', '\t'])
        .filter(|part| !part.trim().is_empty())
    {
        let token = raw.trim();
        let agent = if let Ok(index) = token.parse::<usize>() {
            options
                .get(index.saturating_sub(1))
                .copied()
                .ok_or_else(|| anyhow!("invalid selection {token}: choose 1-{}", options.len()))?
        } else {
            agent_from_name(token).ok_or_else(|| anyhow!("unknown agent: {token}"))?
        };
        if !selected.contains(&agent) {
            selected.push(agent);
        }
    }
    if selected.is_empty() {
        return Err(anyhow!("choose at least one install target"));
    }
    Ok(selected)
}

pub fn dedupe_agents(agents: impl IntoIterator<Item = SkillAgentArg>) -> Vec<SkillAgentArg> {
    let mut deduped = Vec::new();
    for agent in agents {
        if !deduped.contains(&agent) {
            deduped.push(agent);
        }
    }
    deduped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_parsing_is_ordered_and_deduplicated() {
        assert_eq!(
            parse_picker_selection("codex,1,codex", picker_agents()).unwrap(),
            vec![SkillAgentArg::Codex, SkillAgentArg::Universal]
        );
    }
}
