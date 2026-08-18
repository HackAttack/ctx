use super::hosted_pair_install::{
    acquire_hosted_install_lock, hosted_source_path, replace_hosted_file, stage_hosted_bytes,
    HostedInstallMarker,
};
use super::*;

#[test]
fn fingerprint_is_the_sha256_of_the_canonical_inventory() {
    assert_eq!(
        format!("{:x}", Sha256::digest(API_INVENTORY.as_bytes())),
        API_FINGERPRINT
    );
}

#[test]
fn duplicates_and_multiframe_input_fail_closed() {
    assert!(reject_duplicate_keys(r#"{"a":1,"a":2}"#).is_err());
    assert!(parse_frame(b"{}\n{}".to_vec()).is_err());
}

#[test]
fn only_the_exact_hidden_argv_is_intercepted() {
    assert!(intercept(&["ctx".into(), INVOCATION.into()]).is_some());
    assert!(intercept(&["ctx".into(), "--ctx-core-capability-v1=x".into()]).is_none());
    assert!(intercept(&["ctx".into(), "--ctx-core-hosted-pair-install-v1=x".into()]).is_none());
}

#[test]
fn hosted_pair_sources_must_be_absolute_regular_files() {
    assert!(hosted_source_path(std::ffi::OsStr::new("relative"), "test").is_err());
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("artifact");
    std::fs::write(&file, b"artifact").unwrap();
    assert_eq!(hosted_source_path(file.as_os_str(), "test").unwrap(), file);
}

#[test]
fn hosted_pair_replacement_is_atomic_and_idempotently_repairable() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("libexec/ctx-pro");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, b"old-pro").unwrap();

    let abandoned = stage_hosted_bytes(b"abandoned", &target, 0o755, "test Pro").unwrap();
    let restaged = stage_hosted_bytes(b"protocol-v3-pro", &target, 0o755, "test Pro").unwrap();
    assert_eq!(abandoned, restaged);
    replace_hosted_file(&restaged, &target, "test Pro").unwrap();

    for _ in 0..2 {
        let staged = stage_hosted_bytes(b"protocol-v3-pro", &target, 0o755, "test Pro").unwrap();
        replace_hosted_file(&staged, &target, "test Pro").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"protocol-v3-pro");
        assert!(!staged.exists());
    }
}

#[test]
fn hosted_pair_installation_is_serialized_without_persistent_lock_state() {
    let root = tempfile::tempdir().unwrap();
    let first = acquire_hosted_install_lock(root.path()).unwrap();
    assert!(acquire_hosted_install_lock(root.path()).is_err());
    drop(first);
    assert!(acquire_hosted_install_lock(root.path()).is_ok());
}

#[test]
fn hosted_marker_channel_is_distribution_only() {
    let staging = HostedInstallMarker {
        schema_version: 1,
        manager: "ctx-hosted-installer".to_owned(),
        install_path: "/tmp/ctx".to_owned(),
        platform: "linux-x64".to_owned(),
        channel: "stable".to_owned(),
        sha256: "1".repeat(64),
        staging_dogfood: true,
    };
    assert_eq!(
        staging.release_channel().unwrap(),
        ctx_companion_bridge::ReleaseChannel::Staging
    );
}

#[test]
fn capability_response_is_one_exact_flushed_json_frame() {
    let mut output = Vec::new();
    write_response_frame(&mut output, br#"{"ok":true}"#).unwrap();
    assert_eq!(output, b"{\"ok\":true}\n");
}

#[test]
fn local_usage_summary_returns_canonical_config_error_without_aborting() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("config.toml"),
        "[local_usage]\nenabled = unavailable\n",
    )
    .unwrap();

    let response = execute(Request {
        data_root: root.path().to_path_buf(),
        operation: Operation::LocalUsageSummary,
        options: Options::Empty,
    })
    .unwrap();

    assert_eq!(response["ok"], true);
    assert_eq!(response["operation"], "LocalUsageSummary");
    assert_eq!(
        response["facts"],
        serde_json::to_value(crate::local_usage::UsageReport::config_error()).unwrap()
    );
    assert!(!root.path().join("usage.sqlite").exists());
}

#[test]
fn local_usage_summary_protocol_version_mismatches_remain_hard_failures() {
    let root = tempfile::tempdir().unwrap();
    let request = json!({
        "data_root": root.path(),
        "operation": "LocalUsageSummary",
        "options": {},
        "protocol_version": CORE_PRO_PROTOCOL_VERSION.get(),
        "schema_version": 1,
    });
    assert!(parse_frame(canonical(&request).unwrap()).is_ok());

    let mut wrong_protocol = request.clone();
    wrong_protocol["protocol_version"] = json!(CORE_PRO_PROTOCOL_VERSION.get() + 1);
    assert!(parse_frame(canonical(&wrong_protocol).unwrap()).is_err());

    let mut wrong_schema = request.clone();
    wrong_schema["schema_version"] = json!(2);
    assert!(parse_frame(canonical(&wrong_schema).unwrap()).is_err());

    let mut unknown_field = request;
    unknown_field["unexpected"] = json!(true);
    assert!(parse_frame(canonical(&unknown_field).unwrap()).is_err());
}

#[test]
fn canonical_response_is_bounded_machine_json() {
    let bytes = canonical(&json!({"schema_version": 1, "ok": true})).unwrap();
    assert!(bytes.len() <= MAX_RESPONSE_BYTES);
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap()["ok"], true);
    assert!(!bytes.contains(&b'\n'));
}

#[test]
fn managed_setup_generation_is_optional_and_prefers_publication() {
    let no_generation = json!({"lexical": {"generation_id": null}});
    assert_eq!(setup_generation_id(None, &no_generation), None);

    let current = "1".repeat(64);
    let status = json!({"lexical": {"generation_id": current}});
    assert_eq!(setup_generation_id(None, &status), Some("1".repeat(64)));
    assert_eq!(
        setup_generation_id(Some("2".repeat(64)), &status),
        Some("2".repeat(64))
    );
}

#[test]
fn managed_fresh_default_preserves_core_only_empty_publication_wait() {
    let empty: anyhow::Error = crate::semantic::SourceBackedRefreshPendingPublication::new(
        "fresh-empty".to_owned(),
        "queued".to_owned(),
        0,
    )
    .into();
    let nonempty: anyhow::Error = crate::semantic::SourceBackedRefreshPendingPublication::new(
        "fresh-nonempty".to_owned(),
        "queued".to_owned(),
        1,
    )
    .into();
    assert!(should_wait_for_fresh_empty_publication(false, &empty));
    assert!(!should_wait_for_fresh_empty_publication(true, &empty));
    assert!(!should_wait_for_fresh_empty_publication(false, &nonempty));
}

#[test]
fn managed_waited_refresh_failure_is_not_reported_as_setup_success() {
    let failure = anyhow!("source refresh failed");
    assert!(should_propagate_setup_refresh_failure(true, &failure));
    assert!(!should_propagate_setup_refresh_failure(false, &failure));
}

#[test]
fn managed_setup_presentation_options_are_closed_and_bounded() {
    let options = json!({
        "catalog_only": false,
        "defer_fresh_empty_wait": true,
        "no_daemon": false,
        "notice_lines": ["approved line", "", "https://companion.example.test/opaque"],
        "progress": "auto",
        "semantic": false,
        "wait": false,
    });
    let parsed = parse_options(Operation::CoreSetup, &options).unwrap();
    let Options::Setup(CoreSetupOptions {
        defer_fresh_empty_wait,
        notice_lines,
        progress,
        ..
    }) = parsed
    else {
        panic!("expected setup options")
    };
    assert!(defer_fresh_empty_wait);
    assert_eq!(notice_lines[2], "https://companion.example.test/opaque");
    assert_eq!(progress, crate::progress::ProgressArg::Auto);

    let mut invalid = options.clone();
    invalid["notice_lines"] = json!(["line\nforgery"]);
    assert!(parse_options(Operation::CoreSetup, &invalid).is_err());
    invalid = options;
    invalid["progress"] = json!("verbose");
    assert!(parse_options(Operation::CoreSetup, &invalid).is_err());

    let mut oversized = json!({
        "catalog_only": false,
        "defer_fresh_empty_wait": true,
        "no_daemon": false,
        "notice_lines": ["x".repeat(513)],
        "progress": "auto",
        "semantic": false,
        "wait": false,
    });
    assert!(parse_options(Operation::CoreSetup, &oversized).is_err());
    oversized["notice_lines"] = json!(["x".repeat(512)]);
    assert!(parse_options(Operation::CoreSetup, &oversized).is_ok());
}

#[test]
fn oversized_live_notice_degrades_to_plain_progress_before_cursor_rendering() {
    let lines = vec!["one line wider than a narrow terminal".to_owned()];
    assert_eq!(
        progress_mode_for_notice(crate::progress::ProgressArg::Auto, Some(32), &lines),
        crate::progress::ProgressArg::Plain
    );
    assert_eq!(
        progress_mode_for_notice(crate::progress::ProgressArg::Auto, Some(80), &lines),
        crate::progress::ProgressArg::Auto
    );
    assert_eq!(
        progress_mode_for_notice(crate::progress::ProgressArg::Plain, Some(32), &lines),
        crate::progress::ProgressArg::Plain
    );
}
