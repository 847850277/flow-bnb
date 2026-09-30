use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn replay_cli_is_explicitly_simulated_and_needs_no_wallet() {
    let output = Command::new(env!("CARGO_BIN_EXE_flow-bnb"))
        .args(["cycle-replay", "flows/linked_stock_cycle.http.yml"])
        .env("FLOW_BNB_AGENTIC_CONFIG", "/nonexistent/wallet.json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let r: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(r["mode"], "simulation");
    assert_eq!(r["live_transactions"], false);
    assert_eq!(r["simulated_orders"], 2);
}

#[test]
fn polling_script_forwards_arguments_and_stops_at_terminal_phases() {
    let d = tempfile::tempdir().unwrap();
    let mock = d.path().join("flow-bnb");
    let script = d.path().join("run-cycle.sh");
    fs::copy("scripts/run-cycle.sh", &script).unwrap();
    fs::write(
        &mock,
        r#"#!/bin/bash
printf '%s\n' "$@" >> "$CYCLE_TEST_LOG"
case "$1" in
  cycle-step) printf '{"phase":"%s"}\n' "$CYCLE_TEST_PHASE" ;;
  cycle-status) printf '%s\n' "$CYCLE_TEST_PHASE" ;;
  *) exit 90 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&mock, fs::Permissions::from_mode(0o700)).unwrap();
    for (phase, code) in [
        ("completed", 0),
        ("completed_with_discrepancy", 0),
        ("needs_attention", 1),
        ("stopped", 1),
    ] {
        let log = d.path().join(phase);
        let output = Command::new("bash")
            .arg(&script)
            .args([
                "a flow.http.yml",
                "stable-run",
                "--config",
                "a config.json",
                "--execute",
            ])
            .env(
                "FLOW_BNB_BIN",
                if phase == "completed" {
                    mock.as_os_str()
                } else {
                    std::ffi::OsStr::new("")
                },
            )
            .env("CYCLE_TEST_LOG", &log)
            .env("CYCLE_TEST_PHASE", phase)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let args = fs::read_to_string(log).unwrap();
        assert_eq!(args.matches("cycle-step").count(), 1);
        assert_eq!(args.matches("cycle-status").count(), 1);
        assert_eq!(args.matches("a config.json").count(), 2);
        assert!(args.contains("a flow.http.yml\n"));
    }
}
