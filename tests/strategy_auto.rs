//! Full automatic execution against a fake wallet and loopback chain only.
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};
const WALLET: &str = "0x1111111111111111111111111111111111111111";
const AAPL: &str = "0x390a684ef9cade28a7ad0dfa61ab1eb3842618c4";
const USDT: &str = "0x55d398326f99059fF775485246999027B3197955";
const TX: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BLOCK: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
struct Fixture {
    dir: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    server: Option<thread::JoinHandle<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.server.take().unwrap().join().unwrap();
    }
}
impl Fixture {
    fn new(mode: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let received = if mode == "discrepancy" {
            1_990_000_000_000_000_000u128
        } else {
            2_000_000_000_000_000_000u128
        };
        let server = thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse::<usize>().unwrap();
                    }
                }
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).unwrap();
                let req: Value = serde_json::from_slice(&bytes).unwrap();
                let transfer = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
                let account = format!("0x{:0>64}", &WALLET[2..]);
                let router = format!("0x{:0>64}", "2".repeat(40));
                let result = match req["method"].as_str().unwrap() {
                    "eth_chainId" => json!("0x38"),
                    "eth_call" => {
                        if req["params"][0]["data"] == "0x313ce567" {
                            json!("0x12")
                        } else {
                            json!("0xde0b6b3a7640000")
                        }
                    }
                    "eth_getBalance" => json!("0xde0b6b3a7640000"),
                    "eth_blockNumber" => json!("0x12"),
                    "eth_getBlockByNumber" => json!({"hash":BLOCK,"number":"0x10"}),
                    "eth_getTransactionReceipt" => {
                        json!({"transactionHash":TX,"blockHash":BLOCK,"blockNumber":"0x10","status":"0x1","gasUsed":"0x5208","logs":[
                           {"address":AAPL,"topics":[transfer,account,router],"data":format!("0x{:x}",10_000_000_000_000_000u128)},
                           {"address":USDT,"topics":[transfer,router,account],"data":format!("0x{received:x}")}
                        ]})
                    }
                    other => panic!("unexpected RPC {other}"),
                };
                let body = json!({"jsonrpc":"2.0","id":req["id"],"result":result}).to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
            }
        });
        let state = dir.path().join(".flow-bnb");
        fs::DirBuilder::new().mode(0o700).create(&state).unwrap();
        let baw = dir.path().join("baw");
        fs::write(&baw,r#"#!/bin/sh
if [ -n "$BINANCE_WEB3_API_KEY" ] || [ -n "$BINANCE_WEB3_SECRET_KEY" ]; then exit 91; fi
case "$1 $2" in
'wallet status') printf '{"success":true,"data":{"status":"CONNECTED"}}\n' ;;
'wallet address') printf '{"success":true,"data":{"addresses":[{"binanceChainId":"56","address":"0x1111111111111111111111111111111111111111"}]}}\n' ;;
'wallet settings') printf '{"success":true,"data":{}}\n' ;;
'wallet tx-lock') printf '{"success":true,"data":{"status":"UNLOCKED"}}\n' ;;
'market-order quote')
 n=0; if [ -f "$AUTO_FIXTURE/quotes" ]; then n=$(/bin/cat "$AUTO_FIXTURE/quotes"); fi
 n=$((n+1)); printf '%s' "$n" > "$AUTO_FIXTURE/quotes"
 receive=2
 if [ -f "$AUTO_FIXTURE/condition_changes" ] && [ "$n" -ge 4 ]; then receive=0.5; fi
 printf '{"success":true,"data":{"fromCoinSymbol":"AAPLon","toCoinSymbol":"USDT","fromCoinAmount":"0.01","toCoinAmount":"%s","slippage":0.005}}\n' "$receive" ;;
'market-order swap') printf 'swap\n' >> "$AUTO_FIXTURE/swaps"; /bin/cat "$AUTO_FIXTURE/swap.json" ;;
'market-order list') /bin/cat "$AUTO_FIXTURE/order.json" ;;
*) exit 92 ;;
esac
"#).unwrap();
        fs::set_permissions(&baw, fs::Permissions::from_mode(0o700)).unwrap();
        let mut config: Value =
            serde_json::from_str(include_str!("../examples/agentic-config.json")).unwrap();
        config["executable"] = json!(baw);
        config["wallet_address"] = json!(WALLET);
        config["state_dir"] = json!(state.join("state"));
        config["rpc_url"] = json!(url);
        fs::write(state.join("agentic.json"), config.to_string()).unwrap();
        let source = include_str!("../flows/stock_strategy.http.yml");
        fs::write(dir.path().join("strategy.http.yml"), source).unwrap();
        fs::write(
            dir.path().join("swap.json"),
            if mode == "unknown" {
                json!({"success":true,"data":{}})
            } else {
                json!({"success":true,"data":{"orderId":"123"}})
            }
            .to_string(),
        )
        .unwrap();
        fs::write(dir.path().join("order.json"),json!({"success":true,"data":{"list":[{"orderId":"123","chain":"56","fromToken":AAPL,"toToken":USDT,"fromTokenQty":"0.01","slippage":"0.5","status":"FINISHED","txHash":TX,"toTokenActualQty":"2"}]}}).to_string()).unwrap();
        Self {
            dir,
            stop,
            server: Some(server),
        }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_flow-bnb"));
        c.current_dir(self.dir.path())
            .args(args)
            .stdin(Stdio::null())
            .env_remove("FLOW_BNB_AGENTIC_CONFIG")
            .env("AUTO_FIXTURE", self.dir.path())
            .env("BINANCE_WEB3_API_KEY", "must-not-reach-baw")
            .env("BINANCE_WEB3_SECRET_KEY", "must-not-reach-baw");
        c
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        self.command(args).output().unwrap()
    }
    fn authorize(&self, minimum: &str) {
        let input = format!(
            "intent={}",
            json!({"from_token":AAPL,"to_token":USDT,"amount":"0.01","slippage_bps":50})
        );
        let minimum = format!("min_receive=\"{minimum}\"");
        let o = self.run(&[
            "strategy-authorize",
            "strategy.http.yml",
            "--id",
            "apple",
            "--input",
            &input,
            "--input",
            &minimum,
            "--max-orders",
            "1",
            "--max-total-sell-amount",
            "0.01",
            "--valid-for-minutes",
            "60",
            "--cooldown-seconds",
            "0",
        ]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(self.swaps(), 0);
    }
    fn swaps(&self) -> usize {
        fs::read_to_string(self.dir.path().join("swaps"))
            .unwrap_or_default()
            .lines()
            .count()
    }
}
#[test]
fn completes_without_stdin_confirmation_and_retries_never_resubmit() {
    let f = Fixture::new("complete");
    f.authorize("1");
    let args = [
        "strategy-auto",
        "--authorization-id",
        "apple",
        "--request-id",
        "once",
    ];
    let o = f.run(&args);
    assert!(
        o.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&o.stderr),
        String::from_utf8_lossy(&o.stdout)
    );
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["state"], "completed");
    assert_eq!(v["result"]["settlement"]["received"], "2");
    assert_eq!(f.swaps(), 1);
    assert!(f.run(&args).status.success());
    assert_eq!(f.swaps(), 1);
    assert!(!f
        .run(&[
            "strategy-auto",
            "--authorization-id",
            "apple",
            "--request-id",
            "new"
        ])
        .status
        .success());
    assert_eq!(f.swaps(), 1);
}
#[test]
fn unknown_and_discrepancy_preserve_user_budget_without_refunding_or_replaying() {
    for mode in ["unknown", "discrepancy"] {
        let f = Fixture::new(mode);
        f.authorize("1");
        let o = f.run(&[
            "strategy-auto",
            "--authorization-id",
            "apple",
            "--request-id",
            "once",
        ]);
        assert_eq!(o.status.success(), mode == "discrepancy");
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(
            v["state"],
            if mode == "unknown" {
                "submission_outcome_unknown"
            } else {
                "settled_with_discrepancy"
            },
            "{v}"
        );
        assert_eq!(f.swaps(), 1);
        let o = f.run(&["strategy-authorization", "--id", "apple"]);
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(v["reserved_orders"], 1);
        assert_eq!(v["eligible"], false);
        assert!(!f
            .run(&[
                "strategy-auto",
                "--authorization-id",
                "apple",
                "--request-id",
                "new"
            ])
            .status
            .success());
        assert_eq!(f.swaps(), 1);
    }
}
#[test]
fn non_triggered_and_revoked_mandates_never_call_swap() {
    let f = Fixture::new("complete");
    f.authorize("3");
    let o = f.run(&[
        "strategy-auto",
        "--authorization-id",
        "apple",
        "--request-id",
        "check-1",
    ]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["state"], "not_triggered");
    assert!(f
        .run(&["strategy-revoke", "--id", "apple"])
        .status
        .success());
    assert!(!f
        .run(&[
            "strategy-auto",
            "--authorization-id",
            "apple",
            "--request-id",
            "check-2"
        ])
        .status
        .success());
    assert_eq!(f.swaps(), 0);
}

#[test]
fn mcp_runs_in_background_and_returns_pure_json_without_a_terminal() {
    use std::sync::mpsc;
    for mode in ["authorized", "direct", "strategy"] {
        let f = Fixture::new("complete");
        if mode == "authorized" {
            f.authorize("1");
        }
        let mut child = f
            .command(&["mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        struct Kill(std::process::Child);
        impl Drop for Kill {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut child = Kill(child);
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let v: Value = serde_json::from_str(&line.unwrap())
                    .expect("MCP stdout must contain only JSON");
                if sender.send(v).is_err() {
                    break;
                }
            }
        });
        let mut sequence = 0u64;
        {
            let mut call = |method: &str, params: Value| {
                sequence += 1;
                let id = sequence;
                writeln!(
                    input,
                    "{}",
                    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
                )
                .unwrap();
                input.flush().unwrap();
                loop {
                    let v = receiver
                        .recv_timeout(Duration::from_secs(15))
                        .expect("MCP timeout");
                    if v["id"] == id {
                        assert!(v.get("error").is_none(), "{v}");
                        return v["result"].clone();
                    }
                }
            };
            call(
                "initialize",
                json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"auto-test","version":"1"}}),
            );
            // The rmcp server accepts calls after the initialize response; explicit notification below.
        }
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        input.flush().unwrap();
        let mut tool = |name: &str, args: Value| {
            sequence += 1;
            let id = sequence;
            writeln!(input,"{}",json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}})).unwrap();
            input.flush().unwrap();
            loop {
                let v = receiver
                    .recv_timeout(Duration::from_secs(15))
                    .expect("MCP tool timeout");
                if v["id"] == id {
                    assert!(v.get("error").is_none(), "{v}");
                    return v["result"].clone();
                }
            }
        };
        let inspected = tool("get_bnb_strategy_authorization", json!({}));
        assert_ne!(inspected["isError"], true, "{inspected}");
        assert_eq!(
            inspected["structuredContent"]["authorizations"]
                .as_array()
                .unwrap()
                .len(),
            usize::from(mode == "authorized")
        );
        // Historical global barriers do not stop either execution path and are not deleted.
        let legacy = f
            .dir
            .path()
            .join(format!(".flow-bnb/state/agentic-{WALLET}.lock"));
        fs::write(&legacy, "historical-order-report").unwrap();
        let intent = json!({"from_token":AAPL,"to_token":USDT,"amount":"0.01","slippage_bps":50});
        let (execute, query, args) = match mode {
            "authorized" => (
                "execute_bnb_authorized_strategy",
                "get_bnb_authorized_execution",
                json!({"authorization_id":"apple","request_id":"one"}),
            ),
            "direct" => {
                let preview = tool("prepare_agentic_trade", intent.clone());
                assert_eq!(preview["structuredContent"]["state"], "ready", "{preview}");
                assert_eq!(f.swaps(), 0);
                (
                    "request_agentic_execution",
                    "get_agentic_execution",
                    json!({"request_id":"one","intent":intent}),
                )
            }
            _ => {
                let source = tool("read_bnb_flow", json!({"path":"strategy.http.yml"}));
                assert_ne!(source["isError"], true, "{source}");
                (
                    "request_bnb_strategy_execution",
                    "get_agentic_execution",
                    json!({"path":"strategy.http.yml","expected_sha256":source["structuredContent"]["sha256"],"request_id":"one","inputs":{"intent":intent,"min_receive":"1"}}),
                )
            }
        };
        let started = tool(execute, args.clone());
        assert_ne!(started["isError"], true, "{started}");
        let query_args = if mode == "authorized" {
            args.clone()
        } else {
            assert_eq!(started["structuredContent"]["state"], "executing");
            json!({"intent_id":started["structuredContent"]["intent_id"]})
        };
        let concurrent_retry = tool(execute, args.clone());
        assert_ne!(concurrent_retry["isError"], true, "{concurrent_retry}");
        let until = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let response = tool(query, query_args.clone());
            assert_ne!(response["isError"], true, "{response}");
            if response["structuredContent"]["state"] == "completed" {
                break;
            }
            assert!(std::time::Instant::now() < until, "{response}");
            thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(f.swaps(), 1);
        let retry = tool(execute, args);
        assert_eq!(retry["structuredContent"]["state"], "completed");
        assert_eq!(f.swaps(), 1);
        assert_eq!(
            fs::read_to_string(&legacy).unwrap(),
            "historical-order-report"
        );
        child.0.kill().unwrap();
    }
}

#[test]
fn condition_fading_before_dispatch_is_not_a_submission_or_a_permanent_halt() {
    let f = Fixture::new("complete");
    fs::write(f.dir.path().join("condition_changes"), b"yes").unwrap();
    f.authorize("1");
    let o = f.run(&[
        "strategy-auto",
        "--authorization-id",
        "apple",
        "--request-id",
        "changed",
    ]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["state"], "not_triggered");
    assert_eq!(f.swaps(), 0);
    let o = f.run(&["strategy-authorization", "--id", "apple"]);
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["reserved_orders"], 0);
    assert_eq!(v["eligible"], true);
}

#[test]
fn native_cli_executes_without_tty_and_never_reuses_a_report() {
    let f = Fixture::new("complete");
    let intent = json!({"from_token":AAPL,"to_token":USDT,"amount":"0.01","slippage_bps":50});
    fs::write(f.dir.path().join("intent.json"), intent.to_string()).unwrap();
    let preview = f.run(&[
        "agentic-trade",
        "--request",
        "intent.json",
        "--report",
        "preview.json",
    ]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    assert_eq!(f.swaps(), 0);
    let args = [
        "agentic-trade",
        "--request",
        "intent.json",
        "--report",
        "execution.json",
        "--execute",
    ];
    let result = f.run(&args);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["state"],
        "completed"
    );
    assert_eq!(f.swaps(), 1);
    assert!(!f.run(&args).status.success());
    assert_eq!(f.swaps(), 1);
    assert!(fs::read_dir(f.dir.path().join(".flow-bnb/state"))
        .unwrap()
        .all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".lock")));
}

#[test]
fn exceptional_order_results_do_not_lock_independent_requests_or_replay_old_ones() {
    for mode in ["unknown", "discrepancy"] {
        let f = Fixture::new(mode);
        let input = format!(
            "intent={}",
            json!({"from_token":AAPL,"to_token":USDT,"amount":"0.01","slippage_bps":50})
        );
        let expected = if mode == "unknown" {
            "submission_outcome_unknown"
        } else {
            "settled_with_discrepancy"
        };
        for (id, count) in [("one", 1), ("one", 1), ("independent-order", 2)] {
            let result = f.run(&[
                "strategy-run",
                "strategy.http.yml",
                "--input",
                &input,
                "--input",
                "min_receive=\"1\"",
                "--execute",
                id,
            ]);
            let report: Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(
                report["state"],
                expected,
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(f.swaps(), count);
        }
    }
}

#[test]
fn direct_strategy_rechecks_conditions_and_startup_never_drains_old_requests() {
    let f = Fixture::new("complete");
    let input = format!(
        "intent={}",
        json!({"from_token":AAPL,"to_token":USDT,"amount":"0.01","slippage_bps":50})
    );
    fs::write(f.dir.path().join("condition_changes"), "yes").unwrap();
    fs::write(f.dir.path().join("quotes"), "1").unwrap();
    let result = f.run(&[
        "strategy-run",
        "strategy.http.yml",
        "--input",
        &input,
        "--input",
        "min_receive=\"1\"",
        "--execute",
        "changed",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["state"],
        "not_triggered"
    );
    assert_eq!(f.swaps(), 0);
    fs::remove_file(f.dir.path().join("condition_changes")).unwrap();
    let queued = f.run(&[
        "strategy-run",
        "strategy.http.yml",
        "--input",
        &input,
        "--input",
        "min_receive=\"1\"",
        "--enqueue",
        "old",
    ]);
    assert!(queued.status.success());
    let queued: Value = serde_json::from_slice(&queued.stdout).unwrap();
    assert_eq!(queued["state"], "queued");
    let c = flow_bnb::agentic::Config::read(&f.dir.path().join(".flow-bnb/agentic.json")).unwrap();
    let inbox = flow_bnb::agentic_handoff::Inbox::open(c).unwrap();
    assert_eq!(
        inbox.status(queued["intent_id"].as_str().unwrap()).unwrap()["state"],
        "queued"
    );
    // MCP can initialize and exit with a saved request present; startup does not execute it.
    let mut child = f
        .command(&["mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"startup-test","version":"1"}}})).unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(serde_json::from_str::<Value>(&line)
        .unwrap()
        .get("result")
        .is_some());
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(f.swaps(), 0);
}
