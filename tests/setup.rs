//! Onboarding is exercised with a fake backend; these tests cannot submit transactions.
use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("flow setup ' ")
            .tempdir()
            .unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        script(
            &bin.join("node"),
            r#"#!/bin/sh
if [ "$1" = '--version' ]; then printf 'v22.23.3\n'; exit 0; fi
case "$1" in
*/npm)
  printf 'install\n' >> "$FLOW_SETUP_TEST_HOME/calls"
  if [ "$FLOW_SETUP_FAIL_INSTALL" = '1' ]; then exit 1; fi
  while [ "$#" -gt 0 ]; do
    if [ "$1" = '--prefix' ]; then shift; prefix="$1"; fi
    shift
  done
  /bin/mkdir -p "$prefix/node_modules/.bin" "$prefix/node_modules/@binance/agentic-wallet/dist"
  /bin/cp "$FLOW_SETUP_TEST_HOME/backend" "$prefix/node_modules/.bin/baw"
  /bin/cp "$FLOW_SETUP_TEST_HOME/backend" "$prefix/node_modules/@binance/agentic-wallet/dist/index.js"
  ;;
*) exec /bin/sh "$@" ;;
esac
"#,
        );
        script(&bin.join("npm"), "#!/bin/sh\nexit 99\n");
        script(
            &dir.path().join("backend"),
            r#"#!/bin/sh
if [ -n "$BINANCE_WEB3_API_KEY" ] || [ -n "$BINANCE_WEB3_SECRET_KEY" ]; then exit 88; fi
printf '%s %s\n' "$1" "$2" >> "$FLOW_SETUP_TEST_HOME/calls"
case "$1 $2" in
'--version ') printf '1.10.0\n' ;;
'wallet status')
 if [ -f "$FLOW_SETUP_TEST_HOME/connected" ]; then state=CONNECTED; else state=UNCONNECTED; fi
 printf '{"success":true,"data":{"status":"%s"}}\n' "$state" ;;
'wallet address')
 address="${FLOW_SETUP_TEST_ADDRESS:-0xDaD97288C1fcc449D499b7Aa578d1960fdAEeA23}"
 printf '{"success":true,"data":{"addresses":[{"binanceChainId":"56","address":"%s"}]}}\n' "$address" ;;
'auth signout')
 /bin/rm -f "$FLOW_SETUP_TEST_HOME/connected"
 printf '{"success":true,"data":{}}\n' ;;
'auth signin') printf '{"success":true,"data":{"urlForWeb":"https://web3.binance.com/en/agent-login?test=1","pairingCode":"001234","qrCodeId":"test-only"}}\n' ;;
'auth verify')
 if [ "$FLOW_SETUP_DELAY_VERIFY" = '1' ]; then /bin/sleep 1; fi
 : > "$FLOW_SETUP_TEST_HOME/connected"
 printf '{"success":true,"data":{"status":"SUCCESS"}}\n' ;;
*) exit 89 ;;
esac
"#,
        );
        Self { dir }
    }
    fn run(&self, args: &[&str], extra: &[(&str, &str)]) -> std::process::Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_flow-bnb"));
        c.current_dir(self.dir.path())
            .args(args)
            .env_remove("FLOW_BNB_AGENTIC_CONFIG")
            .env("PATH", self.dir.path().join("bin"))
            .env("FLOW_SETUP_TEST_HOME", self.dir.path())
            .env("BINANCE_WEB3_API_KEY", "must-not-reach-wallet")
            .env("BINANCE_WEB3_SECRET_KEY", "must-not-reach-wallet")
            .envs(extra.iter().copied());
        c.output().unwrap()
    }
    fn config(&self) -> std::path::PathBuf {
        self.dir.path().join(".flow-bnb/agentic.json")
    }
    fn calls(&self) -> String {
        fs::read_to_string(self.dir.path().join("calls")).unwrap()
    }
}
fn script(p: &Path, s: &str) {
    fs::write(p, s).unwrap();
    fs::set_permissions(p, fs::Permissions::from_mode(0o700)).unwrap();
}
fn succeeds(o: &std::process::Output) {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(
        o.stdout.is_empty(),
        "setup must not leak auth/config to stdout"
    );
}

#[test]
fn fresh_install_login_repeat_and_doctor_preserve_state() {
    let f = Fixture::new();
    let o = f.run(&["setup", "--no-open"], &[]);
    succeeds(&o);
    assert!(String::from_utf8_lossy(&o.stderr).contains("001234"));
    let before = fs::read(f.config()).unwrap();
    let c: Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(c["tokens"][0]["max_sell_amount"], "6");
    let lock = Path::new(c["state_dir"].as_str().unwrap()).join("agentic-wallet.lock");
    fs::write(&lock, b"unresolved-order").unwrap();
    succeeds(&f.run(&["setup", "--no-open"], &[]));
    let doctor = f.run(&["doctor"], &[]);
    succeeds(&doctor);
    assert!(!String::from_utf8_lossy(&doctor.stderr).contains("交易提交锁"));
    assert_eq!(before, fs::read(f.config()).unwrap());
    assert_eq!(fs::read(lock).unwrap(), b"unresolved-order");
    assert_eq!(f.calls().lines().filter(|l| *l == "install").count(), 1);
    assert_eq!(f.calls().lines().filter(|l| *l == "auth signin").count(), 1);
    let mcp: Value =
        serde_json::from_slice(&fs::read(f.dir.path().join(".flow-bnb/mcp.json")).unwrap())
            .unwrap();
    assert_eq!(mcp["mcpServers"]["flow-bnb"]["args"][0], "mcp");
    assert_eq!(
        mcp["mcpServers"]["flow-bnb"]["env"]["FLOW_BNB_AGENTIC_CONFIG"],
        fs::canonicalize(f.config()).unwrap().to_str().unwrap()
    );
    // Managed launcher embeds absolute paths and works in a desktop client's empty PATH.
    let out = Command::new(c["executable"].as_str().unwrap())
        .arg("--version")
        .env("PATH", "/nonexistent")
        .env("FLOW_SETUP_TEST_HOME", f.dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout, b"1.10.0\n");
    assert!(!f.calls().contains("market-order"));
}

#[test]
fn failed_install_retries_without_publishing_partial_package() {
    let f = Fixture::new();
    let o = f.run(&["setup", "--no-open"], &[("FLOW_SETUP_FAIL_INSTALL", "1")]);
    assert!(!o.status.success());
    assert!(!f.config().exists());
    let managed = f.dir.path().join(".flow-bnb/managed");
    assert_eq!(fs::read_dir(managed).unwrap().count(), 0);
    succeeds(&f.run(&["setup", "--no-open"], &[]));
    assert_eq!(f.calls().lines().filter(|l| *l == "install").count(), 2);
}

#[test]
fn no_login_can_resume_and_account_mismatch_never_rebinds() {
    let f = Fixture::new();
    succeeds(&f.run(&["setup", "--no-login"], &[]));
    assert!(!f.config().exists());
    assert!(!f.calls().contains("auth signin"));
    succeeds(&f.run(&["setup", "--no-open"], &[]));
    let before = fs::read(f.config()).unwrap();
    let o = f.run(
        &["setup", "--no-open"],
        &[(
            "FLOW_SETUP_TEST_ADDRESS",
            "0x0000000000000000000000000000000000000001",
        )],
    );
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("不符"));
    assert_eq!(before, fs::read(f.config()).unwrap());
}

#[test]
fn reserved_config_name_is_rejected_before_installation() {
    let f = Fixture::new();
    let o = f.run(
        &["setup", "--config", ".flow-bnb/mcp.json", "--no-login"],
        &[],
    );
    assert!(!o.status.success());
    assert!(!f.dir.path().join(".flow-bnb").exists());
    assert!(!f.dir.path().join("calls").exists());
}

#[test]
fn connector_status_logout_and_relogin_preserve_policy_and_order_locks() {
    let f = Fixture::new();
    let missing = f.run(&["connection-status"], &[]);
    assert!(!missing.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&missing.stdout).unwrap()["connected"],
        false
    );
    assert!(!f.config().exists());
    succeeds(&f.run(&["setup", "--no-open"], &[]));
    let before = fs::read(f.config()).unwrap();
    let lock = f.config().parent().unwrap().join("unresolved.lock");
    fs::write(&lock, "pending").unwrap();
    let ok = f.run(&["connection-status"], &[]);
    assert!(ok.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&ok.stdout).unwrap()["connected"],
        true
    );
    let wrong = f.run(
        &["connection-status"],
        &[(
            "FLOW_SETUP_TEST_ADDRESS",
            "0x0000000000000000000000000000000000000001",
        )],
    );
    assert!(!wrong.status.success());
    let logout = f.run(&["disconnect"], &[]);
    assert!(logout.status.success());
    assert!(!f.run(&["connection-status"], &[]).status.success());
    assert!(f.run(&["disconnect"], &[]).status.success());
    succeeds(&f.run(&["setup", "--no-open"], &[]));
    assert!(f.run(&["connection-status"], &[]).status.success());
    assert_eq!(before, fs::read(f.config()).unwrap());
    assert_eq!(fs::read_to_string(lock).unwrap(), "pending");
    assert!(!f.calls().contains("market-order"));
}

#[test]
fn runtime_bootstrap_does_not_install_wallet_or_login() {
    let f = Fixture::new();
    let o = f.run(&["prepare-runtime"], &[]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout)
        .trim()
        .ends_with("/bin/node"));
    assert!(!f.config().exists());
    assert!(!f.dir.path().join("calls").exists());
}

#[test]
fn mcp_pairing_returns_promptly_and_uses_bound_root_without_trading() {
    use serde_json::json;
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::{sync::mpsc, thread, time::Duration};
    let f = Fixture::new();
    let mut child = Command::new(env!("CARGO_BIN_EXE_flow-bnb"))
        .args(["mcp", "--root"])
        .arg(f.dir.path())
        .env_remove("FLOW_BNB_AGENTIC_CONFIG")
        .env("PATH", f.dir.path().join("bin"))
        .env("FLOW_SETUP_TEST_HOME", f.dir.path())
        .env("FLOW_SETUP_DELAY_VERIFY", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            if tx
                .send(serde_json::from_str::<Value>(&line.unwrap()).unwrap())
                .is_err()
            {
                break;
            }
        }
    });
    let mut sequence = 0;
    let mut call = |method: &str, params: Value| {
        sequence += 1;
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","id":sequence,"method":method,"params":params})
        )
        .unwrap();
        input.flush().unwrap();
        loop {
            let v = rx.recv_timeout(Duration::from_secs(10)).unwrap();
            if v["id"] == sequence {
                assert!(v.get("error").is_none(), "{v}");
                return v["result"].clone();
            }
        }
    };
    call(
        "initialize",
        json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"pairing-test","version":"1"}}),
    );
    // rmcp accepts tool calls after initialization response.
    let before = call(
        "tools/call",
        json!({"name":"get_bnb_connection","arguments":{}}),
    );
    assert_eq!(before["structuredContent"]["connected"], false);
    assert!(!f.config().exists());
    let start = call(
        "tools/call",
        json!({"name":"connect_bnb_wallet","arguments":{}}),
    );
    assert_eq!(start["structuredContent"]["phase"], "preparing");
    let mut saw_pairing = false;
    let mut connected = false;
    for _ in 0..100 {
        let result = call(
            "tools/call",
            json!({"name":"get_bnb_connection","arguments":{}}),
        );
        let state = &result["structuredContent"];
        if state["phase"] == "awaiting_wallet" {
            saw_pairing = true;
            assert_eq!(state["pairing_code"], "001234");
            assert!(state["login_url"]
                .as_str()
                .unwrap()
                .starts_with("https://web3.binance.com/"));
            let repeat = call(
                "tools/call",
                json!({"name":"connect_bnb_wallet","arguments":{}}),
            );
            // Pairing may finish between these calls. A repeated connect then
            // rechecks the already-connected wallet and reports preparing.
            assert!(
                matches!(
                    repeat["structuredContent"]["phase"].as_str(),
                    Some("awaiting_wallet" | "preparing")
                ),
                "{repeat}"
            );
        }
        if state["connected"] == true {
            assert_eq!(state["execution_mode"], "direct");
            assert_eq!(state["confirmation_required"], false);
            assert_eq!(state["tokens"][0]["symbol"], "USDT");
            assert_eq!(state["max_slippage_bps"], 50);
            connected = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(saw_pairing && connected);
    let c: Value = serde_json::from_slice(&fs::read(f.config()).unwrap()).unwrap();
    assert!(c["state_dir"]
        .as_str()
        .unwrap()
        .starts_with(f.dir.path().canonicalize().unwrap().to_str().unwrap()));
    assert_eq!(f.calls().lines().filter(|l| *l == "auth signin").count(), 1);
    assert!(!f.calls().contains("market-order"));
}
