#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt};

use assert_cmd::Command;
use tempfile::tempdir;

#[test]
fn release_binary_ignores_all_untrusted_helper_overrides() {
    let root = tempdir().unwrap();
    let helper = root.path().join("untrusted-ctx-pro");
    let executed = root.path().join("executed");
    fs::write(
        &helper,
        format!("#!/bin/sh\n: > '{}'\nexit 99\n", executed.display()),
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();

    let output = Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_CHANNEL", "staging")
        .env("CTX_PRO_HELPER", &helper)
        .env("CTX_PRO_QUALIFICATION_HELPER_PATH", &helper)
        .env(
            "CTX_PRO_QUALIFICATION_HELPER_SHA256",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .env("CTX_PRO_QUALIFICATION_HELPER_CHANNEL", "staging")
        .args([
            "--data-root",
            root.path().to_str().unwrap(),
            "status",
            "--json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(!executed.exists());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let status = &status["pro"];
    assert_eq!(status["installed"], false);
    assert_eq!(status["error_code"], "pro_not_installed");
    let rendered = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!rendered.contains(helper.to_str().unwrap()));
}
