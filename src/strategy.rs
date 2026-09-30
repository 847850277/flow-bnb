//! Bounded, read-only user workflows. A decision is an intent, never a signature.
use crate::agentic::{self, Config, Intent};
use anyhow::{anyhow, bail, ensure, Context, Result};
use futures::StreamExt;
use postman_flow::{
    compile_flow, execute_flow, parse_flow_yaml, write_flow_yaml, BodyTemplate, CompileEnvironment,
    FlowDocument, FlowEvent, FlowInputs, FlowSessionEnvironment, HttpRequestSource,
    HttpRequestTemplate, HttpStepDefinition,
};
use postman_http::{
    request::{HttpMethod, Request, RequestBody, RequestOptions},
    response::HttpResponse,
    HttpError, HttpTransport,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const TEMPLATE: &str = include_str!("../flows/stock_strategy.http.yml");
pub const SPREAD_TEMPLATE: &str = include_str!("../flows/stock_spread_strategy.http.yml");
const PREFIX: &str = "https://flow-bnb.invalid/strategy/";
const MAX_YAML: usize = 65_536;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub yaml: String,
    pub inputs: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub snapshot_sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub triggered: bool,
    pub intent: Intent,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunReport {
    pub success: bool,
    pub mode: String,
    pub outputs: BTreeMap<String, Value>,
    pub decision: Option<Decision>,
    pub steps: Vec<Value>,
    pub requests: usize,
    pub elapsed_ms: u128,
    pub error: Option<String>,
}

pub fn check(source: &str) -> Result<(FlowDocument, String)> {
    ensure!(source.len() <= MAX_YAML, "strategy YAML exceeds 64 KiB");
    let doc = parse_flow_yaml(source)?;
    compile_flow(&doc.flow, &doc.apis, &CompileEnvironment::default())
        .map_err(|e| anyhow!("flow compile error: {e:?}"))?;
    fn request(r: &HttpRequestTemplate) -> Result<()> {
        ensure!(
            r.auth.is_none() && !matches!(r.body, BodyTemplate::File(_)),
            "strategy cannot read files or supply authentication; credentials are operator-owned"
        );
        ensure!(
            matches!(r.method, HttpMethod::GET | HttpMethod::POST),
            "unsupported strategy method"
        );
        if let [postman_flow::TemplatePart::Literal(url)] = r.url.parts.as_slice() {
            let mut req = Request::new(r.method, url);
            if !matches!(r.body, BodyTemplate::None) {
                req.body = RequestBody::Json("{}".into());
            }
            destination(&req)?;
        }
        Ok(())
    }
    fn steps(
        items: &[HttpStepDefinition],
        doc: &FlowDocument,
        depth: usize,
        count: &mut usize,
    ) -> Result<()> {
        ensure!(depth <= 4, "strategy nesting exceeds 4");
        for s in items {
            *count += 1;
            ensure!(*count <= 128, "strategy exceeds 128 step definitions");
            match &s.request {
                HttpRequestSource::Inline(r) => request(r)?,
                HttpRequestSource::Api(a) => {
                    request(&doc.apis.get(&a.api_id).context("missing API")?.request)?
                }
                HttpRequestSource::ForEach(l) => {
                    ensure!(
                        l.max_iterations <= 32,
                        "strategy loop exceeds 32 iterations"
                    );
                    steps(&l.steps, doc, depth + 1, count)?;
                }
                HttpRequestSource::RepeatUntil(l) => {
                    ensure!(
                        l.max_iterations <= 32 && l.timeout_ms <= 30_000 && l.interval_ms <= 5_000,
                        "strategy polling exceeds bounded run limits"
                    );
                    steps(&l.steps, doc, depth + 1, count)?;
                }
            }
        }
        Ok(())
    }
    steps(&doc.flow.steps, &doc, 0, &mut 0)?;
    let canonical = write_flow_yaml(&doc)?;
    ensure!(canonical.len() <= MAX_YAML, "canonical YAML exceeds 64 KiB");
    Ok((doc, canonical))
}
impl Snapshot {
    pub fn new(yaml: &str, inputs: BTreeMap<String, Value>) -> Result<Self> {
        let (doc, yaml) = check(yaml)?;
        ensure!(
            serde_json::to_vec(&inputs)?.len() <= 16_384,
            "strategy inputs exceed 16 KiB"
        );
        ensure!(
            inputs
                .keys()
                .all(|k| doc.flow.inputs.iter().any(|i| &i.name == k)),
            "undeclared strategy input"
        );
        ensure!(
            doc.flow
                .inputs
                .iter()
                .all(|i| i.default.is_some() || inputs.contains_key(&i.name)),
            "missing required strategy input"
        );
        Ok(Self { yaml, inputs })
    }
    pub fn binding(&self) -> Result<Binding> {
        Ok(Binding {
            snapshot_sha256: format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)),
        })
    }
    pub fn persist(&self, c: &Config) -> Result<Binding> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let dir = c.state_dir.join("strategy-snapshots");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        let m = fs::symlink_metadata(&dir)?;
        ensure!(
            m.is_dir() && !m.file_type().is_symlink() && m.permissions().mode() & 0o077 == 0,
            "invalid strategy snapshot directory"
        );
        let binding = self.binding()?;
        let path = dir.join(format!("{}.json", binding.snapshot_sha256));
        if path.try_exists()? {
            binding.load(c)?;
            return Ok(binding);
        }
        ensure!(
            fs::read_dir(&dir)?.take(1025).count() < 1024,
            "strategy snapshot store full"
        );
        // Stage then hard-link, so concurrent readers never see partial snapshots.
        let tmp = dir.join(format!(
            ".{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        let result = (|| -> Result<()> {
            f.write_all(&serde_json::to_vec(self)?)?;
            f.sync_all()?;
            match fs::hard_link(&tmp, &path) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    binding.load(c)?;
                }
                Err(e) => return Err(e.into()),
            }
            fs::File::open(&dir)?.sync_all()?;
            Ok(())
        })();
        let _ = fs::remove_file(tmp);
        result?;
        Ok(binding)
    }
}
impl Binding {
    pub fn load(&self, c: &Config) -> Result<Snapshot> {
        ensure!(
            self.snapshot_sha256.len() == 64
                && self.snapshot_sha256.bytes().all(|x| x.is_ascii_hexdigit()),
            "invalid strategy snapshot hash"
        );
        let dir = c.state_dir.join("strategy-snapshots");
        ensure!(
            !fs::symlink_metadata(&dir)?.file_type().is_symlink(),
            "symlinked snapshot directory"
        );
        let p = dir.join(format!("{}.json", self.snapshot_sha256));
        let m = fs::symlink_metadata(&p)?;
        ensure!(
            m.is_file() && !m.file_type().is_symlink() && m.len() <= 262_144,
            "invalid strategy snapshot file"
        );
        let s: Snapshot = serde_json::from_slice(&fs::read(p)?)?;
        ensure!(
            s.binding()?.snapshot_sha256 == self.snapshot_sha256,
            "strategy snapshot has changed"
        );
        Snapshot::new(&s.yaml, s.inputs)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Comparison {
    left: String,
    operator: String,
    right: String,
}
fn compare(v: Value) -> Result<Value> {
    let c: Comparison = serde_json::from_value(v)?;
    ensure!(
        c.left.len() <= 100 && c.right.len() <= 100,
        "decimal too long"
    );
    // Compare exact nonnegative decimal amounts, never floating point token amounts.
    let a = agentic::units(&c.left, 36)?;
    let b = agentic::units(&c.right, 36)?;
    let matched = match c.operator.as_str() {
        "eq" => a == b,
        "gt" => a > b,
        "gte" => a >= b,
        "lt" => a < b,
        "lte" => a <= b,
        _ => bail!("comparison operator must be eq/gt/gte/lt/lte"),
    };
    Ok(json!({"matched":matched}))
}

#[derive(Clone)]
struct Backend {
    config: Config,
}
impl HttpTransport for Backend {
    async fn execute(
        &self,
        r: Request,
        options: RequestOptions,
    ) -> Result<HttpResponse, HttpError> {
        if r.url == format!("{PREFIX}quote") {
            let result = async {
                let RequestBody::Json(body) = r.body else {
                    bail!("expected JSON intent")
                };
                let i: Intent = serde_json::from_str(&body)?;
                agentic::strategy_quote(&self.config, &i).await
            }
            .await
            .map_err(|_| {
                HttpError::network(
                    "strategy quote unavailable; check wallet login, pair and local limits",
                )
            })?;
            return Ok(HttpResponse::new(
                200,
                vec![],
                json!({"success":true,"data":result}).to_string(),
            ));
        }
        let key = std::env::var("BINANCE_WEB3_API_KEY").map_err(|_| {
            HttpError::invalid_request(
                "Web3 data step requires operator API credentials; native quote does not",
            )
        })?;
        let secret = std::env::var("BINANCE_WEB3_SECRET_KEY").map_err(|_| {
            HttpError::invalid_request("Web3 data step requires operator API credentials")
        })?;
        crate::BinanceWeb3Transport::new(key, secret)?
            .execute(r, options)
            .await
    }
}

#[derive(Default)]
struct State {
    requests: usize,
    decision: Option<Decision>,
}
#[derive(Clone)]
struct Restricted<T> {
    backend: T,
    config: Config,
    state: Arc<Mutex<State>>,
}
fn destination(r: &Request) -> Result<&'static str> {
    ensure!(
        r.headers.iter().all(
            |(k, _)| k.eq_ignore_ascii_case("content-type") || k.eq_ignore_ascii_case("accept")
        ),
        "custom strategy headers are not permitted"
    );
    if r.url.starts_with(PREFIX) {
        ensure!(
            r.method == HttpMethod::POST,
            "local strategy operations require POST"
        );
        return match &r.url[PREFIX.len()..] {
            "quote" => Ok("quote"),
            "compare" => Ok("compare"),
            "rwa-spread" => Ok("rwa-spread"),
            "decision" => Ok("decision"),
            _ => bail!("unknown strategy operation; direct swap/signing is not permitted"),
        };
    }
    let u = url::Url::parse(&r.url)?;
    ensure!(
        r.method == HttpMethod::GET
            && matches!(r.body, RequestBody::None)
            && u.scheme() == "https"
            && u.host_str() == Some("web3.binance.com")
            && u.port().is_none()
            && u.username().is_empty()
            && u.password().is_none()
            && u.fragment().is_none()
            && matches!(
                u.path(),
                "/build/api/v1/dex/market/rwa/platforms"
                    | "/build/api/v1/dex/market/rwa/search"
                    | "/build/api/v1/dex/market/rwa/price"
                    | "/build/api/v1/dex/balance/all-token-balances-by-address"
            ),
        "strategy destination is outside the read-only allowlist"
    );
    Ok("web3")
}
impl<T: HttpTransport> HttpTransport for Restricted<T> {
    async fn execute(
        &self,
        r: Request,
        mut options: RequestOptions,
    ) -> Result<HttpResponse, HttpError> {
        let run = async {
            {
                let mut state = self.state.lock().unwrap();
                state.requests += 1;
                ensure!(state.requests <= 32, "strategy exceeded 32 requests");
            }
            let op = destination(&r)?;
            if op == "web3" {
                options.timeout_ms = Some(10_000);
                let response = self.backend.execute(r, options).await?;
                ensure!(
                    response.body.len() <= 1_048_576,
                    "strategy response too large"
                );
                return Ok(response);
            }
            let RequestBody::Json(ref body) = r.body else {
                bail!("local operation requires JSON")
            };
            ensure!(body.len() <= 16_384, "strategy operation exceeds 16 KiB");
            let value: Value = serde_json::from_str(body)?;
            let data = match op {
                "compare" => compare(value)?,
                "rwa-spread" => crate::rwa::spread(&self.config, value)?,
                "decision" => {
                    let d: Decision = serde_json::from_value(value)?;
                    self.config.rules(&d.intent)?;
                    let mut state = self.state.lock().unwrap();
                    ensure!(
                        state.decision.is_none(),
                        "at most one trade decision per run"
                    );
                    state.decision = Some(d.clone());
                    serde_json::to_value(d)?
                }
                "quote" => {
                    let i: Intent = serde_json::from_value(value)?;
                    self.config.rules(&i)?;
                    options.timeout_ms = Some(10_000);
                    let response = self.backend.execute(r, options).await?;
                    ensure!(
                        response.body.len() <= 1_048_576,
                        "strategy quote response too large"
                    );
                    return Ok(response);
                }
                _ => unreachable!(),
            };
            Ok::<_, anyhow::Error>(HttpResponse::new(
                200,
                vec![],
                json!({"success":true,"data":data}).to_string(),
            ))
        };
        run.await
            .map_err(|e| HttpError::invalid_request(format!("strategy: {e}")))
    }
}

pub async fn run(c: &Config, s: &Snapshot) -> Result<RunReport> {
    run_with(c, s, Backend { config: c.clone() }).await
}
async fn run_with<T: HttpTransport>(c: &Config, s: &Snapshot, backend: T) -> Result<RunReport> {
    let (doc, _) = check(&s.yaml)?;
    let plan = compile_flow(&doc.flow, &doc.apis, &CompileEnvironment::default())
        .map_err(|e| anyhow!("{e:?}"))?;
    let mut inputs = FlowInputs::new();
    for (k, v) in &s.inputs {
        inputs.insert(k, v.clone());
    }
    let state = Arc::new(Mutex::new(State::default()));
    let stream = execute_flow(
        plan,
        Restricted {
            backend,
            config: c.clone(),
            state: state.clone(),
        },
        FlowSessionEnvironment::new(inputs),
    )?;
    let mut stream = std::pin::pin!(stream);
    let started = Instant::now();
    let mut report = RunReport {
        success: false,
        mode: "read_only".into(),
        outputs: Default::default(),
        decision: None,
        steps: vec![],
        requests: 0,
        elapsed_ms: 0,
        error: None,
    };
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let mut events = 0;
        while let Some(e) = stream.next().await {
            events += 1;
            ensure!(events <= 2048, "strategy event limit reached");
            match e? {
                FlowEvent::StepSkipped {
                    step_id, reason, ..
                } => report
                    .steps
                    .push(json!({"step_id":step_id,"outcome":"skipped","reason":reason})),
                FlowEvent::StepFinished { step_id, outcome } => report
                    .steps
                    .push(json!({"step_id":step_id,"outcome":format!("{outcome:?}")})),
                FlowEvent::FlowFinished { success, outputs } => {
                    report.success = success;
                    for (k, v) in outputs {
                        report.outputs.insert(
                            k,
                            if v.is_sensitive() {
                                json!("[REDACTED]")
                            } else {
                                v.value().clone()
                            },
                        );
                    }
                    if !success {
                        report.error =
                            Some("flow checks or conditions failed; no intent queued".into());
                    }
                    return Ok(());
                }
                _ => (),
            }
        }
        bail!("strategy ended without a result")
    })
    .await;
    match result {
        Ok(Ok(())) => (),
        Ok(Err(e)) => report.error = Some(e.to_string()),
        Err(_) => report.error = Some("strategy exceeded 30 seconds; no intent queued".into()),
    }
    report.requests = state.lock().unwrap().requests;
    report.elapsed_ms = started.elapsed().as_millis();
    if report.success {
        report.decision = state.lock().unwrap().decision.clone();
    }
    ensure!(
        serde_json::to_vec(&report)?.len() <= 262_144,
        "strategy report exceeds 256 KiB; export fewer fields"
    );
    Ok(report)
}

pub fn check_decision(report: &RunReport, intent: &Intent) -> Result<()> {
    ensure!(
        report.success,
        "strategy recheck failed; no order submitted"
    );
    let d = report
        .decision
        .as_ref()
        .context("strategy produced no trade decision")?;
    ensure!(
        d.triggered,
        "strategy condition no longer holds; no order submitted"
    );
    ensure!(
        serde_json::to_value(&d.intent)? == serde_json::to_value(intent)?,
        "strategy now selects a different intent; no order submitted"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    fn config() -> (tempfile::TempDir, Config) {
        let d = tempfile::tempdir().unwrap();
        let mut v: Value =
            serde_json::from_str(include_str!("../examples/agentic-config.json")).unwrap();
        let executable = d.path().join("unused-backend");
        fs::write(&executable, "unused").unwrap();
        v["executable"] = json!(executable);
        v["wallet_address"] = json!(format!("0x{}", "1".repeat(40)));
        v["state_dir"] = json!(d.path().join("state"));
        let path = d.path().join("config.json");
        fs::write(&path, v.to_string()).unwrap();
        let c = Config::read(&path).unwrap();
        (d, c)
    }
    #[derive(Clone)]
    struct Quote {
        amount: &'static str,
        calls: Arc<AtomicUsize>,
        status: u16,
    }
    impl HttpTransport for Quote {
        async fn execute(&self, r: Request, _: RequestOptions) -> Result<HttpResponse, HttpError> {
            assert_eq!(r.url, format!("{PREFIX}quote"));
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(HttpResponse::new(
                self.status,
                vec![],
                json!({"success":true,"data":{"toCoinAmount":self.amount}}).to_string(),
            ))
        }
    }
    fn quote(amount: &'static str) -> Quote {
        Quote {
            amount,
            calls: Default::default(),
            status: 200,
        }
    }
    #[derive(Clone)]
    struct RwaPrice {
        body: Value,
        calls: Arc<AtomicUsize>,
    }
    impl HttpTransport for RwaPrice {
        async fn execute(&self, r: Request, _: RequestOptions) -> Result<HttpResponse, HttpError> {
            assert_eq!(r.method, HttpMethod::GET);
            assert_eq!(r.url, "https://web3.binance.com/build/api/v1/dex/market/rwa/price?binanceChainId=56&tokenContractAddresses=0x390a684ef9cade28a7ad0dfa61ab1eb3842618c4");
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(HttpResponse::new(200, vec![], self.body.to_string()))
        }
    }
    fn rwa_price(price: &str) -> RwaPrice {
        RwaPrice {
            body: json!({"code":0,"data":[{
                "binanceChainId":"56",
                "tokenContractAddress":"0x390a684ef9cade28a7ad0dfa61ab1eb3842618c4",
                "platformId":"ondo","tokenPrice":price,"referencePrice":"100",
                "tokenPriceUpdatedAt":chrono::Utc::now().timestamp_millis(),
            }]}),
            calls: Default::default(),
        }
    }
    #[tokio::test]
    async fn spread_strategy_reports_signed_signal_without_wallet_or_state_writes() {
        let (_d, c) = config();
        let s = Snapshot::new(SPREAD_TEMPLATE, BTreeMap::new()).unwrap();
        for (price, triggered, bps) in [("99", true, "-100"), ("100.5", false, "50")] {
            let backend = rwa_price(price);
            let r = run_with(&c, &s, backend.clone()).await.unwrap();
            assert!(r.success, "{r:?}");
            assert_eq!(r.decision.unwrap().triggered, triggered);
            assert_eq!(r.outputs["threshold_met"], triggered);
            assert_eq!(r.outputs["spread"]["spread_bps"], bps);
            assert_eq!(
                r.outputs["spread"]["reference_price_basis"],
                "token_derived_per_share"
            );
            assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
            assert_eq!(fs::read_dir(&c.state_dir).unwrap().count(), 0);
        }
        // The same condition supports a premium-side sell with frozen inputs.
        let s = Snapshot::new(
            SPREAD_TEMPLATE,
            BTreeMap::from([
                ("operator".into(), json!("gte")),
                ("threshold_bps".into(), json!(100)),
                (
                    "intent".into(),
                    json!({
                        "from_token":"0x390a684ef9cade28a7ad0dfa61ab1eb3842618c4",
                        "to_token":"0x55d398326f99059fF775485246999027B3197955",
                        "amount":"0.01","slippage_bps":50,
                    }),
                ),
            ]),
        )
        .unwrap();
        let r = run_with(&c, &s, rwa_price("101")).await.unwrap();
        assert!(r.success && r.decision.as_ref().unwrap().triggered, "{r:?}");
    }
    #[tokio::test]
    async fn spread_recheck_blocks_a_disappeared_signal_or_unusable_data() {
        let (_d, c) = config();
        let s = Snapshot::new(SPREAD_TEMPLATE, BTreeMap::new()).unwrap();
        let initial = run_with(&c, &s, rwa_price("99")).await.unwrap();
        let intent = initial.decision.as_ref().unwrap().intent.clone();
        assert!(check_decision(&initial, &intent).is_ok());
        let changed = run_with(&c, &s, rwa_price("99.000000000000000001"))
            .await
            .unwrap();
        assert!(changed.success);
        assert!(check_decision(&changed, &intent).is_err());
        for (pointer, value) in [
            ("/code", json!(40375)),
            ("/data/0/referencePrice", json!("0")),
            ("/data/0/tokenPriceUpdatedAt", json!(1)),
            (
                "/data/0/tokenContractAddress",
                json!("0x1111111111111111111111111111111111111111"),
            ),
        ] {
            let mut backend = rwa_price("99");
            *backend.body.pointer_mut(pointer).unwrap() = value;
            let failed = run_with(&c, &s, backend).await.unwrap();
            assert!(!failed.success && failed.decision.is_none(), "{failed:?}");
            assert!(check_decision(&failed, &intent).is_err());
        }
        assert_eq!(fs::read_dir(&c.state_dir).unwrap().count(), 0);
    }
    #[tokio::test]
    async fn custom_strategy_uses_exact_threshold_and_never_submits_or_queues() {
        let (_d, c) = config();
        let s = Snapshot::new(TEMPLATE, BTreeMap::new()).unwrap();
        for (amount, triggered) in [
            ("0.020000000000000001", true),
            ("0.019999999999999999", false),
        ] {
            let backend = quote(amount);
            let r = run_with(&c, &s, backend.clone()).await.unwrap();
            assert!(r.success, "{:?}", r);
            assert_eq!(r.decision.unwrap().triggered, triggered);
            assert_eq!(r.outputs["quoted_receive"], amount);
            assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
            assert_eq!(fs::read_dir(&c.state_dir).unwrap().count(), 0);
        }
    }
    #[tokio::test]
    async fn failing_response_or_late_check_cannot_emit_decision() {
        let (_d, c) = config();
        let s = Snapshot::new(TEMPLATE, BTreeMap::new()).unwrap();
        let mut backend = quote("0.03");
        backend.status = 500;
        let r = run_with(&c, &s, backend).await.unwrap();
        assert!(!r.success && r.decision.is_none());
        let source = TEMPLATE;
        // A decision may be computed, but a later failing check invalidates the entire run.
        let source = source.replace("  outputs:\n", "      exports:\n        - {name: should_not_exist, path: '$.does_not_exist'}\n  outputs:\n");
        let s = Snapshot::new(&source, BTreeMap::new()).unwrap();
        let r = run_with(&c, &s, quote("0.03")).await.unwrap();
        assert!(!r.success && r.decision.is_none());
    }
    #[test]
    fn static_restrictions_reject_swaps_network_escapes_and_polling_abuse() {
        for url in [
            "https://agentic-wallet.invalid/local",
            "http://127.0.0.1:8545",
            "https://flow-bnb.invalid/strategy/swap",
            "https://web3.binance.com/build/api/v1/dex/aggregator/swap",
        ] {
            assert!(
                Snapshot::new(
                    &TEMPLATE.replace(&format!("{PREFIX}quote"), url),
                    BTreeMap::new()
                )
                .is_err(),
                "{url}"
            );
        }
        assert!(check(crate::agentic::ORDER).is_err());
        assert!(Snapshot::new(TEMPLATE, BTreeMap::from([("extra".into(), json!(1))])).is_err());
        assert!(compare(json!({"left":"NaN","right":"1","operator":"lt"})).is_err());
    }
    #[tokio::test]
    async fn dynamic_destination_cannot_escape_runtime_gate() {
        let (_d, c) = config();
        let yaml = TEMPLATE
            .replace("  inputs:\n", "  inputs:\n    - name: endpoint\n")
            .replace(
                &format!("{{literal: '{PREFIX}quote'}}"),
                "{input: endpoint}",
            );
        let s = Snapshot::new(
            &yaml,
            BTreeMap::from([("endpoint".into(), json!("http://127.0.0.1:8545"))]),
        )
        .unwrap();
        let backend = quote("0.03");
        let r = run_with(&c, &s, backend.clone()).await.unwrap();
        assert!(!r.success && r.decision.is_none());
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn duplicate_decisions_and_out_of_policy_intents_fail_closed() {
        let (_d, c) = config();
        let yaml = TEMPLATE.replace(
            "  outputs:\n",
            &format!(
                "{}  outputs:\n",
                TEMPLATE
                    .split("    - id: decision")
                    .nth(1)
                    .unwrap()
                    .split("  outputs:")
                    .next()
                    .map(|s| format!("    - id: second{s}"))
                    .unwrap()
            ),
        );
        let s = Snapshot::new(&yaml, BTreeMap::new()).unwrap();
        let r = run_with(&c, &s, quote("0.03")).await.unwrap();
        assert!(!r.success && r.decision.is_none());
        let s = Snapshot::new(
            &TEMPLATE.replace("amount: '6'", "amount: '7'"),
            BTreeMap::new(),
        )
        .unwrap();
        let backend = quote("0.03");
        assert!(!run_with(&c, &s, backend.clone()).await.unwrap().success);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn snapshot_tampering_and_changed_conditions_block_operator_recheck() {
        let (_d, c) = config();
        let s = Snapshot::new(TEMPLATE, BTreeMap::new()).unwrap();
        let binding = s.persist(&c).unwrap();
        assert_eq!(binding.load(&c).unwrap().yaml, s.yaml);
        let passed = run_with(&c, &s, quote("0.03")).await.unwrap();
        let intent = passed.decision.as_ref().unwrap().intent.clone();
        assert!(check_decision(&passed, &intent).is_ok());
        let failed = run_with(&c, &s, quote("0.01")).await.unwrap();
        assert!(check_decision(&failed, &intent).is_err());
        let mut other = intent.clone();
        other.amount = "5".into();
        assert!(check_decision(&passed, &other).is_err());
        let p = c
            .state_dir
            .join("strategy-snapshots")
            .join(format!("{}.json", binding.snapshot_sha256));
        fs::write(p, "{}").unwrap();
        assert!(binding.load(&c).is_err());
    }
    #[tokio::test]
    async fn runtime_budget_stops_even_valid_requests() {
        let (_d, c) = config();
        let state = Arc::new(Mutex::new(State::default()));
        let backend = quote("0.02");
        let t = Restricted {
            config: c,
            backend: backend.clone(),
            state,
        };
        for n in 0..33 {
            let mut r = Request::new(HttpMethod::POST, format!("{PREFIX}compare"));
            r.body = RequestBody::Json(json!({"left":"1","operator":"eq","right":"1"}).to_string());
            assert_eq!(
                t.execute(r, RequestOptions::default()).await.is_ok(),
                n < 32
            );
        }
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }
}
