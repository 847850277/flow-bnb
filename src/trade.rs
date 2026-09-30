//! Evidence-bound trade preparation. All Binance HTTP stages run through Flow;
//! chain-specific validation and signer handoff stay in this side project.
use crate::{ExecutionMode, SimulationStatus, TradeIntent, TradePolicy};
use anyhow::{anyhow, bail, ensure, Context, Result};
use futures::StreamExt;
use num_bigint::BigUint;
use postman_flow::{
    compile_flow, execute_flow, parse_flow_yaml, CompileEnvironment, FlowEvent, FlowInputs,
    FlowSessionEnvironment,
};
use postman_http::{
    request::{RedirectPolicy, Request, RequestOptions},
    HttpError, HttpResponse, HttpTransport,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const BASE: &str = "https://web3.binance.com/build/api/v1/dex/";
const GET: &str = include_str!("../flows/trade_api_get.http.yml");
const POST: &str = include_str!("../flows/trade_api_post.http.yml");

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TradeRequest {
    pub wallet_address: String,
    pub from_token_address: String,
    pub to_token_address: String,
    /// Exact sell-token base units; never a floating point token amount.
    pub amount: String,
    pub slippage_bps: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPolicy {
    pub risk: TradePolicy,
    /// Explicit router/spender trust. Empty lists allow read-only diagnosis only.
    #[serde(default)]
    pub allowed_routers: Vec<String>,
    #[serde(default)]
    pub allowed_spenders: Vec<String>,
    #[serde(default = "default_age")]
    pub max_age_seconds: u64,
    /// Exact ERC-20 approvals do not execute a quote. A swap needs fresh preparation.
    #[serde(default = "default_approval_age")]
    pub approval_max_age_seconds: u64,
}
fn default_age() -> u64 {
    30
}
fn default_approval_age() -> u64 {
    300
}
impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            risk: TradePolicy::default(),
            allowed_routers: vec![],
            allowed_spenders: vec![],
            max_age_seconds: default_age(),
            approval_max_age_seconds: default_approval_age(),
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StageEvidence {
    pub stage: String,
    pub http_status: Option<u16>,
    pub business_code: Option<i64>,
    pub elapsed_ms: u64,
    pub response_sha256: Option<String>,
    pub success: bool,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct QuoteEvidence {
    pub quote_id: String,
    pub vendor: String,
    pub execution_mode: String,
    pub sell_amount: String,
    pub buy_amount: String,
    pub sell_token_decimals: u32,
    pub sell_token_usd_price: String,
    pub notional_usd: String,
    pub price_impact_bps: u32,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TransactionSummary {
    pub kind: String,
    pub chain_id: u64,
    pub from: String,
    pub to: String,
    pub value: String,
    pub calldata_sha256: String,
    pub spender: Option<String>,
    pub approval_amount: Option<String>,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TradeReport {
    pub schema_version: u32,
    pub state: String,
    pub request: TradeRequest,
    pub quote: Option<QuoteEvidence>,
    pub sell_balance: Option<String>,
    pub buy_balance: Option<String>,
    pub balance_sources: BTreeMap<String, String>,
    pub transaction: Option<TransactionSummary>,
    pub simulation_status: Option<String>,
    pub rfq_payload_sha256: Option<String>,
    pub policy_sha256: String,
    pub confirmation_id: Option<String>,
    pub max_age_seconds: u64,
    pub created_at: String,
    pub stages: Vec<StageEvidence>,
    pub blockers: Vec<String>,
}
/// Kept private and never deserialized from an agent-supplied report. A report is
/// evidence, not authority to execute. Handoff consumes the prepared action once.
pub struct PreparedTrade {
    report: TradeReport,
    transaction: Option<Value>,
    started: Instant,
}
impl PreparedTrade {
    pub fn report(&self) -> &TradeReport {
        &self.report
    }
    pub fn into_report(self) -> TradeReport {
        self.report
    }
    pub fn ready(&self) -> bool {
        self.transaction.is_some() && self.report.blockers.is_empty()
    }
    /// Binding includes wallet, exact transaction, quote, simulation and policy.
    /// The caller must obtain this exact identifier through an operator interface.
    pub fn authorize(self, confirmation_id: &str) -> Result<SignerRequest> {
        ensure!(self.ready(), "trade has not passed preparation gates");
        ensure!(
            self.report.confirmation_id.as_deref() == Some(confirmation_id),
            "confirmation does not match the prepared action"
        );
        ensure!(
            self.started.elapsed() < Duration::from_secs(self.report.max_age_seconds),
            "prepared action expired; prepare again"
        );
        Ok(SignerRequest {
            protocol: "flow-bnb-signer-v1".into(),
            confirmation_id: confirmation_id.into(),
            chain_id: 56,
            kind: self.report.transaction.as_ref().unwrap().kind.clone(),
            transaction: self.transaction.unwrap(),
            expires_in_ms: Duration::from_secs(self.report.max_age_seconds)
                .saturating_sub(self.started.elapsed())
                .as_millis() as u64,
        })
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignerRequest {
    pub protocol: String,
    pub confirmation_id: String,
    pub chain_id: u64,
    pub kind: String,
    pub transaction: Value,
    pub expires_in_ms: u64,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignerResponse {
    pub confirmation_id: String,
    pub tx_hash: String,
}

/// A local, explicitly selected wallet adapter owns keys and submits the exact
/// transaction after its own wallet confirmation. No shell and no ambient API keys.
/// Never retries: a timeout can mean the wallet already broadcast the transaction.
pub async fn invoke_signer(executable: &Path, request: &SignerRequest) -> Result<SignerResponse> {
    invoke_signer_with_args(executable, &[], request).await
}

pub async fn invoke_signer_with_args(
    executable: &Path,
    args: &[String],
    request: &SignerRequest,
) -> Result<SignerResponse> {
    use std::process::Stdio;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    ensure!(
        executable.is_absolute() && executable.is_file(),
        "signer must be an absolute executable file path"
    );
    let mut child = tokio::process::Command::new(executable)
        .args(args)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("cannot start signer")?;
    let result = tokio::time::timeout(Duration::from_millis(request.expires_in_ms), async {
        let mut stdin = child.stdin.take().context("signer stdin unavailable")?;
        stdin.write_all(&serde_json::to_vec(request)?).await?;
        stdin.write_all(b"\n").await?;
        drop(stdin);
        let stdout = child.stdout.take().context("signer stdout unavailable")?;
        let mut bytes = Vec::new();
        stdout.take(8193).read_to_end(&mut bytes).await?;
        ensure!(bytes.len() <= 8192, "signer response exceeds limit");
        ensure!(
            child.wait().await?.success(),
            "signer failed; inspect wallet before retrying"
        );
        let response: SignerResponse = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow!("invalid signer response; inspect wallet before retrying"))?;
        ensure!(
            response.confirmation_id == request.confirmation_id && hex_bytes(&response.tx_hash, 32),
            "signer response binding or transaction hash mismatch; inspect wallet"
        );
        Ok(response)
    })
    .await;
    match result {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            bail!("signer timed out; broadcast outcome unknown, inspect wallet before retrying")
        }
    }
}

#[derive(Clone)]
struct Audited<T> {
    inner: T,
    evidence: Arc<Mutex<StageEvidence>>,
}
impl<T: HttpTransport> HttpTransport for Audited<T> {
    async fn execute(
        &self,
        request: Request,
        options: RequestOptions,
    ) -> Result<HttpResponse, HttpError> {
        let started = Instant::now();
        let response = self.inner.execute(request, options).await;
        let mut evidence = self.evidence.lock().unwrap();
        evidence.elapsed_ms = started.elapsed().as_millis() as u64;
        match response {
            Ok(response) => {
                evidence.http_status = Some(response.status);
                evidence.response_sha256 = Some(hash(response.body.as_bytes()));
                evidence.business_code = serde_json::from_str::<Value>(&response.body)
                    .ok()
                    .and_then(|v| v["code"].as_i64());
                Ok(response)
            }
            Err(_) => Err(HttpError::invalid_response("trade API transport failed")),
        }
    }
}

struct Runner<T> {
    api: T,
}
impl<T: HttpTransport + Clone + 'static> Runner<T> {
    async fn call(
        &self,
        report: &mut TradeReport,
        stage: &str,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Value> {
        let mut url = url::Url::parse(&format!("{BASE}{path}"))?;
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
        }
        let document = parse_flow_yaml(if body.is_some() { POST } else { GET })?;
        let plan = compile_flow(
            &document.flow,
            &document.apis,
            &CompileEnvironment::default(),
        )
        .map_err(|_| anyhow!("internal trade stage compilation failed"))?;
        let mut inputs = FlowInputs::new().with("url", json!(url.as_str()));
        if let Some(body) = body {
            inputs.insert("body", body);
        }
        let evidence = Arc::new(Mutex::new(StageEvidence {
            stage: stage.into(),
            http_status: None,
            business_code: None,
            elapsed_ms: 0,
            response_sha256: None,
            success: false,
        }));
        let transport = Audited {
            inner: self.api.clone(),
            evidence: evidence.clone(),
        };
        let session = FlowSessionEnvironment::new(inputs).with_request_options(RequestOptions {
            timeout_ms: Some(15_000),
            redirect_policy: RedirectPolicy::DoNotFollow,
            ..Default::default()
        });
        let run = async {
            let events = execute_flow(plan, transport, session)?;
            let mut events = std::pin::pin!(events);
            while let Some(event) = events.next().await {
                if let FlowEvent::FlowFinished { success, outputs } =
                    event.map_err(|_| anyhow!("trade stage failed"))?
                {
                    if success {
                        return outputs
                            .get("data")
                            .map(|v| v.value().clone())
                            .context("trade response data missing");
                    }
                }
            }
            bail!("trade API or response check failed")
        };
        let result = tokio::time::timeout(Duration::from_secs(16), run)
            .await
            .unwrap_or_else(|_| Err(anyhow!("trade stage timed out")));
        let mut evidence = evidence.lock().unwrap().clone();
        evidence.success = result.is_ok();
        report.stages.push(evidence);
        result.with_context(|| format!("{stage} failed (see HTTP/business code in stages)"))
    }
}

pub async fn prepare_with_transport<T: HttpTransport + Clone + 'static>(
    request: TradeRequest,
    policy: ExecutionPolicy,
    api: T,
) -> PreparedTrade {
    let started = Instant::now();
    let policy_hash = hash(&serde_json::to_vec(&policy).unwrap_or_default());
    let mut prepared = PreparedTrade {
        report: TradeReport {
            schema_version: 1,
            state: "blocked".into(),
            request,
            quote: None,
            sell_balance: None,
            buy_balance: None,
            balance_sources: BTreeMap::new(),
            transaction: None,
            simulation_status: None,
            rfq_payload_sha256: None,
            policy_sha256: policy_hash,
            confirmation_id: None,
            max_age_seconds: policy.max_age_seconds,
            created_at: chrono::Utc::now().to_rfc3339(),
            stages: vec![],
            blockers: vec![],
        },
        transaction: None,
        started,
    };
    match prepare(&Runner { api }, &mut prepared.report, &policy).await {
        Ok(transaction) => {
            if let Some(transaction) = transaction {
                if started.elapsed() >= Duration::from_secs(policy.max_age_seconds) {
                    prepared
                        .report
                        .blockers
                        .push("preparation expired; obtain a fresh quote".into());
                    prepared.report.state = "blocked".into();
                } else {
                    // All preparation gates still run within the quote TTL. Only
                    // the separately simulated exact approval gets longer review.
                    if prepared
                        .report
                        .transaction
                        .as_ref()
                        .is_some_and(|tx| tx.kind == "approval")
                    {
                        prepared.report.max_age_seconds = policy.approval_max_age_seconds;
                    }
                    if started.elapsed() >= Duration::from_secs(prepared.report.max_age_seconds) {
                        prepared.report.state = "blocked".into();
                        prepared
                            .report
                            .blockers
                            .push("prepared action expired; prepare again".into());
                        return prepared;
                    }
                    let binding = json!({"report": prepared.report, "transaction": transaction});
                    prepared.report.confirmation_id =
                        Some(hash(&serde_json::to_vec(&binding).unwrap()));
                    prepared.transaction = Some(transaction);
                }
            }
        }
        Err(error) => prepared.report.blockers.push(format!("{error:#}")),
    }
    prepared
}

async fn prepare<T: HttpTransport + Clone + 'static>(
    runner: &Runner<T>,
    report: &mut TradeReport,
    policy: &ExecutionPolicy,
) -> Result<Option<Value>> {
    let req = report.request.clone();
    for address in [
        &req.wallet_address,
        &req.from_token_address,
        &req.to_token_address,
    ] {
        ensure!(valid_address(address), "invalid or zero EVM address");
    }
    ensure!(
        !req.from_token_address
            .eq_ignore_ascii_case(&req.to_token_address),
        "sell and buy token must differ"
    );
    let amount = uint(&req.amount)?;
    ensure!(amount > BigUint::from(0u8), "amount must be positive");
    ensure!(
        policy.risk.chain_id == "56",
        "only BSC mainnet is supported"
    );
    ensure!(
        policy.risk.max_notional_usd.is_finite() && policy.risk.max_notional_usd > 0.0,
        "invalid notional limit"
    );
    ensure!(
        policy.risk.require_operator_confirmation && policy.risk.require_successful_simulation,
        "execution cannot disable simulation or confirmation"
    );
    ensure!(
        (1..=30).contains(&policy.max_age_seconds),
        "max_age_seconds must be 1..=30 (quote TTL)"
    );
    ensure!(
        (1..=300).contains(&policy.approval_max_age_seconds),
        "approval_max_age_seconds must be 1..=300"
    );
    ensure!(
        req.slippage_bps <= policy.risk.max_slippage_bps && req.slippage_bps <= 10_000,
        "slippage limit exceeded"
    );
    for address in policy
        .allowed_routers
        .iter()
        .chain(&policy.allowed_spenders)
        .chain(&policy.risk.allowed_token_addresses)
    {
        ensure!(valid_address(address), "invalid policy allowlist address");
    }
    if !policy.risk.allowed_token_addresses.is_empty() {
        ensure!(
            allowed(
                &policy.risk.allowed_token_addresses,
                &req.from_token_address
            ) && allowed(&policy.risk.allowed_token_addresses, &req.to_token_address),
            "token not in policy allowlist"
        );
    }
    let balance = runner
        .call(
            report,
            "wallet",
            "balance/token-balances-by-address",
            &[],
            Some(
                json!({"address":req.wallet_address, "tokenContractAddresses":[
        {"binanceChainId":"56","tokenContractAddress":req.from_token_address},
        {"binanceChainId":"56","tokenContractAddress":req.to_token_address}]}),
            ),
        )
        .await?;
    let (sell, buy) = optional_balances(&balance, &req)?;
    for group in balance.as_array().into_iter().flatten() {
        for asset in group["tokenAssets"].as_array().into_iter().flatten() {
            let token = asset["tokenContractAddress"].as_str().unwrap_or("");
            let side = if token.eq_ignore_ascii_case(&req.from_token_address) {
                "sell"
            } else {
                "buy"
            };
            report.balance_sources.insert(
                side.into(),
                asset["flowBnbBalanceSource"]
                    .as_str()
                    .unwrap_or("wallet_api")
                    .into(),
            );
        }
    }
    report.sell_balance = sell.as_ref().map(|n| n.to_str_radix(10));
    report.buy_balance = buy.as_ref().map(|n| n.to_str_radix(10));
    if sell.is_none() || buy.is_none() {
        report
            .blockers
            .push("wallet API omitted a requested token balance; unknown is not zero".into());
    }
    // Continue read-only quote diagnosis for an unfunded address, but never authorize it.
    let funded = sell.is_some_and(|sell| sell >= amount);
    let query = vec![
        ("binanceChainId", "56".into()),
        ("amount", req.amount.clone()),
        ("fromTokenAddress", req.from_token_address.clone()),
        ("toTokenAddress", req.to_token_address.clone()),
        ("userWalletAddress", req.wallet_address.clone()),
    ];
    let routes = runner
        .call(report, "quote", "aggregator/quote", &query, None)
        .await?;
    let quote = routes
        .as_array()
        .and_then(|r| r.first())
        .context("no quoted route")?;
    let evidence = validate_quote(quote, &req, policy)?;
    let execution_mode = evidence.execution_mode.clone();
    let mut intent = TradeIntent {
        chain_id: "56".into(),
        from_token_address: req.from_token_address.clone(),
        to_token_address: req.to_token_address.clone(),
        notional_usd: evidence.notional_usd.parse()?,
        slippage_bps: req.slippage_bps,
        price_impact_bps: evidence.price_impact_bps,
        mode: ExecutionMode::Prepare,
        simulation_status: None,
        operator_confirmed: false,
    };
    report.quote = Some(evidence.clone());
    let result = policy.risk.evaluate(&intent);
    ensure!(
        result.allowed,
        "policy rejected: {}",
        result
            .violations
            .iter()
            .map(|v| v.code.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut build_query = query;
    build_query.push(("quoteId", evidence.quote_id.clone()));
    build_query.push((
        "slippagePercent",
        format!("{}.{:02}", req.slippage_bps / 100, req.slippage_bps % 100),
    ));
    let build = runner
        .call(report, "build", "aggregator/swap", &build_query, None)
        .await?;
    ensure!(
        string(&build, "executionMode")? == execution_mode,
        "build execution mode does not match quote"
    );
    let router = build
        .get("routerResult")
        .context("build routerResult missing")?;
    let built_quote = validate_quote_fields(router, &req)?;
    let mut built_risk = router.clone();
    built_risk["quoteId"] = json!(evidence.quote_id);
    built_risk["executionMode"] = json!(execution_mode);
    validate_quote(&built_risk, &req, policy)?;
    ensure!(
        built_quote.0 == evidence.vendor && built_quote.1 == evidence.buy_amount,
        "build route differs from quote"
    );
    if !funded {
        report
            .blockers
            .push("insufficient sell-token balance; fund wallet before execution".into());
    }
    // RFQ EIP-712 messages are not EVM calldata. Never feed them into the generic
    // transaction simulator or claim simulation succeeded. Vendor-specific semantic
    // decoding and an order simulation path must be implemented before signing.
    if execution_mode == "RFQ" {
        let rfq = build
            .get("rfq")
            .filter(|v| v.is_object())
            .context("RFQ payload missing")?;
        ensure!(
            string(rfq, "vendor")? == evidence.vendor,
            "RFQ vendor differs from quote"
        );
        ensure!(
            rfq.get("typedDataToSign").is_some_and(|v| !v.is_null()),
            "RFQ typed data missing"
        );
        report.rfq_payload_sha256 = Some(hash(&serde_json::to_vec(rfq)?));
        report.state = "rfq_requires_adapter".into();
        report.blockers.push("RFQ requires vendor-specific EIP-712 validation and order simulation; signing and submission are disabled".into());
        // Still prepare exact approval evidence below when the route needs it.
        if let Some(spender) = quote.get("approveTarget").and_then(Value::as_str) {
            prepare_approval(runner, report, policy, &evidence.vendor, spender).await?;
        }
        return Ok(None);
    }
    ensure!(execution_mode == "SWAP", "unsupported execution mode");
    let raw_tx = build.get("tx").context("swap transaction missing")?;
    let minimum = uint(string(raw_tx, "minReceiveAmount")?)?;
    let required_minimum = uint(&evidence.buy_amount)? * BigUint::from(10_000 - req.slippage_bps)
        / BigUint::from(10_000u32);
    ensure!(
        minimum > BigUint::from(0u8) && minimum >= required_minimum,
        "build minimum receive amount violates slippage policy"
    );
    let tx = normalize_tx(raw_tx, &req.wallet_address)?;
    ensure!(tx["value"] == "0", "ERC-20 swap cannot send native value");
    if !allowed(&policy.allowed_routers, string(&tx, "to")?) {
        report
            .blockers
            .push("swap router is not in local allowlist".into());
    }
    report.transaction = Some(summary("swap", &tx, None, None)?);
    // Require a separately confirmed exact-amount approval first. Re-run to obtain
    // a fresh quote after confirmation. Do not simulate a swap with missing allowance.
    if let Some(spender) = quote.get("approveTarget").and_then(Value::as_str) {
        let approval = prepare_approval(runner, report, policy, &evidence.vendor, spender).await?;
        report.state = if report.blockers.is_empty() {
            "approval_ready"
        } else {
            "blocked"
        }
        .into();
        return Ok(if report.blockers.is_empty() {
            Some(approval)
        } else {
            None
        });
    }
    let simulation = simulate(runner, report, &tx).await?;
    ensure!(simulation == "SUCCESS", "transaction simulation failed");
    intent.mode = ExecutionMode::Execute;
    intent.simulation_status = Some(SimulationStatus::Success);
    // Operator confirmation is still pending; it is enforced by authorize(), not
    // supplied by the model. Evaluate the other gates without bypassing that gate.
    let evaluation = policy.risk.evaluate(&intent);
    ensure!(
        evaluation
            .violations
            .iter()
            .all(|v| v.code == "confirmation_required"),
        "execution policy rejected transaction"
    );
    report.transaction = Some(summary("swap", &tx, None, None)?);
    report.state = if report.blockers.is_empty() {
        "swap_ready"
    } else {
        "blocked"
    }
    .into();
    Ok(if report.blockers.is_empty() {
        Some(tx)
    } else {
        None
    })
}

async fn prepare_approval<T: HttpTransport + Clone + 'static>(
    runner: &Runner<T>,
    report: &mut TradeReport,
    policy: &ExecutionPolicy,
    vendor: &str,
    spender: &str,
) -> Result<Value> {
    ensure!(valid_address(spender), "invalid quote approval target");
    if !allowed(&policy.allowed_spenders, spender) {
        report
            .blockers
            .push("approval spender is not in local allowlist".into());
    }
    let req = report.request.clone();
    let response = runner
        .call(
            report,
            "approval",
            "aggregator/approve-transaction",
            &[
                ("binanceChainId", "56".into()),
                ("tokenContractAddress", req.from_token_address.clone()),
                ("approveAmount", req.amount.clone()),
                ("vendor", vendor.into()),
            ],
            None,
        )
        .await?;
    let approvals = response.as_array().context("invalid approval response")?;
    ensure!(
        approvals.len() == 1,
        "multi-transaction approvals require a separate reset-allowance workflow"
    );
    let approval = &approvals[0];
    ensure!(
        string(approval, "dexContractAddress")?.eq_ignore_ascii_case(spender),
        "approval target differs from quote"
    );
    let expected = format!(
        "0x095ea7b3{:0>64}{:0>64}",
        spender[2..].to_ascii_lowercase(),
        uint(&req.amount)?.to_str_radix(16)
    );
    ensure!(
        string(approval, "data")?.eq_ignore_ascii_case(&expected),
        "approval calldata must approve exactly the requested amount to the quoted spender"
    );
    let tx =
        json!({"from":req.wallet_address,"to":req.from_token_address,"value":"0","data":expected});
    report.transaction = Some(summary(
        "approval",
        &tx,
        Some(spender.into()),
        Some(req.amount),
    )?);
    ensure!(
        simulate(runner, report, &tx).await? == "SUCCESS",
        "approval simulation failed"
    );
    Ok(tx)
}
async fn simulate<T: HttpTransport + Clone + 'static>(
    runner: &Runner<T>,
    report: &mut TradeReport,
    tx: &Value,
) -> Result<String> {
    let data = runner
        .call(
            report,
            "simulate",
            "pre-transaction/simulate",
            &[],
            Some(json!({"binanceChainId":"56","evmTx":tx})),
        )
        .await?;
    let status = string(&data, "status")?.to_owned();
    ensure!(
        matches!(status.as_str(), "SUCCESS" | "FAILED"),
        "unknown simulation status"
    );
    report.simulation_status = Some(status.clone());
    Ok(status)
}

fn validate_quote_fields(quote: &Value, req: &TradeRequest) -> Result<(String, String)> {
    ensure!(
        string(quote, "binanceChainId")? == "56",
        "quote chain mismatch"
    );
    ensure!(
        uint(string(quote, "fromTokenAmount")?)? == uint(&req.amount)?,
        "quote sell amount mismatch"
    );
    for (side, expected) in [
        ("fromToken", &req.from_token_address),
        ("toToken", &req.to_token_address),
    ] {
        ensure!(
            string(&quote[side], "tokenContractAddress")?.eq_ignore_ascii_case(expected),
            "quote token mismatch"
        );
    }
    let amount = string(quote, "toTokenAmount")?;
    ensure!(uint(amount)? > BigUint::from(0u8), "zero output quote");
    Ok((string(quote, "vendorName")?.to_owned(), amount.into()))
}
fn validate_quote(
    quote: &Value,
    req: &TradeRequest,
    policy: &ExecutionPolicy,
) -> Result<QuoteEvidence> {
    let (vendor, buy_amount) = validate_quote_fields(quote, req)?;
    let decimals: u32 = string(&quote["fromToken"], "decimal")?
        .parse()
        .context("invalid token decimals")?;
    ensure!(decimals <= 36, "unsupported token decimals");
    let price = string(&quote["fromToken"], "tokenUnitPrice")?;
    let (price_int, scale) = decimal(price)?;
    ensure!(price_int > BigUint::from(0u8), "invalid sell-token price");
    let notional_numerator = uint(&req.amount)? * price_int;
    let notional_scale = decimals + scale;
    let (limit, limit_scale) = decimal(&policy.risk.max_notional_usd.to_string())?;
    ensure!(
        &notional_numerator * pow10(limit_scale) <= limit * pow10(notional_scale),
        "notional_limit: quote-derived USD amount exceeds policy"
    );
    let impact = string(quote, "priceImpactPercent")?.trim_start_matches('-');
    let (impact_int, impact_scale) = decimal(impact)?;
    let denominator = pow10(impact_scale);
    let bps =
        (impact_int * BigUint::from(100u32) + &denominator - BigUint::from(1u8)) / denominator;
    let bps: u32 = bps
        .to_str_radix(10)
        .parse()
        .context("invalid price impact")?;
    ensure!(
        bps <= policy.risk.max_price_impact_bps,
        "price_impact_limit"
    );
    let mode = string(quote, "executionMode")?;
    ensure!(matches!(mode, "SWAP" | "RFQ"), "unknown execution mode");
    Ok(QuoteEvidence {
        quote_id: string(quote, "quoteId")?.into(),
        vendor,
        execution_mode: mode.into(),
        sell_amount: req.amount.clone(),
        buy_amount,
        sell_token_decimals: decimals,
        sell_token_usd_price: price.into(),
        notional_usd: decimal_string(notional_numerator, notional_scale),
        price_impact_bps: bps,
    })
}
fn optional_balances(
    data: &Value,
    req: &TradeRequest,
) -> Result<(Option<BigUint>, Option<BigUint>)> {
    let groups = data.as_array().context("invalid wallet balance response")?;
    let mut sell = None;
    let mut buy = None;
    for group in groups {
        for asset in group["tokenAssets"]
            .as_array()
            .context("tokenAssets missing")?
        {
            ensure!(
                string(asset, "binanceChainId")? == "56"
                    && string(asset, "address")?.eq_ignore_ascii_case(&req.wallet_address),
                "wallet balance identity mismatch"
            );
            let token = string(asset, "tokenContractAddress")?;
            let destination = if token.eq_ignore_ascii_case(&req.from_token_address) {
                &mut sell
            } else if token.eq_ignore_ascii_case(&req.to_token_address) {
                &mut buy
            } else {
                bail!("unexpected wallet token")
            };
            ensure!(destination.is_none(), "duplicate wallet balance");
            *destination = Some(uint(string(asset, "rawBalance")?)?);
        }
    }
    // Missing entries are unknown, not zero; callers must obtain an explicit balance.
    Ok((sell, buy))
}
fn normalize_tx(tx: &Value, wallet: &str) -> Result<Value> {
    ensure!(
        string(tx, "from")?.eq_ignore_ascii_case(wallet),
        "transaction sender mismatch"
    );
    ensure!(
        valid_address(string(tx, "to")?),
        "invalid transaction destination"
    );
    let value = uint(string(tx, "value")?)?.to_str_radix(10);
    let data = string(tx, "data")?;
    ensure!(
        data.starts_with("0x")
            && data.len() >= 10
            && data.len() % 2 == 0
            && data.len() <= 131074
            && data[2..].bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid transaction calldata"
    );
    Ok(json!({"from":wallet,"to":string(tx,"to")?,"value":value,"data":data}))
}
fn summary(
    kind: &str,
    tx: &Value,
    spender: Option<String>,
    approval_amount: Option<String>,
) -> Result<TransactionSummary> {
    Ok(TransactionSummary {
        kind: kind.into(),
        chain_id: 56,
        from: string(tx, "from")?.into(),
        to: string(tx, "to")?.into(),
        value: string(tx, "value")?.into(),
        calldata_sha256: hash(string(tx, "data")?.as_bytes()),
        spender,
        approval_amount,
    })
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 131074)
        .with_context(|| format!("missing or invalid {key}"))
}
fn allowed(list: &[String], address: &str) -> bool {
    list.iter().any(|a| a.eq_ignore_ascii_case(address))
}
fn valid_address(s: &str) -> bool {
    hex_bytes(s, 20) && s[2..].bytes().any(|b| b != b'0')
}
fn hex_bytes(s: &str, n: usize) -> bool {
    s.len() == 2 + n * 2 && s.starts_with("0x") && s[2..].bytes().all(|b| b.is_ascii_hexdigit())
}
fn uint(s: &str) -> Result<BigUint> {
    ensure!(
        !s.is_empty() && s.len() <= 78 && s.bytes().all(|b| b.is_ascii_digit()),
        "invalid uint256 decimal"
    );
    let value = BigUint::parse_bytes(s.as_bytes(), 10).context("invalid integer")?;
    ensure!(value.bits() <= 256, "uint256 overflow");
    Ok(value)
}
fn decimal(s: &str) -> Result<(BigUint, u32)> {
    ensure!(s.len() <= 100, "decimal too long");
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    ensure!(
        !whole.is_empty()
            && whole.bytes().all(|b| b.is_ascii_digit())
            && frac.bytes().all(|b| b.is_ascii_digit())
            && frac.len() <= 36,
        "invalid unsigned decimal"
    );
    Ok((
        BigUint::parse_bytes(format!("{whole}{frac}").as_bytes(), 10).context("invalid decimal")?,
        frac.len() as u32,
    ))
}
fn pow10(n: u32) -> BigUint {
    BigUint::from(10u8).pow(n)
}
fn decimal_string(n: BigUint, scale: u32) -> String {
    if scale == 0 {
        return n.to_str_radix(10);
    }
    let digits = format!(
        "{:0>width$}",
        n.to_str_radix(10),
        width = scale as usize + 1
    );
    let split = digits.len() - scale as usize;
    format!("{}.{}", &digits[..split], &digits[split..])
        .trim_end_matches('0')
        .trim_end_matches('.')
        .into()
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Serialize)]
pub struct SettlementReport {
    pub schema_version: u32,
    pub confirmation_id: String,
    pub tx_hash: String,
    pub state: String,
    pub receipt: Option<crate::receipt::WatchReport>,
    pub sell_balance_after: Option<String>,
    pub buy_balance_after: Option<String>,
    pub sell_delta: Option<String>,
    pub buy_delta: Option<String>,
    pub errors: Vec<String>,
}
/// Verify that a trusted wallet actually submitted the reviewed transaction, then
/// use the existing Flow receipt tracker and observe balances. API balance deltas
/// can include concurrent activity and are not proof of isolated transaction effects.
pub async fn verify_settlement<
    A: HttpTransport + Clone + 'static,
    R: HttpTransport + Clone + 'static,
>(
    trade: &TradeReport,
    signer_request: &SignerRequest,
    response: &SignerResponse,
    rpc_url: &str,
    api: A,
    rpc: R,
) -> SettlementReport {
    let mut report = SettlementReport {
        schema_version: 1,
        confirmation_id: response.confirmation_id.clone(),
        tx_hash: response.tx_hash.clone(),
        state: "unverified".into(),
        receipt: None,
        sell_balance_after: None,
        buy_balance_after: None,
        sell_delta: None,
        buy_delta: None,
        errors: vec![],
    };
    let result = async {
        ensure!(response.confirmation_id == signer_request.confirmation_id && hex_bytes(&response.tx_hash,32), "signer response mismatch");
        let endpoint = url::Url::parse(rpc_url).map_err(|_|anyhow!("invalid RPC URL"))?;
        ensure!(matches!(endpoint.scheme(),"http"|"https") && endpoint.host_str().is_some() && endpoint.username().is_empty() && endpoint.password().is_none() && endpoint.fragment().is_none(), "invalid RPC endpoint");
        let chain = rpc_read(&rpc,rpc_url,"eth_chainId",json!([])).await?;
        ensure!(hex_quantity(chain.as_str().context("RPC chain missing")?)? == BigUint::from(56u32), "RPC chain mismatch");
        let tx = rpc_read(&rpc,rpc_url,"eth_getTransactionByHash",json!([response.tx_hash])).await?;
        ensure!(!tx.is_null(), "submitted transaction not indexed yet; verify again without resubmitting");
        ensure!(string(&tx,"hash")?.eq_ignore_ascii_case(&response.tx_hash), "transaction hash mismatch");
        for field in ["from","to"] {ensure!(string(&tx,field)?.eq_ignore_ascii_case(string(&signer_request.transaction,field)?), "submitted transaction identity mismatch");}
        ensure!(string(&tx,"input")?.eq_ignore_ascii_case(string(&signer_request.transaction,"data")?), "submitted calldata mismatch");
        ensure!(hex_quantity(string(&tx,"value")?)? == uint(string(&signer_request.transaction,"value")?)?, "submitted value mismatch");
        ensure!(hex_quantity(string(&tx,"chainId")?)? == BigUint::from(56u32), "submitted chain mismatch");
        let options: crate::receipt::WatchOptions = serde_json::from_value(json!({"rpc_url":rpc_url,"tx_hash":response.tx_hash,"chain_id":56,"confirmations":3}))?;
        let receipt = crate::receipt::watch_with_transport(options,rpc).await?;
        let success = receipt.success;
        report.receipt = Some(receipt);
        ensure!(success, "receipt did not confirm successful execution");
        report.state = "confirmed".into();
        let mut observation = trade.clone();
        let req = &trade.request;
        let balances_response = Runner{api}.call(&mut observation,"wallet-after","balance/token-balances-by-address", &[], Some(json!({"address":req.wallet_address,"tokenContractAddresses":[{"binanceChainId":"56","tokenContractAddress":req.from_token_address},{"binanceChainId":"56","tokenContractAddress":req.to_token_address}]}))).await?;
        let (sell,buy) = balances(&balances_response,req)?;
        report.sell_balance_after = Some(sell.to_str_radix(10));
        report.buy_balance_after = Some(buy.to_str_radix(10));
        let sell_before=uint(trade.sell_balance.as_deref().context("pre-trade balance missing")?)?;
        let buy_before=uint(trade.buy_balance.as_deref().context("pre-trade balance missing")?)?;
        report.sell_delta=Some(delta(&sell,&sell_before));
        report.buy_delta=Some(delta(&buy,&buy_before));
        if signer_request.kind == "swap" {
            ensure!(sell_before >= sell && sell_before - &sell == uint(&req.amount)?, "observed sell balance delta differs from requested amount");
            ensure!(buy > buy_before, "buy balance increase not observed yet");
            report.state="confirmed_balances_observed".into();
        } else {report.state="approval_confirmed_reprepare".into();}
        Ok::<(),anyhow::Error>(())
    }.await;
    if let Err(error) = result {
        report.errors.push(format!("{error:#}"));
    }
    report
}
async fn rpc_read<T: HttpTransport>(
    rpc: &T,
    url: &str,
    method: &str,
    params: Value,
) -> Result<Value> {
    use postman_http::request::{HttpMethod, RequestBody};
    let mut request = Request::new(HttpMethod::POST, url);
    request.headers = vec![("Content-Type".into(), "application/json".into())];
    request.body = RequestBody::Json(
        json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string(),
    );
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        rpc.execute(
            request,
            RequestOptions {
                timeout_ms: Some(10_000),
                redirect_policy: RedirectPolicy::DoNotFollow,
                ..Default::default()
            },
        ),
    )
    .await
    .map_err(|_| anyhow!("RPC timed out"))?
    .map_err(|_| anyhow!("RPC request failed"))?;
    ensure!(response.status == 200, "RPC HTTP failure");
    let body: Value =
        serde_json::from_str(&response.body).map_err(|_| anyhow!("invalid RPC JSON"))?;
    ensure!(
        body["jsonrpc"] == "2.0" && body["id"] == 1 && body.get("error").is_none(),
        "RPC envelope or remote error"
    );
    body.get("result").cloned().context("RPC result missing")
}
fn hex_quantity(s: &str) -> Result<BigUint> {
    ensure!(
        s.starts_with("0x")
            && s.len() > 2
            && s.len() <= 66
            && s[2..].bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid EVM quantity"
    );
    BigUint::parse_bytes(&s.as_bytes()[2..], 16).context("invalid EVM quantity")
}
fn delta(after: &BigUint, before: &BigUint) -> String {
    if after >= before {
        (after - before).to_str_radix(10)
    } else {
        format!("-{}", (before - after).to_str_radix(10))
    }
}

fn balances(data: &Value, req: &TradeRequest) -> Result<(BigUint, BigUint)> {
    let (sell, buy) = optional_balances(data, req)?;
    Ok((
        sell.context("sell-token balance missing")?,
        buy.context("buy-token balance missing")?,
    ))
}

/// Fill only omitted balances using ERC-20 balanceOf on an explicitly configured
/// BSC RPC. The API envelope is annotated with the observation source before Flow
/// exports it; this is local enrichment, not a field returned by Binance.
#[derive(Clone)]
pub struct BalanceFallback<T, R> {
    pub api: T,
    pub rpc: R,
    pub rpc_url: Option<String>,
}
impl<T: HttpTransport, R: HttpTransport> HttpTransport for BalanceFallback<T, R> {
    async fn execute(
        &self,
        request: Request,
        options: RequestOptions,
    ) -> Result<HttpResponse, HttpError> {
        use postman_http::request::RequestBody;
        let balance_query = if request.url == format!("{BASE}balance/token-balances-by-address") {
            if let RequestBody::Json(body) = &request.body {
                serde_json::from_str::<Value>(body).ok()
            } else {
                None
            }
        } else {
            None
        };
        let mut response = self.api.execute(request, options).await?;
        if let (Some(query), Some(url)) = (balance_query, self.rpc_url.as_deref()) {
            if response.status == 200 {
                let result=async {
                    let mut data:Value=serde_json::from_str(&response.body).context("invalid wallet response")?;
                    if data["code"]!=0 {return Ok(data)}
                    let address=string(&query,"address")?;
                    ensure!(valid_address(address),"invalid wallet address");
                    let groups=data["data"].as_array().context("invalid balance response")?;
                    let mut missing=vec![];
                    for token in query["tokenContractAddresses"].as_array().context("missing requested tokens")? {
                        let contract=string(token,"tokenContractAddress")?;
                        ensure!(valid_address(contract) && token["binanceChainId"]=="56","invalid requested token");
                        if !groups.iter().filter_map(|g|g["tokenAssets"].as_array()).flatten().any(|a|a["tokenContractAddress"].as_str().is_some_and(|s|s.eq_ignore_ascii_case(contract))) { missing.push(contract.to_owned()); }
                    }
                    if missing.is_empty() {return Ok(data)}
                    validate_rpc_url(url)?;
                    let chain=rpc_read(&self.rpc,url,"eth_chainId",json!([])).await?;
                    ensure!(hex_quantity(chain.as_str().context("chain ID missing")?)? == BigUint::from(56u32),"RPC chain mismatch");
                    let mut assets=vec![];
                    for contract in missing {
                        let calldata=format!("0x70a08231{:0>64}",&address[2..]);
                        let raw=rpc_read(&self.rpc,url,"eth_call",json!([{"to":contract,"data":calldata},"latest"])).await?;
                        let raw=raw.as_str().context("balanceOf result missing")?;
                        ensure!(hex_bytes(raw,32),"invalid balanceOf ABI result");
                        assets.push(json!({"binanceChainId":"56","tokenContractAddress":contract,"address":address,"rawBalance":hex_quantity(raw)?.to_str_radix(10),"flowBnbBalanceSource":"rpc_balanceOf"}));
                    }
                    data["data"].as_array_mut().unwrap().push(json!({"tokenAssets":assets}));
                    Ok::<Value,anyhow::Error>(data)
                }.await;
                response.body = result
                    .map_err(|_| {
                        HttpError::invalid_response(
                            "RPC balance fallback failed; balance remains unknown",
                        )
                    })?
                    .to_string();
            }
        }
        Ok(response)
    }
}
fn validate_rpc_url(url: &str) -> Result<()> {
    let endpoint = url::Url::parse(url).map_err(|_| anyhow!("invalid RPC URL"))?;
    ensure!(
        matches!(endpoint.scheme(), "http" | "https")
            && endpoint.host_str().is_some()
            && endpoint.username().is_empty()
            && endpoint.password().is_none()
            && endpoint.fragment().is_none(),
        "invalid RPC endpoint"
    );
    Ok(())
}
