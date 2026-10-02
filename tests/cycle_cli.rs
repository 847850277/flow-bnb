use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn cycle_preview_preserves_its_baseline_between_cli_processes_without_submitting() {
    let d = tempfile::tempdir().unwrap();
    let wallet = d.path().join("quote-only-wallet");
    fs::write(&wallet, r#"#!/bin/sh
if [ "$1 $2" != 'market-order quote' ]; then exit 81; fi
printf 'quote\n' >> "$CYCLE_PREVIEW_FIXTURE/calls"
amount=$(/bin/cat "$CYCLE_PREVIEW_FIXTURE/quote")
printf '{"success":true,"data":{"fromCoinSymbol":"USDT","toCoinSymbol":"NVDAon","fromCoinAmount":"5","toCoinAmount":"%s","slippage":0.005}}\n' "$amount"
"#).unwrap();
    fs::set_permissions(&wallet, fs::Permissions::from_mode(0o700)).unwrap();
    let config = d.path().join("agentic.json");
    let mut c: Value =
        serde_json::from_str(include_str!("../examples/agentic-config.json")).unwrap();
    c["executable"] = json!(wallet);
    c["wallet_address"] = json!(format!("0x{}", "1".repeat(40)));
    c["state_dir"] = json!(d.path().join("state"));
    c["rpc_url"] = json!("http://127.0.0.1:1");
    fs::write(&config, c.to_string()).unwrap();
    let run = |command: &str| -> Value {
        let mut cli = Command::new(env!("CARGO_BIN_EXE_flow-bnb"));
        cli.arg(command);
        if command == "cycle-step" {
            cli.arg("flows/linked_stock_cycle.http.yml");
        }
        let output = cli
            .args(["--run-id", "preview", "--config"])
            .arg(&config)
            .env("CYCLE_PREVIEW_FIXTURE", d.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };
    fs::write(d.path().join("quote"), "0.0245").unwrap();
    let initial = run("cycle-step");
    assert_eq!(initial["phase"], "waiting_entry");
    assert_eq!(initial["baseline_receive"], "0.0245");
    fs::write(d.path().join("quote"), "0.025").unwrap();
    let triggered = run("cycle-step");
    assert_eq!(triggered["phase"], "waiting_entry");
    assert_eq!(triggered["baseline_receive"], "0.0245");
    assert_eq!(triggered["ready_to_submit"], true);
    assert!(triggered["buy"].is_null());
    assert_eq!(run("cycle-status"), triggered);
    assert_eq!(
        fs::read_to_string(d.path().join("calls")).unwrap(),
        "quote\nquote\n"
    );
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
