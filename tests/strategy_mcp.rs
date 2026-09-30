use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    sequence: u64,
}
impl Mcp {
    fn start(root: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_flow-bnb"))
            .args(["mcp", "--root"])
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_remove("FLOW_BNB_AGENTIC_CONFIG")
            .env("STRATEGY_FIXTURE", root)
            .env("BINANCE_WEB3_API_KEY", "must-not-reach-baw")
            .env("BINANCE_WEB3_SECRET_KEY", "must-not-reach-baw")
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, messages) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let value = serde_json::from_str(&line.unwrap()).unwrap();
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        let mut m = Self {
            child,
            stdin,
            messages,
            sequence: 0,
        };
        m.call("initialize",json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"strategy-test","version":"1"}}));
        m.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        m
    }
    fn send(&mut self, v: Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        self.sequence += 1;
        let id = self.sequence;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        loop {
            let v = self
                .messages
                .recv_timeout(Duration::from_secs(10))
                .expect("MCP timed out");
            if v["id"] == id {
                assert!(v.get("error").is_none(), "{v}");
                return v["result"].clone();
            }
        }
    }
    fn tool(&mut self, name: &str, args: Value) -> Value {
        let v = self.call("tools/call", json!({"name":name,"arguments":args}));
        assert_ne!(v["isError"], true, "{v}");
        v["structuredContent"].clone()
    }
    fn fails(&mut self, name: &str, args: Value) {
        let v = self.call("tools/call", json!({"name":name,"arguments":args}));
        assert_eq!(v["isError"], true, "{v}");
    }
}
impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn fixture() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join(".flow-bnb");
    fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
    let baw = d.path().join("baw");
    fs::write(&baw,r#"#!/bin/sh
if [ -n "$BINANCE_WEB3_API_KEY" ] || [ -n "$BINANCE_WEB3_SECRET_KEY" ]; then exit 80; fi
if [ "$1 $2" != 'market-order quote' ]; then exit 81; fi
printf 'quote\n' >> "$STRATEGY_FIXTURE/calls"
amount=$(/bin/cat "$STRATEGY_FIXTURE/quote")
printf '{"success":true,"data":{"fromCoinSymbol":"USDT","toCoinSymbol":"AAPLon","fromCoinAmount":"6","toCoinAmount":"%s","slippage":0.005}}\n' "$amount"
"#).unwrap();
    fs::set_permissions(&baw, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(d.path().join("quote"), "0.021").unwrap();
    let mut c: Value =
        serde_json::from_str(include_str!("../examples/agentic-config.json")).unwrap();
    c["executable"] = json!(baw);
    c["wallet_address"] = json!(format!("0x{}", "1".repeat(40)));
    c["state_dir"] = json!(dir.join("state"));
    fs::write(dir.join("agentic.json"), c.to_string()).unwrap();
    d
}
fn calls(root: &Path) -> usize {
    fs::read_to_string(root.join("calls"))
        .unwrap_or_default()
        .lines()
        .count()
}

#[test]
fn linked_cycle_can_be_generated_saved_and_replayed_without_wallet_configuration() {
    let d = tempfile::tempdir().unwrap();
    let mut m = Mcp::start(d.path());
    let generated = m.tool(
        "generate_bnb_flow",
        json!({"template":"linked_stock_cycle"}),
    );
    let yaml = generated["canonical_yaml"].as_str().unwrap();
    let validation = m.tool("validate_bnb_flow", json!({"yaml":yaml}));
    assert_eq!(validation["valid"], true);
    assert!(validation["strategy_validation_error"].is_null());
    let saved = m.tool(
        "save_bnb_flow",
        json!({"path":"strategies/linked.http.yml","yaml":yaml}),
    );
    let replay = m.tool(
        "replay_bnb_cycle",
        json!({"path":"strategies/linked.http.yml","expected_sha256":saved["sha256"]}),
    );
    assert_eq!(replay["mode"], "simulation");
    assert_eq!(replay["live_transactions"], false);
    assert_eq!(replay["final_phase"], "completed");
    assert_eq!(replay["simulated_orders"], 2);
    assert!(!d.path().join(".flow-bnb").exists());
}

#[test]
fn stock_spread_template_is_discoverable_and_can_be_reviewed_and_saved() {
    let d = fixture();
    let mut m = Mcp::start(d.path());
    let capabilities = m.tool("list_bnb_capabilities", json!({}));
    assert!(capabilities["templates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["id"] == "stock_spread_strategy"));
    let generated = m.tool(
        "generate_bnb_flow",
        json!({"template":"stock_spread_strategy"}),
    );
    let yaml = generated["canonical_yaml"].as_str().unwrap();
    let validation = m.tool("validate_bnb_flow", json!({"yaml":yaml}));
    assert_eq!(validation["valid"], true);
    assert!(validation["strategy_validation_error"].is_null());
    m.tool(
        "save_bnb_flow",
        json!({"path":"strategies/spread.http.yml","yaml":yaml}),
    );
    let saved = fs::read_to_string(d.path().join("strategies/spread.http.yml")).unwrap();
    assert!(saved.contains("rwa-spread"));
    assert!(saved.contains("threshold_bps"));
    assert_eq!(calls(d.path()), 0);
}

#[test]
fn authored_yaml_round_trip_preview_execution_retry_and_frozen_snapshot() {
    let d = fixture();
    let mut m = Mcp::start(d.path());
    let tools = m.call("tools/list", json!({}));
    for name in [
        "save_bnb_flow",
        "read_bnb_flow",
        "run_bnb_strategy",
        "request_bnb_strategy_execution",
    ] {
        assert!(tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == name));
    }
    let generated = m.tool("generate_bnb_flow", json!({"template":"stock_strategy"}));
    let source = generated["canonical_yaml"]
        .as_str()
        .unwrap()
        .replace("aaplon-minimum-receive", "user-authored-aapl");
    let validated = m.tool("validate_bnb_flow", json!({"yaml":source}));
    assert_eq!(validated["valid"], true);
    assert!(validated["strategy_validation_error"].is_null());
    let saved = m.tool(
        "save_bnb_flow",
        json!({"path":"strategies/apple.http.yml","yaml":source}),
    );
    let read = m.tool("read_bnb_flow", json!({"path":"strategies/apple.http.yml"}));
    assert_eq!(saved["sha256"], read["sha256"]);
    let args = json!({"path":"strategies/apple.http.yml","expected_sha256":saved["sha256"]});
    let preview = m.tool("run_bnb_strategy", args.clone());
    assert_eq!(preview["report"]["success"], true, "{preview}");
    assert_eq!(preview["report"]["decision"]["triggered"], true);
    assert!(!d.path().join(".flow-bnb/state/native-handoff").exists());
    let mut queue = args.clone();
    queue["request_id"] = json!("user-order-1");
    fs::write(d.path().join("quote"), "0.01").unwrap();
    assert_eq!(
        m.tool("request_bnb_strategy_execution", queue.clone())["state"],
        "not_triggered"
    );
    let inbox = d.path().join(".flow-bnb/state/native-handoff");
    assert_eq!(fs::read_dir(&inbox).unwrap().count(), 0);
    fs::write(d.path().join("quote"), "0.021").unwrap();
    let queued = m.tool("request_bnb_strategy_execution", queue.clone());
    assert_eq!(queued["state"], "executing");
    // This quote-only wallet cannot execute. The worker must finish as blocked,
    // not wait for an operator. Wait before checking that retries do no work.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let status = m.tool(
            "get_agentic_execution",
            json!({"intent_id":queued["intent_id"]}),
        );
        if status["state"] == "blocked" {
            break;
        }
        assert!(std::time::Instant::now() < until, "{status}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let id = queued["intent_id"].clone();
    let before = calls(d.path());
    fs::write(d.path().join("quote"), "0.001").unwrap();
    let retry = m.tool("request_bnb_strategy_execution", queue.clone());
    assert_eq!(retry["intent_id"], id);
    assert_eq!(
        calls(d.path()),
        before,
        "retry must not reevaluate or submit"
    );
    let mut changed = queue.clone();
    changed["inputs"] = json!({"min_receive":"0.01"});
    m.fails("request_bnb_strategy_execution", changed);
    m.fails("cancel_agentic_execution", json!({"intent_id":id}));
    assert_eq!(
        m.tool("request_bnb_strategy_execution", queue.clone())["state"],
        "blocked"
    );
    let mut modified = source.clone();
    modified.push_str("\n# user edit\n");
    m.fails("save_bnb_flow",json!({"path":"strategies/apple.http.yml","yaml":modified,"overwrite":true,"expected_sha256":"wrong"}));
    let modified = source.replace("user-authored-aapl", "edited-after-queue");
    m.tool("save_bnb_flow",json!({"path":"strategies/apple.http.yml","yaml":modified,"overwrite":true,"expected_sha256":saved["sha256"]}));
    m.fails("request_bnb_strategy_execution", queue);
    let hash = queued["strategy"]["snapshot_sha256"].as_str().unwrap();
    let frozen = fs::read_to_string(
        d.path()
            .join(".flow-bnb/state/strategy-snapshots")
            .join(format!("{hash}.json")),
    )
    .unwrap();
    assert!(frozen.contains("user-authored-aapl") && !frozen.contains("edited-after-queue"));
    assert_eq!(
        fs::read_dir(inbox)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".intent.json"))
            .count(),
        1
    );
}

#[test]
fn malformed_yaml_paths_and_symlinks_cannot_write_outside_root() {
    let d = fixture();
    let mut m = Mcp::start(d.path());
    let yaml = include_str!("../flows/stock_strategy.http.yml");
    for path in ["../escape.http.yml", "/tmp/escape.http.yml", "config.json"] {
        m.fails("save_bnb_flow", json!({"path":path,"yaml":yaml}));
    }
    let other = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(other.path(), d.path().join("escape")).unwrap();
    m.fails(
        "save_bnb_flow",
        json!({"path":"escape/file.http.yml","yaml":yaml}),
    );
    m.fails(
        "save_bnb_flow",
        json!({"path":"bad.http.yml","yaml":"not: [valid"}),
    );
    assert!(!d.path().join("bad.http.yml").exists());
    assert_eq!(fs::read_dir(other.path()).unwrap().count(), 0);
}

#[test]
fn cli_runs_the_same_strategy_profile_without_trading() {
    let d = fixture();
    let file = d.path().join("example.http.yml");
    fs::write(&file, include_str!("../flows/stock_strategy.http.yml")).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_flow-bnb"))
        .arg("strategy-run")
        .arg(file)
        .args(["--config"])
        .arg(d.path().join(".flow-bnb/agentic.json"))
        .args(["--input", "min_receive=\"0.03\""])
        .env("STRATEGY_FIXTURE", d.path())
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["success"], true);
    assert_eq!(v["decision"]["triggered"], false);
    assert!(!d.path().join(".flow-bnb/state/native-handoff").exists());
}
