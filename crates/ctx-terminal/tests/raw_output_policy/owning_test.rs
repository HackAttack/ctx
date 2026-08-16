use std::{fs, path::Path};

use super::{
    is_ident_continue, is_ident_start, is_path_ident, lex, package_root, AllowEntry, Token,
};

const POLICY_TEST_SOURCE: &str = include_str!("../raw_output_policy.rs");
const POLICY_SELF_TEST_SOURCE: &str = include_str!("self_tests.rs");

pub(super) fn validate(entry: &AllowEntry) -> Result<(), String> {
    let owner = entry.owning_test;
    let (path, symbol) = parse_identity(owner.identity)?;
    if !owner
        .covered_paths
        .iter()
        .any(|coverage| path_is_covered(entry.path, coverage))
    {
        return Err(format!(
            "owning test `{}` has no source coverage contract for {}",
            owner.identity, entry.path
        ));
    }
    let source = read_test_source(&path)?;
    let matches = runnable_test_function_names(&source)
        .into_iter()
        .filter(|test| test == &symbol)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [_] => {}
        [] => {
            return Err(format!(
                "owning test `{symbol}` is not one runnable #[test] in {path}"
            ));
        }
        tests => {
            return Err(format!(
                "owning test `{symbol}` is ambiguous ({} definitions in {path})",
                tests.len()
            ));
        }
    }
    Ok(())
}

fn parse_identity(identity: &str) -> Result<(String, String), String> {
    let Some((path, symbol)) = identity.split_once(".rs::") else {
        return Err(
            "owning test must be an exact `<source>.rs::<test_function>` identity".to_owned(),
        );
    };
    let path = format!("{path}.rs");
    if symbol.is_empty()
        || symbol.contains("::")
        || !symbol.bytes().next().is_some_and(is_ident_start)
        || !symbol.bytes().all(is_ident_continue)
    {
        return Err("owning test function is not one exact Rust identifier".to_owned());
    }
    if path != "tests/raw_output_policy.rs"
        && path != "tests/raw_output_policy/self_tests.rs"
        && !path.starts_with("src/")
        && !path.starts_with("crates/ctx-cli-presentation/src/")
        && !path.starts_with("crates/ctx-terminal/src/")
        && !path.starts_with("crates/ctx-agent-application/src/")
        && !path.starts_with("crates/ctx-agent-integrations/src/")
        && !path.starts_with("crates/ctx-client-observability/src/")
        && !path.starts_with("crates/ctx-history-cli/src/")
        && !path.starts_with("crates/ctx-upgrade-engine/src/")
    {
        return Err("owning test source is outside the source-checked test roots".to_owned());
    }
    if Path::new(&path)
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("owning test source path is not normalized".to_owned());
    }
    Ok((path, symbol.to_owned()))
}

fn read_test_source(path: &str) -> Result<String, String> {
    match path {
        "tests/raw_output_policy.rs" => Ok(POLICY_TEST_SOURCE.to_owned()),
        "tests/raw_output_policy/self_tests.rs" => Ok(POLICY_SELF_TEST_SOURCE.to_owned()),
        _ => {
            let package_root = package_root();
            let source = if path.starts_with("crates/") {
                package_root
                    .parent()
                    .and_then(Path::parent)
                    .expect("ctx-cli package belongs to the workspace crates directory")
                    .join(path)
            } else {
                package_root.join(path)
            };
            fs::read_to_string(source)
                .map_err(|error| format!("cannot read owning test source {path}: {error}"))
        }
    }
}

fn path_is_covered(path: &str, coverage: &str) -> bool {
    if coverage.ends_with('/') {
        path.starts_with(coverage)
    } else {
        path == coverage
    }
}

pub(super) fn runnable_test_function_names(source: &str) -> Vec<String> {
    let tokens = lex(source);
    let mut tests = Vec::new();
    for index in 0..tokens.len().saturating_sub(1) {
        if tokens[index].text != "fn"
            || !has_exact_test_attribute(&tokens, index)
            || has_ignore_attribute(&tokens, index)
            || !tokens
                .get(index + 1)
                .is_some_and(|token| is_path_ident(&token.text))
        {
            continue;
        }
        tests.push(tokens[index + 1].text.clone());
    }
    tests
}

fn has_exact_test_attribute(tokens: &[Token], fn_index: usize) -> bool {
    attributes_before(tokens, fn_index)
        .into_iter()
        .any(|attribute| attribute.len() == 1 && attribute[0].text == "test")
}

fn has_ignore_attribute(tokens: &[Token], fn_index: usize) -> bool {
    attributes_before(tokens, fn_index)
        .into_iter()
        .any(|attribute| {
            attribute
                .first()
                .is_some_and(|token| token.text == "ignore")
        })
}

fn attributes_before(tokens: &[Token], fn_index: usize) -> Vec<&[Token]> {
    let mut cursor = fn_index;
    if cursor > 0 && tokens[cursor - 1].text == "async" {
        cursor -= 1;
    }
    let mut attributes = Vec::new();
    while cursor > 0 && tokens[cursor - 1].text == "]" {
        let Some(open) = super::reverse_matching_delimiter(tokens, cursor - 1, "[", "]") else {
            break;
        };
        if open == 0 || tokens[open - 1].text != "#" {
            break;
        }
        attributes.push(&tokens[open + 1..cursor - 1]);
        cursor = open - 1;
    }
    attributes
}
