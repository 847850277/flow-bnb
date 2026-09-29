//! Agentic Wallet native orders. Flow executes local-adapter stages and bounded
//! polling. This is a different backend from exact-calldata external signing.
use anyhow::{anyhow, bail, ensure, Context, Result};
use futures::StreamExt;
use num_bigint::BigUint;
use postman_flow::{
    compile_flow, execute_flow, parse_flow_yaml, CompileEnvironment, FlowEvent, FlowInputs,
    FlowSessionEnvironment,
};
use postman_http::{
    request::{HttpMethod, RedirectPolicy, Request, RequestBody, RequestOptions},
    HttpError, HttpResponse, HttpTransport,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::AsyncReadExt;

pub const STAGE: &str = include_str!("../flows/agentic_stage.http.yml");
pub const ORDER: &str = include_str!("../flows/agentic_order.http.yml");
const URL: &str = "https://agentic-wallet.invalid/local";
const TRANSFER: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub from_token: String,
    pub to_token: String,
    /// Human-readable decimal units, unlike the legacy TradeRequest.amount.
    pub amount: String,
    pub slippage_bps: u32,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenRule {
    pub address: String,
    pub symbol: String,
    pub decimals: u32,
    pub max_sell_amount: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub executable: PathBuf,
    pub wallet_address: String,
    pub rpc_url: String,
    pub max_slippage_bps: u32,
    pub state_dir: PathBuf,
    #[serde(skip)]
    pub trace: Arc<Mutex<Vec<Value>>>,
    pub tokens: Vec<TokenRule>,
}
impl Config {
    pub fn read(path: &Path) -> Result<Self> {
        let c: Self = serde_json::from_slice(&fs::read(path)?)?;
        ensure!(
            c.executable.is_absolute() && c.executable.is_file(),
            "configure an absolute baw executable path"
        );
        address(&c.wallet_address)?;
        ensure!(c.state_dir.is_absolute(), "state_dir must be absolute");
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&c.state_dir)?;
        ensure!(
            fs::metadata(&c.state_dir)?.permissions().mode() & 0o077 == 0,
            "state_dir must be private (0700)"
        );
        ensure!(
            !fs::symlink_metadata(&c.state_dir)?.file_type().is_symlink(),
            "state_dir cannot be a symlink"
        );
        let u = url::Url::parse(&c.rpc_url)?;
        ensure!(
            matches!(u.scheme(), "http" | "https")
                && u.host_str().is_some()
                && u.username().is_empty()
                && u.password().is_none()
                && u.fragment().is_none(),
            "invalid RPC URL"
        );
        ensure!(
            c.max_slippage_bps <= 100,
            "local slippage cap must be at most 1%"
        );
        let mut seen = std::collections::HashSet::new();
        for t in &c.tokens {
            address(&t.address)?;
            ensure!(
                seen.insert(t.address.to_lowercase()) && t.decimals <= 36 && !t.symbol.is_empty(),
                "invalid/duplicate token rule"
            );
            ensure!(
                units(&t.max_sell_amount, t.decimals)? > BigUint::from(0u8),
                "invalid token limit"
            );
        }
        Ok(c)
    }
    pub fn rules(&self, i: &Intent) -> Result<(&TokenRule, &TokenRule)> {
        address(&i.from_token)?;
        address(&i.to_token)?;
        ensure!(
            !i.from_token.eq_ignore_ascii_case(&i.to_token),
            "tokens must differ"
        );
        ensure!(
            i.slippage_bps <= self.max_slippage_bps,
            "slippage exceeds local limit"
        );
        let find = |a: &str| {
            self.tokens
                .iter()
                .find(|t| t.address.eq_ignore_ascii_case(a))
                .context("token outside local allowlist")
        };
        let sell = find(&i.from_token)?;
        let buy = find(&i.to_token)?;
        let amount = units(&i.amount, sell.decimals)?;
        ensure!(
            amount > BigUint::from(0u8) && amount <= units(&sell.max_sell_amount, sell.decimals)?,
            "amount exceeds local token limit or is zero"
        );
        Ok((sell, buy))
    }
}
fn address(a: &str) -> Result<()> {
    ensure!(
        a.len() == 42
            && a.starts_with("0x")
            && a[2..].bytes().all(|b| b.is_ascii_hexdigit())
            && a[2..].bytes().any(|b| b != b'0'),
        "invalid address"
    );
    Ok(())
}
pub fn units(s: &str, decimals: u32) -> Result<BigUint> {
    ensure!(decimals <= 36 && s.len() <= 120, "invalid decimal size");
    let parts: Vec<_> = s.split('.').collect();
    ensure!(
        !parts[0].is_empty()
            && parts.len() <= 2
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
        "amount must be a plain decimal string"
    );
    let fraction = parts.get(1).copied().unwrap_or("");
    ensure!(
        fraction.len() <= decimals as usize,
        "too many decimal places"
    );
    let amount = BigUint::parse_bytes(
        format!(
            "{}{}{}",
            parts[0],
            fraction,
            "0".repeat(decimals as usize - fraction.len())
        )
        .as_bytes(),
        10,
    )
    .context("invalid amount")?;
    ensure!(amount.bits() <= 256, "amount exceeds uint256");
    Ok(amount)
}
pub fn decimal(n: &BigUint, decimals: u32) -> String {
    if decimals == 0 {
        return n.to_string();
    }
    let s = format!("{:0>width$}", n.to_string(), width = decimals as usize + 1);
    let cut = s.len() - decimals as usize;
    let value = format!("{}.{}", &s[..cut], &s[cut..]);
    value
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}
fn slippage(i: &Intent) -> String {
    decimal(&BigUint::from(i.slippage_bps), 2)
}
#[derive(Clone)]
struct Adapter {
    config: Config,
    last: Arc<Mutex<Option<Value>>>,
}
fn args(v: &Value) -> Result<Vec<String>> {
    let op = v["op"].as_str().context("missing operation")?;
    let mut a: Vec<String> = match op {
        "status" | "address" | "settings" => vec!["wallet".into(), op.into()],
        "lock" => vec![
            "wallet".into(),
            "tx-lock".into(),
            "--binanceChainId".into(),
            "56".into(),
        ],
        "quote" | "swap" => {
            let i: Intent = serde_json::from_value(v["intent"].clone())?;
            address(&i.from_token)?;
            address(&i.to_token)?;
            units(&i.amount, 36)?;
            ensure!(i.slippage_bps <= 100, "invalid slippage");
            let mut args = vec![
                "market-order".into(),
                op.into(),
                "--binanceChainId".into(),
                "56".into(),
                "--fromTokenQty".into(),
                i.amount.clone(),
                "--fromToken".into(),
                i.from_token.clone(),
                "--toToken".into(),
                i.to_token.clone(),
                "--slippage".into(),
                slippage(&i),
            ];
            if op == "swap" {
                args.extend(["--mev".into(), "true".into()]);
            }
            args
        }
        "order" => {
            let id = v["order_id"].as_str().context("missing order ID")?;
            valid_id(id)?;
            vec![
                "market-order".into(),
                "list".into(),
                "--orderId".into(),
                id.into(),
            ]
        }
        _ => bail!("unsupported local operation"),
    };
    a.push("--json".into());
    Ok(a)
}
fn valid_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty() && id.len() <= 80 && id.bytes().all(|b| b.is_ascii_digit()),
        "invalid order ID"
    );
    Ok(())
}
async fn process(path: &Path, arguments: &[String]) -> Result<Value> {
    use std::process::Stdio;
    let mut child = tokio::process::Command::new(path)
        .args(arguments)
        .env_remove("BINANCE_WEB3_API_KEY")
        .env_remove("BINANCE_WEB3_SECRET_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("cannot start baw")?;
    let run = async {
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .context("missing stdout")?
            .take(1_048_577)
            .read_to_end(&mut bytes)
            .await?;
        ensure!(bytes.len() <= 1_048_576, "baw response too large");
        ensure!(
            child.wait().await?.success(),
            "baw failed; if submission started, inspect order history before retrying"
        );
        serde_json::from_slice(&bytes).context("invalid baw JSON")
    };
    tokio::time::timeout(Duration::from_secs(20), run)
        .await
        .map_err(|_| anyhow!("baw timed out; if submission started, outcome is unknown"))?
}
impl HttpTransport for Adapter {
    async fn execute(&self, r: Request, _: RequestOptions) -> Result<HttpResponse, HttpError> {
        let run = async {
            ensure!(
                r.url == URL && r.method == HttpMethod::POST,
                "unexpected adapter request"
            );
            let RequestBody::Json(body) = r.body else {
                bail!("expected JSON")
            };
            let command: Value = serde_json::from_str(&body)?;
            let started = std::time::Instant::now();
            let op = command["op"].as_str().context("missing operation")?;
            let result = match op {
                "rpc" => rpc_direct(
                    &self.config,
                    command["method"].as_str().context("missing RPC method")?,
                    command["params"].clone(),
                )
                .await
                .map(|v| json!({"success":true,"data":v})),
                "audit" => audit_direct(command["token"].as_str().context("missing audit token")?)
                    .await
                    .map(|v| json!({"success":true,"data":v})),
                _ => process(&self.config.executable, &args(&command)?).await,
            };
            let mut evidence = json!({"operation":op,"method":command.get("method"),"elapsed_ms":started.elapsed().as_millis(),"success":false});
            if let Ok(v) = &result {
                use sha2::{Digest, Sha256};
                evidence["success"] = json!(v["success"] == true);
                evidence["response_sha256"] =
                    json!(format!("{:x}", Sha256::digest(serde_json::to_vec(v)?)));
            }
            self.config.trace.lock().unwrap().push(evidence);
            let v = result?;
            *self.last.lock().unwrap() = Some(v.clone());
            Ok::<_, anyhow::Error>(HttpResponse::new(200, vec![], v.to_string()))
        };
        run.await.map_err(|_| {
            HttpError::network(
                "Agentic Wallet operation failed; check durable report before retrying",
            )
        })
    }
}
async fn flow<T: HttpTransport>(template: &str, command: Value, transport: T) -> Result<Value> {
    let doc = parse_flow_yaml(template)?;
    let plan = compile_flow(&doc.flow, &doc.apis, &CompileEnvironment::default())
        .map_err(|_| anyhow!("agentic flow compilation failed"))?;
    let session = FlowSessionEnvironment::new(FlowInputs::new().with("command", command));
    let stream = execute_flow(plan, transport, session)?;
    let mut stream = std::pin::pin!(stream);
    while let Some(e) = stream.next().await {
        if let FlowEvent::FlowFinished { success, outputs } = e? {
            ensure!(
                success,
                "Agentic Wallet flow failed or polling ended; inspect report, do not resubmit"
            );
            return Ok(outputs
                .get("data")
                .map(|v| v.value().clone())
                .unwrap_or(Value::Null));
        }
    }
    bail!("flow ended without result")
}
async fn call(c: &Config, command: Value) -> Result<Value> {
    flow(
        STAGE,
        command,
        Adapter {
            config: c.clone(),
            last: Default::default(),
        },
    )
    .await
}
async fn rpc(c: &Config, method: &str, params: Value) -> Result<Value> {
    call(c, json!({"op":"rpc","method":method,"params":params})).await
}
async fn rpc_direct(c: &Config, method: &str, params: Value) -> Result<Value> {
    ensure!(
        matches!(
            method,
            "eth_chainId" | "eth_call" | "eth_getBalance" | "eth_getTransactionReceipt"
        ),
        "RPC method not allowed"
    );
    let client = postman_request::RequestClient::try_new("flow-bnb-agentic")?;
    let mut r = Request::new(HttpMethod::POST, &c.rpc_url);
    r.headers = vec![("Content-Type".into(), "application/json".into())];
    r.body = RequestBody::Json(
        json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string(),
    );
    let res = client
        .execute(
            r,
            RequestOptions {
                timeout_ms: Some(10000),
                redirect_policy: RedirectPolicy::DoNotFollow,
                ..Default::default()
            },
        )
        .await
        .map_err(|_| anyhow!("RPC transport failed"))?;
    ensure!(res.status == 200, "RPC HTTP failure");
    let v: Value = serde_json::from_str(&res.body)?;
    ensure!(
        v["id"] == 1 && v["jsonrpc"] == "2.0" && v.get("error").is_none(),
        "RPC response failure"
    );
    v.get("result").cloned().context("RPC result missing")
}
async fn audit_direct(token: &str) -> Result<Value> {
    address(token)?;
    let client = postman_request::RequestClient::try_new("flow-bnb-agentic-audit")?;
    let mut r = Request::new(
        HttpMethod::POST,
        "https://web3.binance.com/bapi/defi/v1/public/wallet-direct/security/token/audit",
    );
    r.headers = vec![
        ("Content-Type".into(), "application/json".into()),
        ("Accept-Encoding".into(), "identity".into()),
        ("User-Agent".into(), "binance-web3/1.4 (Skill)".into()),
        ("source".into(), "agent".into()),
    ];
    // Generate a UUID v4 from OS randomness without exposing wallet credentials.
    use std::io::Read;
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let h: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let id = format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    );
    r.body = RequestBody::Json(
        json!({"binanceChainId":"56","contractAddress":token,"requestId":id}).to_string(),
    );
    let res = client
        .execute(
            r,
            RequestOptions {
                timeout_ms: Some(10000),
                redirect_policy: RedirectPolicy::DoNotFollow,
                ..Default::default()
            },
        )
        .await
        .map_err(|_| anyhow!("token audit unavailable"))?;
    ensure!(res.status == 200, "token audit HTTP failure");
    let v: Value = serde_json::from_str(&res.body)?;
    ensure!(
        v["success"] == true && v["code"] == "000000",
        "token audit API failure"
    );
    Ok(v["data"].clone())
}
fn check_audit(a: &Value) -> Result<()> {
    ensure!(
        a["hasResult"] == true && a["isSupported"] == true,
        "token audit unavailable; preparation blocked"
    );
    ensure!(
        a["riskLevel"].as_u64().is_some_and(|n| n <= 1),
        "token audit requires risk review"
    );
    let groups = a["riskItems"]
        .as_array()
        .context("audit risk items missing")?;
    for g in groups {
        for d in g["details"].as_array().context("audit details missing")? {
            ensure!(
                d["isHit"] == false,
                "token audit has flagged items; review required"
            );
        }
    }
    for field in ["buyTax", "sellTax"] {
        let n = a["extraInfo"][field]
            .as_str()
            .context("unknown token tax")?
            .parse::<f64>()?;
        ensure!(
            n.is_finite() && (0.0..=5.0).contains(&n),
            "unknown or excessive token tax"
        );
    }
    Ok(())
}
fn hex(v: &Value) -> Result<BigUint> {
    let s = v.as_str().context("expected hex")?;
    ensure!(
        s.starts_with("0x") && s.len() > 2 && s.len() <= 66,
        "invalid hex quantity"
    );
    BigUint::parse_bytes(&s.as_bytes()[2..], 16).context("invalid hex")
}
async fn token_call(c: &Config, t: &TokenRule, data: String) -> Result<BigUint> {
    hex(&rpc(
        c,
        "eth_call",
        json!([{"to":t.address,"data":data},"latest"]),
    )
    .await?)
}
#[derive(Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub backend: String,
    pub state: String,
    pub intent: Intent,
    pub wallet_address: String,
    pub config_sha256: String,
    pub prepared_at: String,
    pub stages: Vec<Value>,
    pub balances: Value,
    pub quote: Value,
    pub wallet_settings: Value,
    pub token_audit: Value,
    pub order_id: Option<String>,
    pub order: Value,
    pub settlement: Value,
    pub error: Option<String>,
}
impl Report {
    /// Settlement was observed, but discrepancies may still require review.
    pub fn has_settlement(&self) -> bool {
        matches!(
            self.state.as_str(),
            "completed" | "settled_with_discrepancy"
        )
    }
}
pub(crate) fn digest(c: &Config) -> Result<String> {
    use sha2::{Digest, Sha256};
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(c)?)))
}
/// Read-only and usable by MCP. No swap is invoked by preparation.
pub async fn prepare(c: &Config, i: Intent) -> Result<Report> {
    c.trace.lock().unwrap().clear();
    let (sell, buy) = c.rules(&i)?;
    let mut r = Report {
        schema_version: 1,
        backend: "agentic_wallet".into(),
        state: "preparing".into(),
        intent: i.clone(),
        wallet_address: c.wallet_address.clone(),
        config_sha256: digest(c)?,
        prepared_at: chrono::Utc::now().to_rfc3339(),
        stages: vec![],
        balances: Value::Null,
        quote: Value::Null,
        wallet_settings: Value::Null,
        token_audit: Value::Null,
        order_id: None,
        order: Value::Null,
        settlement: Value::Null,
        error: None,
    };
    let result = async {
        let status = call(c, json!({"op":"status"})).await?;
        ensure!(
            status["status"] == "CONNECTED",
            "Agentic Wallet is not connected"
        );
        let addresses = call(c, json!({"op":"address"})).await?;
        ensure!(
            addresses["addresses"]
                .as_array()
                .is_some_and(|a| a.iter().any(|x| x["binanceChainId"] == "56"
                    && x["address"]
                        .as_str()
                        .is_some_and(|a| a.eq_ignore_ascii_case(&c.wallet_address)))),
            "connected BSC wallet mismatch"
        );
        r.wallet_settings = call(c, json!({"op":"settings"})).await?;
        let lock = call(c, json!({"op":"lock"})).await?;
        ensure!(
            lock["status"] == "UNLOCKED",
            "wallet has a pending transaction or confirmation"
        );
        ensure!(
            hex(&rpc(c, "eth_chainId", json!([])).await?)? == BigUint::from(56u8),
            "RPC chain mismatch"
        );
        for t in [sell, buy] {
            ensure!(
                token_call(c, t, "0x313ce567".into()).await? == BigUint::from(t.decimals),
                "token decimals differ from local config"
            );
        }
        let balance = token_call(
            c,
            sell,
            format!("0x70a08231{:0>64}", &c.wallet_address[2..]),
        )
        .await?;
        ensure!(
            balance >= units(&i.amount, sell.decimals)?,
            "insufficient sell balance"
        );
        let gas = hex(&rpc(c, "eth_getBalance", json!([c.wallet_address, "latest"])).await?)?;
        ensure!(gas > BigUint::from(0u8), "no BNB for gas");
        r.balances =
            json!({"sell_balance":decimal(&balance,sell.decimals),"bnb_balance":decimal(&gas,18)});
        r.quote = call(c, json!({"op":"quote","intent":i})).await?;
        validate_quote(&r.quote, &i, sell, buy)?;
        // Local token allowlisting does not automatically waive the token audit.
        if !buy
            .address
            .eq_ignore_ascii_case("0x55d398326f99059fF775485246999027B3197955")
        {
            r.token_audit = call(c, json!({"op":"audit","token":buy.address})).await?;
            if r.token_audit["hasResult"] != true || r.token_audit["isSupported"] != true {
                r.token_audit =
                    json!({"status":"unavailable","hasResult":false,"isSupported":false});
                bail!("token audit unavailable; preparation blocked");
            }
            check_audit(&r.token_audit)?;
        } else {
            r.token_audit = json!({"status":"builtin_trusted_bsc_usdt"});
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    match result {
        Ok(()) => r.state = "ready".into(),
        Err(e) => {
            r.state = "blocked".into();
            r.error = Some(e.to_string());
        }
    }
    r.stages = c.trace.lock().unwrap().clone();
    Ok(r)
}
fn validate_quote(q: &Value, i: &Intent, sell: &TokenRule, buy: &TokenRule) -> Result<()> {
    ensure!(
        q["fromCoinSymbol"] == sell.symbol && q["toCoinSymbol"] == buy.symbol,
        "quote symbols differ from configured pair"
    );
    ensure!(
        units(
            q["fromCoinAmount"]
                .as_str()
                .context("quote amount missing")?,
            sell.decimals
        )? == units(&i.amount, sell.decimals)?,
        "quote sell amount mismatch"
    );
    ensure!(
        units(
            q["toCoinAmount"].as_str().context("quote output missing")?,
            buy.decimals
        )? > BigUint::from(0u8),
        "empty quote"
    );
    let slip = q["slippage"].as_f64().context("quote slippage missing")?;
    ensure!(
        slip.is_finite() && (slip - i.slippage_bps as f64 / 10000.0).abs() < 1e-10,
        "quote slippage mismatch"
    );
    Ok(())
}
fn save(file: &mut fs::File, r: &Report) -> Result<()> {
    file.seek(SeekFrom::Start(0))?;
    serde_json::to_writer_pretty(&mut *file, r)?;
    let end = file.stream_position()?;
    file.set_len(end)?;
    file.sync_all()?;
    Ok(())
}
/// Operator-owned CLI entry point. No model-facing tool calls this execute path.
pub async fn run(c: Config, i: Intent, path: &Path, execute: bool) -> Result<Report> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .context("report exists or cannot be created; use track to resume an existing order")?;
    file.try_lock().context("report is in use")?;
    let mut r = prepare(&c, i).await?;
    save(&mut file, &r)?;
    if !execute || r.state != "ready" {
        return Ok(r);
    }
    println!("{}", serde_json::to_string_pretty(&r)?);
    let mut tty = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("execution requires an operator terminal")?;
    writeln!(tty,"Native Agentic Wallet execution: quoted output may change. Local limits cover token amounts and slippage, not USD notional, price impact or independent simulation. Wallet risk checks still apply.\nType CONFIRM to submit this one order (anything else cancels):")?;
    tty.flush()?;
    let mut answer = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(tty), &mut answer)?;
    if answer.trim() != "CONFIRM" {
        r.state = "cancelled".into();
        save(&mut file, &r)?;
        return Ok(r);
    }
    // Refresh all read-only gates after the human wait. Never reuse old balances.
    let refreshed = prepare(&c, r.intent.clone()).await?;
    if refreshed.state != "ready" {
        save(&mut file, &refreshed)?;
        return Ok(refreshed);
    }
    let (_, buy) = c.rules(&r.intent)?;
    let old = units(
        r.quote["toCoinAmount"].as_str().context("missing quote")?,
        buy.decimals,
    )?;
    let new = units(
        refreshed.quote["toCoinAmount"]
            .as_str()
            .context("missing fresh quote")?,
        buy.decimals,
    )?;
    if new < old {
        r.state = "quote_changed".into();
        r.error = Some("output decreased while confirming; review a new preparation".into());
        save(&mut file, &r)?;
        return Ok(r);
    }
    r = refreshed;
    // Persistent per-wallet barrier survives crash/timeout even with a new report path.
    reserve_submission(&c, path)?;
    r.state = "submission_outcome_unknown".into();
    save(&mut file, &r)?;
    match call(&c, json!({"op":"swap","intent":r.intent})).await {
        Ok(v) => {
            if let Some(id) = v["orderId"].as_str().filter(|s| valid_id(s).is_ok()) {
                r.order_id = Some(id.into());
                r.state = "submitted".into();
            } else {
                r.error = Some("submit returned no valid order ID; inspect wallet history".into());
            }
        }
        Err(e) => r.error = Some(e.to_string()),
    }
    save(&mut file, &r)?;
    if r.order_id.is_some() {
        if let Err(e) = track_inner(&c, &mut r).await {
            r.error = Some(e.to_string());
            if r.state != "pending" {
                r.state = "needs_attention".into();
            }
        }
        save(&mut file, &r)?;
    }
    r.stages = c.trace.lock().unwrap().clone();
    save(&mut file, &r)?;
    release_terminal(&c, path, &r)?;
    Ok(r)
}
fn validate_order(v: &Value, c: &Config, r: &Report) -> Result<Value> {
    let list = v["list"].as_array().context("missing order list")?;
    ensure!(list.len() == 1, "order lookup is not unique");
    let o = &list[0];
    let (sell, _) = c.rules(&r.intent)?;
    ensure!(
        o["orderId"].as_str() == r.order_id.as_deref()
            && o["chain"] == "56"
            && o["fromToken"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case(&r.intent.from_token))
            && o["toToken"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case(&r.intent.to_token)),
        "order identity mismatch"
    );
    ensure!(
        units(
            o["fromTokenQty"].as_str().context("missing order amount")?,
            sell.decimals
        )? == units(&r.intent.amount, sell.decimals)?,
        "order amount mismatch"
    );
    let slip = o["slippage"]
        .as_str()
        .context("order slippage missing")?
        .parse::<f64>()?;
    ensure!(
        slip.is_finite() && slip >= 0.0 && slip <= r.intent.slippage_bps as f64 / 100.0 + 1e-10,
        "order slippage exceeds request"
    );
    Ok(o.clone())
}
async fn track_inner(c: &Config, r: &mut Report) -> Result<()> {
    // Never present a previous verification as evidence for a failed refresh.
    r.settlement = Value::Null;
    let id = r
        .order_id
        .as_deref()
        .context("no order ID; inspect wallet history; never resubmit automatically")?;
    valid_id(id)?;
    let a = Adapter {
        config: c.clone(),
        last: Default::default(),
    };
    let outcome = flow(ORDER, json!({"op":"order","order_id":id}), a.clone()).await;
    let last = a
        .last
        .lock()
        .unwrap()
        .clone()
        .context("no order response")?;
    ensure!(last["success"] == true, "order API failed");
    r.order = validate_order(&last["data"], c, r)?;
    match r.order["status"].as_str() {
        Some("FAILED") => {
            r.state = "order_failed".into();
            return Ok(());
        }
        Some("FINISHED") => {}
        _ => {
            r.state = "pending".into();
            outcome?;
            bail!("order still pending")
        }
    }
    let hash = r.order["txHash"]
        .as_str()
        .context("finished order missing hash")?;
    ensure!(
        hash.len() == 66
            && hash.starts_with("0x")
            && hash[2..].bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid transaction hash"
    );
    r.state = "verifying_settlement".into();
    let options = serde_json::from_value(
        json!({"rpc_url":c.rpc_url,"tx_hash":hash,"chain_id":56,"confirmations":3}),
    )?;
    let watched = crate::receipt::watch_transaction(options).await?;
    ensure!(watched.success, "receipt not confirmed");
    let receipt = rpc(c, "eth_getTransactionReceipt", json!([hash])).await?;
    ensure!(
        receipt["transactionHash"]
            .as_str()
            .is_some_and(|h| h.eq_ignore_ascii_case(hash))
            && receipt["status"] == "0x1"
            && receipt["blockHash"].as_str() == watched.observation.block_hash.as_deref(),
        "receipt changed after confirmation"
    );
    r.settlement = json!({"receipt_tracking":watched});
    let mut reconciled = settlement(&receipt, c, &r.intent, &r.order)?;
    reconciled["receipt_tracking"] = r.settlement["receipt_tracking"].take();
    r.settlement = reconciled;
    r.state = if r.settlement["review_required"] == true {
        "settled_with_discrepancy"
    } else {
        "completed"
    }
    .into();
    r.error = None;
    Ok(())
}
pub fn settlement(receipt: &Value, c: &Config, i: &Intent, order: &Value) -> Result<Value> {
    let (sell, buy) = c.rules(i)?;
    let account = c.wallet_address.to_lowercase();
    let mut sold = BigUint::from(0u8);
    let mut sold_in = BigUint::from(0u8);
    let mut received = BigUint::from(0u8);
    let mut bought_out = BigUint::from(0u8);
    for log in receipt["logs"].as_array().context("receipt logs missing")? {
        if log["topics"][0] != TRANSFER {
            continue;
        }
        let token = log["address"].as_str().context("log address missing")?;
        if !token.eq_ignore_ascii_case(&sell.address) && !token.eq_ignore_ascii_case(&buy.address) {
            continue;
        }
        let topics = log["topics"].as_array().context("topics missing")?;
        ensure!(
            topics.len() == 3 && log["removed"] != true,
            "invalid transfer log"
        );
        let party = |n: usize| -> Result<String> {
            let s = topics[n].as_str().context("topic missing")?;
            ensure!(
                s.len() == 66
                    && s.starts_with("0x")
                    && s[2..].bytes().all(|b| b.is_ascii_hexdigit())
                    && s[2..26] == *"000000000000000000000000",
                "invalid address topic"
            );
            Ok(format!("0x{}", &s[26..]).to_lowercase())
        };
        let from = party(1)?;
        let to = party(2)?;
        let amount = hex(&log["data"])?;
        if token.eq_ignore_ascii_case(&sell.address) {
            if from == account {
                sold += &amount
            }
            if to == account {
                sold_in += &amount
            }
        } else {
            if to == account {
                received += &amount
            }
            if from == account {
                bought_out += &amount
            }
        }
    }
    ensure!(
        sold >= sold_in && received >= bought_out,
        "unexpected net transfer direction"
    );
    sold -= sold_in;
    received -= bought_out;
    let requested = units(&i.amount, sell.decimals)?;
    ensure!(
        sold <= requested,
        "on-chain sell amount exceeds requested amount"
    );
    ensure!(
        sold > BigUint::from(0u8) && received > BigUint::from(0u8),
        "on-chain wallet has no positive trade transfers"
    );
    let reported = order["toTokenActualQty"]
        .as_str()
        .context("missing reported amount")?;
    let reported_units = units(reported, buy.decimals)?;
    let mut warnings = vec![];
    if sold != requested {
        warnings.push("actual_sell_below_requested");
    }
    if reported_units != received {
        warnings.push("reported_output_differs_from_chain");
    }
    Ok(json!({
        "source":"receipt_transfer_logs",
        "requested_sold":decimal(&requested,sell.decimals),
        "sold":decimal(&sold,sell.decimals),
        // A difference in this transaction, not a current balance or inferred fee.
        "unspent_requested_amount":decimal(&(&requested - &sold),sell.decimals),
        "sold_amount_matches_request":sold==requested,
        "received":decimal(&received,buy.decimals),
        "reported_received":reported,
        "reported_amount_matches_chain":reported_units==received,
        "review_required":!warnings.is_empty(),
        "warnings":warnings,
        "wallet_address":c.wallet_address,
        "tx_hash":receipt["transactionHash"]
    }))
}
pub async fn track(c: Config, path: &Path) -> Result<Report> {
    let mut file = fs::OpenOptions::new().read(true).write(true).open(path)?;
    file.try_lock().context("report is in use")?;
    let mut r: Report = serde_json::from_reader(&file)?;
    ensure!(
        r.order_id.is_some(),
        "no order ID; inspect wallet history before retrying"
    );
    ensure!(
        r.backend == "agentic_wallet"
            && r.wallet_address.eq_ignore_ascii_case(&c.wallet_address)
            && r.config_sha256 == digest(&c)?,
        "report config mismatch"
    );
    if let Err(e) = track_inner(&c, &mut r).await {
        r.error = Some(e.to_string());
        if r.state != "pending" {
            r.state = "needs_attention".into();
        }
    }
    r.stages.extend(c.trace.lock().unwrap().clone());
    save(&mut file, &r)?;
    release_terminal(&c, path, &r)?;
    Ok(r)
}

fn reserve_submission(c: &Config, path: &Path) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let barrier = c
        .state_dir
        .join(format!("agentic-{}.lock", c.wallet_address.to_lowercase()));
    let mut lock = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&barrier)
        .context("wallet submission lock exists; inspect previous order, do not retry")?;
    writeln!(lock, "{}", path.canonicalize()?.display())?;
    lock.sync_all()?;
    fs::File::open(&c.state_dir)?.sync_all()?;
    Ok(())
}
fn release_terminal(c: &Config, path: &Path, r: &Report) -> Result<()> {
    if !matches!(r.state.as_str(), "completed" | "order_failed") {
        return Ok(());
    }
    let barrier = c
        .state_dir
        .join(format!("agentic-{}.lock", c.wallet_address.to_lowercase()));
    if barrier.exists()
        && fs::read_to_string(&barrier)?.trim() == path.canonicalize()?.to_string_lossy()
    {
        fs::remove_file(barrier)?;
        fs::File::open(&c.state_dir)?.sync_all()?;
    }
    Ok(())
}
/// Inspect an existing order without requiring funds for a new trade.
pub async fn inspect(c: Config, i: Intent, id: String, path: &Path) -> Result<Report> {
    use std::os::unix::fs::OpenOptionsExt;
    c.rules(&i)?;
    valid_id(&id)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.try_lock().context("report is in use")?;
    let mut r = order_report(&c, i, id)?;
    save(&mut file, &r)?;
    if let Err(e) = track_inner(&c, &mut r).await {
        r.error = Some(e.to_string());
        if r.state != "pending" {
            r.state = "needs_attention".into();
        }
    }
    r.stages = c.trace.lock().unwrap().clone();
    save(&mut file, &r)?;
    Ok(r)
}

fn order_report(c: &Config, i: Intent, id: String) -> Result<Report> {
    c.rules(&i)?;
    valid_id(&id)?;
    let r = Report {
        schema_version: 1,
        backend: "agentic_wallet".into(),
        state: "tracking_existing_order".into(),
        intent: i,
        wallet_address: c.wallet_address.clone(),
        config_sha256: digest(c)?,
        prepared_at: chrono::Utc::now().to_rfc3339(),
        stages: vec![],
        balances: Value::Null,
        quote: Value::Null,
        wallet_settings: Value::Null,
        token_audit: Value::Null,
        order_id: Some(id),
        order: Value::Null,
        settlement: Value::Null,
        error: None,
    };
    Ok(r)
}
pub async fn inspect_order(c: Config, i: Intent, id: String) -> Result<Report> {
    let mut r = order_report(&c, i, id)?;
    if let Err(e) = track_inner(&c, &mut r).await {
        r.error = Some(e.to_string());
        if r.state != "pending" {
            r.state = "needs_attention".into();
        }
    }
    r.stages = c.trace.lock().unwrap().clone();
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        Config {
            executable: PathBuf::from("/unused"),
            wallet_address: format!("0x{}", "1".repeat(40)),
            rpc_url: "http://127.0.0.1:8545".into(),
            state_dir: PathBuf::from("/unused"),
            max_slippage_bps: 50,
            trace: Default::default(),
            tokens: vec![
                TokenRule {
                    address: format!("0x{}", "2".repeat(40)),
                    symbol: "SELL".into(),
                    decimals: 18,
                    max_sell_amount: "6".into(),
                },
                TokenRule {
                    address: format!("0x{}", "3".repeat(40)),
                    symbol: "BUY".into(),
                    decimals: 18,
                    max_sell_amount: "1".into(),
                },
            ],
        }
    }
    fn intent() -> Intent {
        let c = config();
        Intent {
            from_token: c.tokens[0].address.clone(),
            to_token: c.tokens[1].address.clone(),
            amount: "6".into(),
            slippage_bps: 50,
        }
    }
    #[test]
    fn exact_decimal_limits_and_malformed_inputs() {
        assert_eq!(
            units("0.017694493701703884", 18).unwrap().to_string(),
            "17694493701703884"
        );
        for bad in ["-1", "1e18", "NaN", " 6", "6.", ".5", "1.2.3", ""] {
            assert!(units(bad, 18).is_err(), "{bad}");
        }
        assert!(units("0.0000000000000000001", 18).is_err());
        let c = config();
        let mut i = intent();
        assert!(c.rules(&i).is_ok());
        i.amount = "6.000000000000000001".into();
        assert!(c.rules(&i).is_err());
        i = intent();
        i.slippage_bps = 51;
        assert!(c.rules(&i).is_err());
        i = intent();
        i.to_token = format!("0x{}", "4".repeat(40));
        assert!(c.rules(&i).is_err());
    }
    #[test]
    fn quote_identity_amount_and_slippage_fail_closed() {
        let c = config();
        let i = intent();
        let q = json!({"fromCoinSymbol":"SELL","toCoinSymbol":"BUY","fromCoinAmount":"6","toCoinAmount":"0.01","slippage":0.005});
        assert!(validate_quote(&q, &i, &c.tokens[0], &c.tokens[1]).is_ok());
        for (key, v) in [
            ("fromCoinAmount", json!("7")),
            ("toCoinSymbol", json!("OTHER")),
            ("slippage", json!(0.05)),
            ("toCoinAmount", json!("0")),
        ] {
            let mut bad = q.clone();
            bad[key] = v;
            assert!(validate_quote(&bad, &i, &c.tokens[0], &c.tokens[1]).is_err());
        }
    }
    #[test]
    fn unavailable_or_hit_audit_cannot_be_reported_safe() {
        let unavailable = json!({"hasResult":false,"isSupported":false,"riskLevel":0});
        assert!(check_audit(&unavailable).is_err());
        let mut a = json!({"hasResult":true,"isSupported":true,"riskLevel":1,"riskItems":[{"details":[{"isHit":false}]}],"extraInfo":{"buyTax":"0","sellTax":"0"}});
        assert!(check_audit(&a).is_ok());
        a["riskItems"][0]["details"][0]["isHit"] = json!(true);
        assert!(check_audit(&a).is_err());
    }
    fn transfer(token: &str, from: &str, to: &str, amount: &str) -> Value {
        json!({"address":token,"topics":[TRANSFER,format!("0x{:0>64}",&from[2..]),format!("0x{:0>64}",&to[2..])],"data":format!("0x{:0>64}",units(amount,18).unwrap().to_str_radix(16)),"removed":false})
    }
    #[test]
    fn settlement_uses_wallet_transfer_logs_not_reported_gross_amount() {
        let c = config();
        let i = intent();
        let router = format!("0x{}", "4".repeat(40));
        let receipt = json!({"transactionHash":format!("0x{}","a".repeat(64)),"logs":[
            transfer(&i.from_token,&c.wallet_address,&router,"6"),
            transfer(&i.to_token,&router,&c.wallet_address,"0.017694493701703884"),
            transfer(&i.to_token,&router,&format!("0x{}","5".repeat(40)),"1")
        ]});
        let order = json!({"toTokenActualQty":"0.017754231617236713"});
        let s = settlement(&receipt, &c, &i, &order).unwrap();
        assert_eq!(s["received"], "0.017694493701703884");
        assert_eq!(s["reported_amount_matches_chain"], false);
        assert_eq!(s["review_required"], true);
        assert_eq!(s["unspent_requested_amount"], "0");
        let mut bad = receipt.clone();
        bad["logs"][0] = transfer(&i.from_token, &c.wallet_address, &router, "7");
        assert!(settlement(&bad, &c, &i, &order).is_err());
        let mut bad = receipt.clone();
        bad["logs"][1]["removed"] = json!(true);
        assert!(settlement(&bad, &c, &i, &order).is_err());
    }
    #[test]
    fn partial_sell_preserves_exact_difference_without_assuming_a_fee() {
        let c = config();
        let mut i = intent();
        i.amount = "0.017694493701703884".into();
        let router = format!("0x{}", "4".repeat(40));
        let receipt = json!({"logs":[
            transfer(&i.from_token,&c.wallet_address,&router,"0.017634956787184737"),
            transfer(&i.to_token,&router,&c.wallet_address,"5.948867152986066609")
        ]});
        let order = json!({"toTokenActualQty":"5.948867152986066609"});
        let s = settlement(&receipt, &c, &i, &order).unwrap();
        assert_eq!(s["requested_sold"], i.amount);
        assert_eq!(s["sold"], "0.017634956787184737");
        assert_eq!(s["unspent_requested_amount"], "0.000059536914519147");
        assert_eq!(s["received"], "5.948867152986066609");
        assert_eq!(s["sold_amount_matches_request"], false);
        assert_eq!(s["reported_amount_matches_chain"], true);
        assert_eq!(s["review_required"], true);
        assert_eq!(s["warnings"], json!(["actual_sell_below_requested"]));
    }
    #[test]
    fn discrepancy_handling_still_rejects_excess_zero_or_wrong_wallet_transfers() {
        let c = config();
        let i = intent();
        let router = format!("0x{}", "4".repeat(40));
        let order = json!({"toTokenActualQty":"5"});
        for (sold, received, wallet) in [
            ("6.000000000000000001", "5", c.wallet_address.as_str()),
            ("0", "5", c.wallet_address.as_str()),
            ("6", "0", c.wallet_address.as_str()),
            ("6", "5", router.as_str()),
        ] {
            let receipt = json!({"logs":[
                transfer(&i.from_token,wallet,&router,sold),
                transfer(&i.to_token,&router,wallet,received)
            ]});
            assert!(settlement(&receipt, &c, &i, &order).is_err());
        }
        let receipt = json!({"logs":[
            transfer(&i.from_token,&c.wallet_address,&router,"6"),
            transfer(&i.to_token,&router,&c.wallet_address,"5")
        ]});
        let s = settlement(&receipt, &c, &i, &order).unwrap();
        assert_eq!(s["review_required"], false);
        assert_eq!(s["warnings"], json!([]));
    }
    #[test]
    fn order_binding_rejects_other_orders_and_amounts() {
        let c = config();
        let i = intent();
        let r = order_report(&c, i.clone(), "123".into()).unwrap();
        let o = json!({"list":[{"orderId":"123","chain":"56","fromToken":i.from_token,"toToken":i.to_token,"fromTokenQty":"6","slippage":"0.5"}]});
        assert!(validate_order(&o, &c, &r).is_ok());
        for (k, v) in [
            ("orderId", json!("124")),
            ("chain", json!("1")),
            ("fromTokenQty", json!("5")),
        ] {
            let mut bad = o.clone();
            bad["list"][0][k] = v;
            assert!(validate_order(&bad, &c, &r).is_err());
        }
    }
    #[test]
    fn durable_wallet_barrier_blocks_different_report_paths_until_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = config();
        c.state_dir = tmp.path().to_path_buf();
        let first = tmp.path().join("first.json");
        let other = tmp.path().join("other.json");
        fs::write(&first, "{}").unwrap();
        fs::write(&other, "{}").unwrap();
        reserve_submission(&c, &first).unwrap();
        assert!(reserve_submission(&c, &other).is_err());
        let mut r = order_report(&c, intent(), "123".into()).unwrap();
        r.state = "submission_outcome_unknown".into();
        release_terminal(&c, &first, &r).unwrap();
        assert!(reserve_submission(&c, &other).is_err());
        r.state = "settled_with_discrepancy".into();
        assert!(r.has_settlement());
        release_terminal(&c, &first, &r).unwrap();
        assert!(reserve_submission(&c, &other).is_err());
        r.state = "completed".into();
        release_terminal(&c, &other, &r).unwrap();
        assert!(reserve_submission(&c, &other).is_err());
        release_terminal(&c, &first, &r).unwrap();
        reserve_submission(&c, &other).unwrap();
    }
    #[derive(Clone)]
    struct Script(Arc<Mutex<std::collections::VecDeque<Value>>>);
    impl HttpTransport for Script {
        async fn execute(&self, r: Request, _: RequestOptions) -> Result<HttpResponse, HttpError> {
            assert_eq!(r.url, URL);
            let v = self
                .0
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected repeat");
            Ok(HttpResponse::new(200, vec![], v.to_string()))
        }
    }
    #[tokio::test]
    async fn real_flow_controls_business_checks_and_order_polling() {
        let script = Script(Arc::new(Mutex::new(
            [
                json!({"success":true,"data":{"list":[{"status":"PENDING"}]}}),
                json!({"success":true,"data":{"list":[{"status":"FINISHED"}]}}),
            ]
            .into(),
        )));
        let fast = ORDER.replace("interval_ms: 2000", "interval_ms: 1");
        assert!(flow(
            &fast,
            json!({"op":"order","order_id":"123"}),
            script.clone()
        )
        .await
        .is_ok());
        assert!(script.0.lock().unwrap().is_empty());
        for response in [
            json!({"success":false,"data":{}}),
            json!({"success":true,"data":{"list":[{"status":"FAILED"}]}}),
        ] {
            let script = Script(Arc::new(Mutex::new([response].into())));
            assert!(flow(&fast, json!({"op":"order","order_id":"123"}), script)
                .await
                .is_err());
        }
        let script = Script(Arc::new(Mutex::new(
            [json!({"success":false,"data":{"orderId":"123"}})].into(),
        )));
        assert!(flow(STAGE, json!({"op":"swap","intent":intent()}), script)
            .await
            .is_err());
    }
    #[tokio::test]
    async fn process_adapter_executes_once_without_shell_or_api_secrets() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("mock-baw");
        let marker = tmp.path().join("calls");
        fs::write(&path,format!("#!/bin/sh\nprintf 'call\\n' >> '{}'\nprintf '%s' '{{\"success\":true,\"data\":{{\"orderId\":\"123\"}}}}'\n",marker.display())).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let mut c = config();
        c.executable = path;
        let result = call(&c, json!({"op":"swap","intent":intent()}))
            .await
            .unwrap();
        assert_eq!(result["orderId"], "123");
        assert_eq!(fs::read_to_string(marker).unwrap(), "call\n");
        assert_eq!(c.trace.lock().unwrap().len(), 1);
        assert!(args(&json!({"op":"shell","command":"arbitrary"})).is_err());
    }
}
