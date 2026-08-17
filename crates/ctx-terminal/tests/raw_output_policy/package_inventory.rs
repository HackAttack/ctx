use super::*;

pub(super) fn package_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if manifest.ends_with("ctx-cli") && manifest.join("src").is_dir() {
        return manifest;
    }
    if let Some(workspace_root) = manifest.parent().and_then(Path::parent) {
        let cli = workspace_root.join("crates/ctx-cli");
        if cli.join("src").is_dir() {
            return cli;
        }
    }
    if let (Ok(source_dir), Ok(workspace)) = (env::var("TEST_SRCDIR"), env::var("TEST_WORKSPACE")) {
        let runfiles = PathBuf::from(source_dir)
            .join(workspace)
            .join("crates/ctx-cli");
        if runfiles.join("src").is_dir() {
            return runfiles;
        }
    }
    panic!(
        "cannot resolve crates/ctx-cli source root from CARGO_MANIFEST_DIR={}",
        env!("CARGO_MANIFEST_DIR")
    );
}

pub(super) fn visit_production_source_files(directory: &Path, paths: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("read {} entry: {error}", directory.display()));
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) != Some("tests") {
                visit_production_source_files(&path, paths);
            }
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs")
            && !is_test_source_file(&path)
        {
            paths.push(path);
        }
    }
}

pub(super) fn production_source_paths(root: &Path) -> Vec<PathBuf> {
    let workspace_root = root
        .parent()
        .and_then(Path::parent)
        .expect("ctx-cli package belongs to the workspace crates directory");
    let application_root = workspace_root.join("crates/ctx-agent-application");
    let integrations_root = workspace_root.join("crates/ctx-agent-integrations");
    let daemon_runtime_root = workspace_root.join("crates/ctx-daemon-runtime");
    let engine_root = workspace_root.join("crates/ctx-upgrade-engine");
    let managed_pair_root = workspace_root.join("crates/ctx-managed-pair-engine");
    let history_cli_root = workspace_root.join("crates/ctx-history-cli");
    let presentation_root = workspace_root.join("crates/ctx-cli-presentation");
    let terminal_root = workspace_root.join("crates/ctx-terminal");
    let mut paths = vec![
        root.join("build.rs"),
        workspace_root.join("crates/ctx-semantic-model/build.rs"),
        engine_root.join("build.rs"),
    ];
    visit_production_source_files(&application_root.join("src"), &mut paths);
    visit_production_source_files(&integrations_root.join("src"), &mut paths);
    visit_production_source_files(&root.join("src"), &mut paths);
    visit_production_source_files(&history_cli_root.join("src"), &mut paths);
    visit_production_source_files(&presentation_root.join("src"), &mut paths);
    visit_production_source_files(&terminal_root.join("src"), &mut paths);
    visit_production_source_files(&daemon_runtime_root.join("src"), &mut paths);
    visit_production_source_files(&managed_pair_root.join("src"), &mut paths);
    visit_production_source_files(&engine_root.join("src"), &mut paths);
    paths.sort();
    paths
}

pub(super) fn is_test_source_file(path: &Path) -> bool {
    // Keep this aligned with RUST_PROD_SRC_EXCLUDES. Do not add product files
    // here to silence a finding; narrow a detector and add a scanner test
    // instead.
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    name == "tests.rs"
        || name.ends_with("_tests.rs")
        || name.starts_with("test_support")
        || path
            .components()
            .any(|component| component.as_os_str() == "tests")
}

pub(super) fn scan_package() -> Vec<Site> {
    let root = package_root();
    let workspace_root = root
        .parent()
        .and_then(Path::parent)
        .expect("ctx-cli package belongs to the workspace crates directory");
    let model_build = workspace_root.join("crates/ctx-semantic-model/build.rs");
    let application_root = workspace_root.join("crates/ctx-agent-application");
    let integrations_root = workspace_root.join("crates/ctx-agent-integrations");
    let daemon_runtime_root = workspace_root.join("crates/ctx-daemon-runtime");
    let engine_root = workspace_root.join("crates/ctx-upgrade-engine");
    let managed_pair_root = workspace_root.join("crates/ctx-managed-pair-engine");
    let history_cli_root = workspace_root.join("crates/ctx-history-cli");
    let presentation_root = workspace_root.join("crates/ctx-cli-presentation");
    let terminal_root = workspace_root.join("crates/ctx-terminal");
    let mut sources = Vec::new();
    for path in production_source_paths(&root) {
        let relative = if path == model_build {
            "crates/ctx-semantic-model/build.rs".to_owned()
        } else if let Ok(relative) = path.strip_prefix(&application_root) {
            format!(
                "crates/ctx-agent-application/{}",
                relative.to_string_lossy().replace('\\', "/")
            )
        } else if let Ok(relative) = path.strip_prefix(&integrations_root) {
            format!(
                "crates/ctx-agent-integrations/{}",
                relative.to_string_lossy().replace('\\', "/")
            )
        } else if let Ok(relative) = path.strip_prefix(&daemon_runtime_root) {
            format!(
                "crates/ctx-daemon-runtime/{}",
                relative.to_string_lossy().replace('\\', "/")
            )
        } else if let Ok(relative) = path.strip_prefix(&managed_pair_root) {
            format!(
                "crates/ctx-managed-pair-engine/{}",
                relative.to_string_lossy().replace('\\', "/")
            )
        } else if let Ok(relative) = path.strip_prefix(&engine_root) {
            format!(
                "crates/ctx-upgrade-engine/{}",
                relative.to_string_lossy().replace('\\', "/")
            )
        } else if let Ok(relative) = path.strip_prefix(&presentation_root) {
            format!(
                "crates/ctx-cli-presentation/{}",
                relative.to_string_lossy().replace('\\', "/")
            )
        } else if let Ok(relative) = path.strip_prefix(&terminal_root) {
            format!(
                "crates/ctx-terminal/{}",
                relative.to_string_lossy().replace('\\', "/")
            )
        } else if let Ok(relative) = path.strip_prefix(&history_cli_root) {
            format!(
                "crates/ctx-history-cli/{}",
                relative.to_string_lossy().replace('\\', "/")
            )
        } else {
            path.strip_prefix(&root)
                .expect("CLI source belongs to the CLI package root")
                .to_string_lossy()
                .replace('\\', "/")
        };
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        sources.push((relative, source));
    }
    let mut document_catalog = DocumentCatalog::default();
    for (_, source) in &sources {
        let tokens = lex(source);
        let excluded = test_only_mask(&tokens);
        let functions = function_spans(&tokens, &excluded);
        document_catalog.absorb(&tokens, &functions);
    }
    let mut sites = Vec::new();
    for (relative, source) in sources {
        sites.extend(scan_source_with_catalog(
            &relative,
            &source,
            &document_catalog,
        ));
    }
    sites
}

pub(super) fn assert_terminal_print_macros_are_absent(sites: &[Site]) {
    let bypasses = sites
        .iter()
        .filter(|site| {
            site.key.path.starts_with("crates/ctx-terminal/src/")
                && site.key.primitive == Primitive::PrintMacro
        })
        .map(format_site)
        .collect::<String>();
    assert!(
        bypasses.is_empty(),
        "ctx-terminal production print macros bypass OutputMeasurement; standard-module aliases and re-exports are rejected at declaration; use the explicit measured output API:\n{bypasses}"
    );
}
