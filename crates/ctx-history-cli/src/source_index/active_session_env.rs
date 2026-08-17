use ctx_history_read_application::ActiveSessionExclusion;

#[derive(Clone, Copy)]
struct Marker {
    name: &'static str,
    value: &'static str,
}

#[derive(Clone, Copy)]
enum RuleGate {
    SessionOnly,
    All(&'static [Marker]),
    Goose,
    Mux,
}

#[derive(Clone, Copy)]
struct ProviderRule {
    provider: &'static str,
    session: &'static str,
    gate: RuleGate,
}

const DEEPSEEK_MARKERS: &[Marker] = &[Marker {
    name: "DSH_SHELL",
    value: "1",
}];

const PI_MARKERS: &[Marker] = &[
    Marker {
        name: "AI_AGENT",
        value: "pi",
    },
    Marker {
        name: "PI_CODING_AGENT",
        value: "true",
    },
];

const HERMES_MARKERS: &[Marker] = &[
    Marker {
        name: "AI_AGENT",
        value: "hermes-agent",
    },
    Marker {
        name: "HERMES_AGENT",
        value: "true",
    },
];

const GOOSE_MARKERS: &[Marker] = &[
    Marker {
        name: "AGENT",
        value: "goose",
    },
    Marker {
        name: "GOOSE_TERMINAL",
        value: "1",
    },
];

const MUX_RUNTIMES: &[&str] = &["local", "worktree", "ssh", "docker", "devcontainer"];

const PROVIDER_RULES: &[ProviderRule] = &[
    ProviderRule {
        provider: "codex",
        session: "CODEX_THREAD_ID",
        gate: RuleGate::SessionOnly,
    },
    ProviderRule {
        provider: "deepseek_harness",
        session: "DSH_SESSION_ID",
        gate: RuleGate::All(DEEPSEEK_MARKERS),
    },
    ProviderRule {
        provider: "grok_build",
        session: "GROK_SESSION_ID",
        gate: RuleGate::SessionOnly,
    },
    ProviderRule {
        provider: "pi",
        session: "PI_SESSION_ID",
        gate: RuleGate::All(PI_MARKERS),
    },
    ProviderRule {
        provider: "claude",
        session: "CLAUDE_CODE_SESSION_ID",
        gate: RuleGate::SessionOnly,
    },
    ProviderRule {
        provider: "goose",
        session: "AGENT_SESSION_ID",
        gate: RuleGate::Goose,
    },
    ProviderRule {
        provider: "hermes",
        session: "HERMES_SESSION_ID",
        gate: RuleGate::All(HERMES_MARKERS),
    },
    ProviderRule {
        provider: "shelley",
        session: "SHELLEY_CONVERSATION_ID",
        gate: RuleGate::SessionOnly,
    },
    ProviderRule {
        provider: "qwen_code",
        session: "QWEN_CODE_SESSION_ID",
        gate: RuleGate::All(&[Marker {
            name: "QWEN_CODE",
            value: "1",
        }]),
    },
    ProviderRule {
        provider: "mux",
        session: "MUX_WORKSPACE_ID",
        gate: RuleGate::Mux,
    },
];

/// Resolve the active session from the process environment.
pub(crate) fn detected_active_session() -> Option<ActiveSessionExclusion> {
    active_session_exclusion_with_lookup(|name| std::env::var(name).ok())
}

/// Resolve the active session using an injected environment lookup.
pub(crate) fn active_session_exclusion_with_lookup<F>(lookup: F) -> Option<ActiveSessionExclusion>
where
    F: FnMut(&str) -> Option<String>,
{
    resolve_with_rules(lookup, PROVIDER_RULES)
}

fn resolve_with_rules<F>(mut lookup: F, rules: &[ProviderRule]) -> Option<ActiveSessionExclusion>
where
    F: FnMut(&str) -> Option<String>,
{
    let mut candidates = Vec::new();

    for rule in rules {
        let Some(provider_session_id) = lookup(rule.session).map(|value| value.trim().to_owned())
        else {
            continue;
        };
        if provider_session_id.is_empty() || !rule_matches(rule.gate, &mut lookup) {
            continue;
        }

        let candidate = ActiveSessionExclusion {
            provider: rule.provider.to_owned(),
            provider_session_id,
        };
        if !candidates.iter().any(|existing| existing == &candidate) {
            candidates.push(candidate);
        }
    }

    match candidates.as_slice() {
        [candidate] => Some(candidate.clone()),
        _ => None,
    }
}

fn rule_matches<F>(gate: RuleGate, lookup: &mut F) -> bool
where
    F: FnMut(&str) -> Option<String>,
{
    match gate {
        RuleGate::SessionOnly => true,
        RuleGate::All(markers) => markers
            .iter()
            .all(|marker| lookup(marker.name).as_deref() == Some(marker.value)),
        RuleGate::Goose => {
            let present = GOOSE_MARKERS
                .iter()
                .filter_map(|marker| lookup(marker.name).map(|value| (marker, value)))
                .collect::<Vec<_>>();
            !present.is_empty()
                && present
                    .iter()
                    .all(|(marker, value)| value.as_str() == marker.value)
        }
        RuleGate::Mux => lookup("MUX_RUNTIME")
            .as_deref()
            .is_some_and(|runtime| MUX_RUNTIMES.contains(&runtime)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn env(values: &[(&str, &str)]) -> BTreeMap<String, String> {
        values
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn resolve(values: &[(&str, &str)]) -> Option<ActiveSessionExclusion> {
        let values = env(values);
        active_session_exclusion_with_lookup(|name| values.get(name).cloned())
    }

    fn expected(provider: &str, session: &str) -> ActiveSessionExclusion {
        ActiveSessionExclusion {
            provider: provider.to_owned(),
            provider_session_id: session.to_owned(),
        }
    }

    #[test]
    fn resolves_codex_and_trims_only_the_session_id() {
        assert_eq!(
            resolve(&[("CODEX_THREAD_ID", "  id/with  spaces/Ü  ")]),
            Some(expected("codex", "id/with  spaces/Ü"))
        );
        assert_eq!(resolve(&[("CODEX_THREAD_ID", "   ")]), None);
    }

    #[test]
    fn resolves_deepseek_harness_only_with_exact_shell_marker() {
        let base = &[("DSH_SESSION_ID", "deepseek-session"), ("DSH_SHELL", "1")];
        assert_eq!(
            resolve(base),
            Some(expected("deepseek_harness", "deepseek-session"))
        );
        assert_eq!(
            resolve(&[("DSH_SESSION_ID", "deepseek-session"), ("DSH_SHELL", "1 ")]),
            None
        );
        assert_eq!(
            resolve(&[("DSH_SESSION_ID", "deepseek-session"), ("dsh_shell", "1")]),
            None
        );
    }

    #[test]
    fn resolves_grok_build_from_its_session_variable() {
        assert_eq!(
            resolve(&[("GROK_SESSION_ID", " grok-session ")]),
            Some(expected("grok_build", "grok-session"))
        );
    }

    #[test]
    fn resolves_pi_only_with_both_exact_markers() {
        let base = &[
            ("PI_SESSION_ID", "pi-session"),
            ("AI_AGENT", "pi"),
            ("PI_CODING_AGENT", "true"),
        ];
        assert_eq!(resolve(base), Some(expected("pi", "pi-session")));
        assert_eq!(
            resolve(&[("PI_SESSION_ID", "pi-session"), ("AI_AGENT", "pi")]),
            None
        );
        assert_eq!(
            resolve(&[
                ("PI_SESSION_ID", "pi-session"),
                ("AI_AGENT", "PI"),
                ("PI_CODING_AGENT", "true"),
            ]),
            None
        );
    }

    #[test]
    fn resolves_claude_code_from_its_session_variable() {
        assert_eq!(
            resolve(&[("CLAUDE_CODE_SESSION_ID", " claude-session ")]),
            Some(expected("claude", "claude-session"))
        );
    }

    #[test]
    fn resolves_goose_with_one_or_both_valid_markers() {
        assert_eq!(
            resolve(&[("AGENT_SESSION_ID", "goose-session"), ("AGENT", "goose")]),
            Some(expected("goose", "goose-session"))
        );
        assert_eq!(
            resolve(&[
                ("AGENT_SESSION_ID", "goose-session"),
                ("GOOSE_TERMINAL", "1"),
            ]),
            Some(expected("goose", "goose-session"))
        );
        assert_eq!(
            resolve(&[
                ("AGENT_SESSION_ID", "goose-session"),
                ("AGENT", "goose"),
                ("GOOSE_TERMINAL", "1"),
            ]),
            Some(expected("goose", "goose-session"))
        );
    }

    #[test]
    fn goose_requires_a_session_and_rejects_any_invalid_present_marker() {
        assert_eq!(resolve(&[("AGENT", "goose")]), None);
        assert_eq!(resolve(&[("AGENT_SESSION_ID", "goose-session")]), None);
        assert_eq!(
            resolve(&[("AGENT_SESSION_ID", "goose-session"), ("AGENT", "GOOSE")]),
            None
        );
        assert_eq!(
            resolve(&[
                ("AGENT_SESSION_ID", "goose-session"),
                ("AGENT", "goose"),
                ("GOOSE_TERMINAL", "0"),
            ]),
            None
        );
        assert_eq!(
            resolve(&[
                ("AGENT_SESSION_ID", "goose-session"),
                ("AGENT", "other"),
                ("GOOSE_TERMINAL", "1"),
            ]),
            None
        );
    }

    #[test]
    fn resolves_hermes_only_with_both_exact_markers() {
        let base = &[
            ("HERMES_SESSION_ID", " hermes-session "),
            ("AI_AGENT", "hermes-agent"),
            ("HERMES_AGENT", "true"),
        ];
        assert_eq!(resolve(base), Some(expected("hermes", "hermes-session")));
        assert_eq!(
            resolve(&[
                ("HERMES_SESSION_ID", "hermes-session"),
                ("AI_AGENT", "hermes-agent"),
            ]),
            None
        );
        assert_eq!(
            resolve(&[
                ("HERMES_SESSION_ID", "hermes-session"),
                ("AI_AGENT", "hermes-agent"),
                ("HERMES_AGENT", "TRUE"),
            ]),
            None
        );
    }

    #[test]
    fn resolves_shelley_from_its_conversation_variable() {
        assert_eq!(
            resolve(&[("SHELLEY_CONVERSATION_ID", " shelley-session ")]),
            Some(expected("shelley", "shelley-session"))
        );
    }

    #[test]
    fn resolves_qwen_code_only_with_exact_provider_marker() {
        assert_eq!(
            resolve(&[("QWEN_CODE_SESSION_ID", "qwen-session"), ("QWEN_CODE", "1"),]),
            Some(expected("qwen_code", "qwen-session"))
        );
        assert_eq!(
            resolve(&[
                ("QWEN_CODE_SESSION_ID", "qwen-session"),
                ("QWEN_CODE", "true"),
            ]),
            None
        );
    }

    #[test]
    fn resolves_mux_for_each_allowlisted_runtime() {
        for runtime in MUX_RUNTIMES {
            assert_eq!(
                resolve(&[
                    ("MUX_WORKSPACE_ID", " mux-session "),
                    ("MUX_RUNTIME", runtime),
                ]),
                Some(expected("mux", "mux-session")),
                "runtime {runtime}"
            );
        }
    }

    #[test]
    fn mux_rejects_missing_or_non_allowlisted_runtime_values() {
        for runtime in ["", "LOCAL", "container", "dev_container", "worktrees"] {
            assert_eq!(
                resolve(&[
                    ("MUX_WORKSPACE_ID", "mux-session"),
                    ("MUX_RUNTIME", runtime),
                ]),
                None,
                "runtime {runtime}"
            );
        }
        assert_eq!(resolve(&[("MUX_WORKSPACE_ID", "mux-session")]), None);
    }

    #[test]
    fn zero_candidates_and_multiple_distinct_candidates_abstain() {
        assert_eq!(resolve(&[]), None);
        assert_eq!(
            resolve(&[
                ("CODEX_THREAD_ID", "same-session"),
                ("GROK_SESSION_ID", "same-session"),
            ]),
            None
        );
        assert_eq!(
            resolve(&[
                ("CODEX_THREAD_ID", "codex-session"),
                ("GROK_SESSION_ID", "grok-session"),
            ]),
            None
        );
    }

    #[test]
    fn duplicate_identical_candidates_are_deduplicated() {
        const DUPLICATE_RULES: &[ProviderRule] = &[
            ProviderRule {
                provider: "codex",
                session: "CODEX_THREAD_ID",
                gate: RuleGate::SessionOnly,
            },
            ProviderRule {
                provider: "codex",
                session: "CODEX_THREAD_ID",
                gate: RuleGate::SessionOnly,
            },
        ];
        let values = env(&[("CODEX_THREAD_ID", "codex-session")]);
        assert_eq!(
            resolve_with_rules(|name| values.get(name).cloned(), DUPLICATE_RULES),
            Some(expected("codex", "codex-session"))
        );
    }
}
