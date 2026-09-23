//! Read-only receipt tracking. Flow owns repetition; this adapter owns EVM data validation.
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use futures::StreamExt;
use postman_flow::{
    compile_flow, execute_flow, parse_flow_yaml, CompileEnvironment, FlowEvent, FlowInputs,
    FlowSessionEnvironment, HttpRequestSource, LoopFinishReason,
};
use postman_http::{
    request::{HttpMethod, RedirectPolicy, Request, RequestBody, RequestOptions},
    HttpError, HttpResponse, HttpTransport,
};
use postman_request::RequestClient;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const TRANSACTION_RECEIPT: &str = include_str!("../flows/transaction_receipt.http.yml");
fn default_chain() -> u64 {
    56
}
fn default_confirmations() -> u64 {
    3
}
fn default_timeout() -> u64 {
    120_000
}
fn default_interval() -> u64 {
    1_000
}
fn default_iterations() -> usize {
    120
}
fn default_request_timeout() -> u64 {
    10_000
}

#[derive(Clone, clap::Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WatchOptions {
    /// HTTP(S) JSON-RPC endpoint. Kept out of reports; may contain a provider API key.
    #[arg(long, env = "FLOW_BNB_RPC_URL")]
    pub rpc_url: String,
    /// Hash of a transaction already broadcast by a wallet (0x + 64 hex digits).
    #[arg(long)]
    pub tx_hash: String,
    /// Expected EVM chain ID, as a decimal integer. Defaults to BSC mainnet.
    #[arg(long, default_value_t = default_chain())]
    #[serde(default = "default_chain")]
    pub chain_id: u64,
    /// Inclusion block counts as confirmation 1. This is not consensus finality.
    #[arg(long, default_value_t = default_confirmations())]
    #[serde(default = "default_confirmations")]
    pub confirmations: u64,
    #[arg(long, default_value_t = default_timeout())]
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[arg(long, default_value_t = default_interval())]
    #[serde(default = "default_interval")]
    pub interval_ms: u64,
    #[arg(long, default_value_t = default_iterations())]
    #[serde(default = "default_iterations")]
    pub max_iterations: usize,
    #[arg(long, default_value_t = default_request_timeout())]
    #[serde(default = "default_request_timeout")]
    pub request_timeout_ms: u64,
}

impl WatchOptions {
    fn validate(&self) -> Result<()> {
        let url = url::Url::parse(&self.rpc_url).map_err(|_| anyhow::anyhow!("invalid RPC URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            bail!("RPC URL must be HTTP(S), without userinfo or a fragment");
        }
        if !is_hash(&self.tx_hash) {
            bail!("tx_hash must be 0x followed by 64 hexadecimal digits");
        }
        if self.chain_id == 0 || self.confirmations == 0 || self.confirmations > 10_000 {
            bail!("chain_id must be positive and confirmations must be in 1..=10000");
        }
        if !(1..=86_400_000).contains(&self.timeout_ms)
            || !(1..=60_000).contains(&self.interval_ms)
            || !(1..=10_000).contains(&self.max_iterations)
            || !(1..=60_000).contains(&self.request_timeout_ms)
        {
            bail!("invalid polling limits: timeout 1..=86400000 ms, interval/request timeout 1..=60000 ms, iterations 1..=10000");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptState {
    Pending,
    Confirming,
    Confirmed,
    Reverted,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReceiptObservation {
    pub state: ReceiptState,
    pub confirmations: u64,
    pub block_number: Option<u64>,
    pub block_hash: Option<String>,
    pub gas_used: Option<String>,
}
impl Default for ReceiptObservation {
    fn default() -> Self {
        Self {
            state: ReceiptState::Pending,
            confirmations: 0,
            block_number: None,
            block_hash: None,
            gas_used: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WatchOutcome {
    Confirmed,
    Reverted,
    Timeout,
    MaxIterations,
    RpcError,
    Cancelled,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WatchReport {
    pub schema_version: u32,
    pub success: bool,
    pub outcome: WatchOutcome,
    pub chain_id: u64,
    pub tx_hash: String,
    pub required_confirmations: u64,
    pub iterations: usize,
    pub elapsed_ms: u64,
    pub observation: ReceiptObservation,
    /// Local validation/transport diagnostic, never the provider's response body or RPC URL.
    pub error: Option<String>,
}

pub async fn watch_transaction(options: WatchOptions) -> Result<WatchReport> {
    let client = RequestClient::try_new(concat!("flow-bnb/", env!("CARGO_PKG_VERSION")))?;
    watch_with_transport(options, client).await
}

/// Same engine path as CLI/MCP, with an injectable HTTP transport for deterministic tests.
pub async fn watch_with_transport<T: HttpTransport>(
    options: WatchOptions,
    inner: T,
) -> Result<WatchReport> {
    options.validate()?;
    let mut document = parse_flow_yaml(TRANSACTION_RECEIPT)?;
    let HttpRequestSource::RepeatUntil(wait) = &mut document.flow.steps[1].request else {
        bail!("receipt template must contain wait-receipt");
    };
    wait.max_iterations = options.max_iterations;
    wait.interval_ms = options.interval_ms;
    wait.timeout_ms = options.timeout_ms;
    let plan = compile_flow(
        &document.flow,
        &document.apis,
        &CompileEnvironment::default(),
    )
    .map_err(|_| anyhow::anyhow!("receipt template failed compilation"))?;
    let mut inputs = FlowInputs::new();
    inputs.insert("rpc_url", json!(options.rpc_url));
    inputs.insert("tx_hash", json!(options.tx_hash));
    inputs.insert("chain_id_hex", json!(format!("0x{:x}", options.chain_id)));
    let observation = Arc::new(Mutex::new(ReceiptObservation::default()));
    let error = Arc::new(Mutex::new(None));
    let transport = ReceiptTransport {
        inner,
        options: options.clone(),
        observation: observation.clone(),
        error: error.clone(),
    };
    let session = FlowSessionEnvironment::new(inputs).with_request_options(RequestOptions {
        timeout_ms: Some(options.request_timeout_ms),
        redirect_policy: RedirectPolicy::DoNotFollow,
        ..RequestOptions::default()
    });
    let started = Instant::now();
    let events = execute_flow(plan, transport, session)?;
    let mut events = std::pin::pin!(events);
    let mut success = false;
    let mut outcome = WatchOutcome::RpcError;
    let mut iterations = 0;
    while let Some(event) = events.next().await {
        match event.context("receipt flow execution failed")? {
            FlowEvent::LoopFinished {
                reason,
                total_executed,
                ..
            } => {
                iterations = total_executed;
                outcome = match reason {
                    LoopFinishReason::ConditionMet => WatchOutcome::Confirmed,
                    LoopFinishReason::FailureCondition => WatchOutcome::Reverted,
                    LoopFinishReason::Timeout => WatchOutcome::Timeout,
                    LoopFinishReason::MaxIterations => WatchOutcome::MaxIterations,
                    LoopFinishReason::Cancelled => WatchOutcome::Cancelled,
                    _ => WatchOutcome::RpcError,
                };
            }
            FlowEvent::FlowFinished {
                success: finished, ..
            } => success = finished,
            _ => {}
        }
    }
    let observation = observation.lock().unwrap().clone();
    let failure = error.lock().unwrap().clone();
    let error = failure.map(|(failure_outcome, message)| {
        outcome = failure_outcome;
        message
    });
    Ok(WatchReport {
        schema_version: 1,
        success,
        outcome,
        chain_id: options.chain_id,
        tx_hash: options.tx_hash.to_ascii_lowercase(),
        required_confirmations: options.confirmations,
        iterations,
        elapsed_ms: started.elapsed().as_millis() as u64,
        observation,
        error,
    })
}

struct ReceiptTransport<T> {
    inner: T,
    options: WatchOptions,
    observation: Arc<Mutex<ReceiptObservation>>,
    error: Arc<Mutex<Option<(WatchOutcome, String)>>>,
}

impl<T: HttpTransport> ReceiptTransport<T> {
    async fn rpc(
        &self,
        method: &str,
        params: Value,
        id: u64,
        options: RequestOptions,
    ) -> Result<(HttpResponse, Value), HttpError> {
        // Construct requests here: no caller headers, Binance credentials, cookies or redirects.
        let mut request = Request::new(HttpMethod::POST, &self.options.rpc_url);
        request.headers = vec![("Content-Type".into(), "application/json".into())];
        request.body = RequestBody::Json(
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string(),
        );
        let mut options = options;
        options.redirect_policy = RedirectPolicy::DoNotFollow;
        let response = self
            .inner
            .execute(request, options)
            .await
            .map_err(|error| match error {
                HttpError::Cancelled | HttpError::Timeout { .. } => error,
                _ => HttpError::network(
                    "RPC transport failed (endpoint and provider details omitted)",
                ),
            })?;
        if response.status != 200 {
            return Err(HttpError::invalid_response(format!(
                "RPC HTTP status {}",
                response.status
            )));
        }
        let body: Value = serde_json::from_str(&response.body)
            .map_err(|_| HttpError::invalid_response("RPC returned invalid JSON"))?;
        if body["jsonrpc"] != "2.0" || body["id"] != id {
            return Err(HttpError::invalid_response(
                "RPC envelope or response ID mismatch",
            ));
        }
        if body.get("error").is_some() {
            return Err(HttpError::invalid_response("RPC returned an error"));
        }
        if body.get("result").is_none() {
            return Err(HttpError::invalid_response(
                "RPC response is missing result",
            ));
        }
        Ok((response, body))
    }

    async fn execute_read(
        &self,
        request: Request,
        options: RequestOptions,
    ) -> Result<HttpResponse, HttpError> {
        let RequestBody::Json(body) = request.body else {
            return Err(HttpError::invalid_request("receipt request must be JSON"));
        };
        let body: Value = serde_json::from_str(&body)
            .map_err(|_| HttpError::invalid_request("invalid receipt request"))?;
        match body["method"].as_str() {
            Some("eth_chainId") => {
                let (mut response, mut body) =
                    self.rpc("eth_chainId", json!([]), 1, options).await?;
                if quantity(&body["result"])? != self.options.chain_id {
                    return Err(HttpError::invalid_response(
                        "RPC chain ID does not match expected chain",
                    ));
                }
                // Compare chain IDs numerically, then expose canonical hex to the YAML check.
                body["result"] = json!(format!("0x{:x}", self.options.chain_id));
                response.body = body.to_string();
                Ok(response)
            }
            Some("eth_getTransactionReceipt") => {
                let (mut response, mut body) = self
                    .rpc(
                        "eth_getTransactionReceipt",
                        json!([self.options.tx_hash]),
                        2,
                        options,
                    )
                    .await?;
                let receipt = &body["result"];
                let mut observation = ReceiptObservation::default();
                if !receipt.is_null() {
                    let tx = receipt["transactionHash"].as_str().unwrap_or("");
                    let hash = receipt["blockHash"].as_str().unwrap_or("");
                    if !tx.eq_ignore_ascii_case(&self.options.tx_hash) || !is_hash(hash) {
                        return Err(HttpError::invalid_response(
                            "RPC receipt transaction/block hash mismatch",
                        ));
                    }
                    let number = quantity(&receipt["blockNumber"])?;
                    let status = quantity(&receipt["status"])?;
                    if status > 1 {
                        return Err(HttpError::invalid_response("invalid receipt status"));
                    }
                    let gas_used = receipt["gasUsed"]
                        .as_str()
                        .ok_or_else(|| HttpError::invalid_response("receipt missing gasUsed"))?;
                    quantity(&receipt["gasUsed"])?;
                    let (_, head) = self.rpc("eth_blockNumber", json!([]), 3, options).await?;
                    let head = quantity(&head["result"])?;
                    let (_, canonical) = self
                        .rpc(
                            "eth_getBlockByNumber",
                            json!([format!("0x{number:x}"), false]),
                            4,
                            options,
                        )
                        .await?;
                    let block = &canonical["result"];
                    if !block.is_null() {
                        let canonical_hash = block["hash"]
                            .as_str()
                            .filter(|h| is_hash(h))
                            .ok_or_else(|| {
                                HttpError::invalid_response("invalid canonical block hash")
                            })?;
                        if quantity(&block["number"])? != number {
                            return Err(HttpError::invalid_response(
                                "canonical block number mismatch",
                            ));
                        }
                        // A changed block hash, missing block or stale head means keep waiting.
                        if canonical_hash.eq_ignore_ascii_case(hash) && head >= number {
                            let confirmations = head
                                .checked_sub(number)
                                .and_then(|n| n.checked_add(1))
                                .ok_or_else(|| {
                                    HttpError::invalid_response("confirmation count overflow")
                                })?;
                            observation = ReceiptObservation {
                                state: if status == 0 {
                                    ReceiptState::Reverted
                                } else if confirmations >= self.options.confirmations {
                                    ReceiptState::Confirmed
                                } else {
                                    ReceiptState::Confirming
                                },
                                confirmations,
                                block_number: Some(number),
                                block_hash: Some(hash.to_ascii_lowercase()),
                                gas_used: Some(gas_used.to_owned()),
                            };
                        }
                    }
                }
                // Keep raw JSON-RPC result; add domain observations for Flow's conditions.
                body["tracking"] =
                    serde_json::to_value(&observation).expect("serializable observation");
                *self.observation.lock().unwrap() = observation;
                response.body = body.to_string();
                Ok(response)
            }
            _ => Err(HttpError::invalid_request(
                "receipt adapter only permits chain and receipt reads",
            )),
        }
    }
}
impl<T: HttpTransport> HttpTransport for ReceiptTransport<T> {
    async fn execute(
        &self,
        request: Request,
        options: RequestOptions,
    ) -> Result<HttpResponse, HttpError> {
        // The engine clamps the transport budget to its enclosing loop deadline.
        // One receipt observation performs several RPCs; all must share that budget,
        // including an underlying transport that does not enforce timeouts itself.
        let timeout_ms = options
            .timeout_ms
            .unwrap_or(self.options.request_timeout_ms);
        let result = tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            self.execute_read(request, options),
        )
        .await
        .unwrap_or(Err(HttpError::Timeout { timeout_ms }));
        if let Err(error) = &result {
            let outcome = match error {
                HttpError::Cancelled => WatchOutcome::Cancelled,
                // The engine rounds remaining time to milliseconds. Preserve loop
                // timeout semantics even if that rounding expires just before its clock.
                HttpError::Timeout { .. } if timeout_ms < self.options.request_timeout_ms => {
                    WatchOutcome::Timeout
                }
                _ => WatchOutcome::RpcError,
            };
            *self.error.lock().unwrap() = Some((outcome, error.to_string()));
        }
        result
    }
}
fn is_hash(value: &str) -> bool {
    value.len() == 66
        && value.starts_with("0x")
        && value[2..].bytes().all(|b| b.is_ascii_hexdigit())
}
fn quantity(value: &Value) -> Result<u64, HttpError> {
    let hex = value
        .as_str()
        .and_then(|s| s.strip_prefix("0x"))
        .filter(|s| {
            !s.is_empty()
                && (s.len() == 1 || !s.starts_with('0'))
                && s.bytes().all(|b| b.is_ascii_hexdigit())
        })
        .ok_or_else(|| HttpError::invalid_response("invalid RPC hex quantity"))?;
    u64::from_str_radix(hex, 16)
        .map_err(|_| HttpError::invalid_response("RPC quantity exceeds u64"))
}
