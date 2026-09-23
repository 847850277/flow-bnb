use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

const TX: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BLOCK: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn serve(pending: bool) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!(
        "http://{}/provider-key-do-not-log",
        listener.local_addr().unwrap()
    );
    let server = thread::spawn(move || {
        let methods = if pending {
            vec!["eth_chainId", "eth_getTransactionReceipt"]
        } else {
            vec![
                "eth_chainId",
                "eth_getTransactionReceipt",
                "eth_blockNumber",
                "eth_getBlockByNumber",
            ]
        };
        for method in methods {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "RPC client never sent {method}");
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut headers = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
                headers.push_str(&line);
            }
            assert!(!headers.to_ascii_lowercase().contains("x-oc-"));
            assert!(!headers.to_ascii_lowercase().contains("authorization:"));
            assert!(!headers.contains("fake-binance-secret"));
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["method"], method);
            let result = match method {
                "eth_chainId" => json!("0x38"),
                "eth_getTransactionReceipt" if pending => Value::Null,
                "eth_getTransactionReceipt" => {
                    json!({"transactionHash":TX,"blockHash":BLOCK,"blockNumber":"0x10","status":"0x1","gasUsed":"0x5208"})
                }
                "eth_blockNumber" => json!("0x11"),
                _ => json!({"hash":BLOCK,"number":"0x10"}),
            };
            let response = json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).unwrap();
        }
    });
    (url, server)
}

#[test]
fn cli_returns_json_persists_reports_and_uses_exit_status_without_binance_auth() {
    for pending in [false, true] {
        let (url, server) = serve(pending);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("receipt.json");
        let output = Command::new(env!("CARGO_BIN_EXE_flow-bnb"))
            .args([
                "watch-transaction",
                "--tx-hash",
                TX,
                "--confirmations",
                "2",
                "--max-iterations",
                "1",
                "--report",
            ])
            .arg(&path)
            .env("FLOW_BNB_RPC_URL", url)
            .env("BINANCE_WEB3_API_KEY", "fake-binance-key")
            .env("BINANCE_WEB3_SECRET_KEY", "fake-binance-secret")
            .output()
            .unwrap();
        server.join().unwrap();
        assert_eq!(
            output.status.success(),
            !pending,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["success"], !pending);
        assert_eq!(
            report["outcome"],
            if pending {
                "max_iterations"
            } else {
                "confirmed"
            }
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(path).unwrap()).unwrap(),
            report
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("provider-key-do-not-log"));
    }
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
}
impl Mcp {
    fn start(root: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_flow-bnb-mcp"))
            .arg("--root")
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_remove("BINANCE_WEB3_API_KEY")
            .env_remove("BINANCE_WEB3_SECRET_KEY")
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
        Self {
            child,
            stdin,
            messages,
        }
    }
    fn send(&mut self, value: Value) {
        writeln!(self.stdin, "{value}").unwrap();
        self.stdin.flush().unwrap();
    }
    fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        loop {
            let message = self
                .messages
                .recv_timeout(Duration::from_secs(10))
                .expect("MCP response timed out");
            if message["id"] == id {
                assert!(message.get("error").is_none(), "{message}");
                return message["result"].clone();
            }
        }
    }
}
impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_exposes_template_and_executes_read_only_tracking_without_api_credentials() {
    let (url, server) = serve(false);
    let directory = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::start(directory.path());
    mcp.call(1,"initialize",json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"receipt-test","version":"1"}}));
    mcp.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    let tools = mcp.call(2, "tools/list", json!({}));
    assert!(tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "watch_transaction"));
    let generated = mcp.call(
        3,
        "tools/call",
        json!({"name":"generate_bnb_flow","arguments":{"template":"transaction_receipt"}}),
    );
    assert_ne!(generated["isError"], true, "{generated}");
    assert!(generated.to_string().contains("repeat_until"));
    let result=mcp.call(4,"tools/call",json!({"name":"watch_transaction","arguments":{"rpc_url":url,"tx_hash":TX,"confirmations":2,"max_iterations":1}}));
    server.join().unwrap();
    assert_ne!(result["isError"], true, "{result}");
    let report = &result["structuredContent"];
    assert_eq!(report["outcome"], "confirmed", "{result}");
    assert_eq!(report["success"], true);
    assert!(!result.to_string().contains("provider-key-do-not-log"));
}

#[test]
fn verbose_http_details_go_to_stderr_and_keep_rpc_endpoint_redacted() {
    for flag in ["-v", "--verbose", ""] {
        let (url, server) = serve(false);
        let mut command = Command::new(env!("CARGO_BIN_EXE_flow-bnb"));
        command
            .args(["watch-transaction", "--tx-hash", TX, "--confirmations", "2"])
            .env("FLOW_BNB_RPC_URL", url)
            .env_remove("BINANCE_WEB3_API_KEY")
            .env_remove("BINANCE_WEB3_SECRET_KEY");
        if !flag.is_empty() {
            command.arg(flag);
        }
        let output = command.output().unwrap();
        server.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["outcome"], "confirmed");
        let logs = String::from_utf8_lossy(&output.stderr);
        assert!(!logs.contains("provider-key-do-not-log"));
        if flag.is_empty() {
            assert!(!logs.contains("executing HTTP step"));
        } else {
            for detail in [
                "executing HTTP step",
                "request header",
                "request body",
                "HTTP response received",
                "response header",
                "response body",
                "[REDACTED]",
                "elapsed_ms",
            ] {
                assert!(logs.contains(detail), "missing {detail}: {logs}");
            }
        }
    }
}
