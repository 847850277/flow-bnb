use flow_bnb::trade::{prepare_with_transport, ExecutionPolicy, TradeRequest};
use postman_http::{
    request::{RedirectPolicy, Request, RequestBody, RequestOptions},
    HttpError, HttpResponse, HttpTransport,
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
const WALLET: &str = "0x1111111111111111111111111111111111111111";
const SELL: &str = "0x2222222222222222222222222222222222222222";
const BUY: &str = "0x3333333333333333333333333333333333333333";
const ROUTER: &str = "0x4444444444444444444444444444444444444444";
const SPENDER: &str = "0x5555555555555555555555555555555555555555";
#[derive(Clone)]
struct Script {
    responses: Arc<Mutex<VecDeque<(&'static str, Value)>>>,
    requests: Arc<Mutex<Vec<Request>>>,
}
impl Script {
    fn new(responses: Vec<(&'static str, Value)>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into())),
            requests: Default::default(),
        }
    }
    fn done(&self) {
        assert!(self.responses.lock().unwrap().is_empty());
    }
}
impl HttpTransport for Script {
    async fn execute(
        &self,
        request: Request,
        options: RequestOptions,
    ) -> Result<HttpResponse, HttpError> {
        assert_eq!(options.redirect_policy, RedirectPolicy::DoNotFollow);
        let (path, response) = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected network request");
        let url = url::Url::parse(&request.url).unwrap();
        assert!(url.path().ends_with(path));
        self.requests.lock().unwrap().push(request);
        Ok(HttpResponse::new(200, vec![], response.to_string()))
    }
}
fn ok(data: Value) -> Value {
    json!({"code":0,"data":data})
}
fn request() -> TradeRequest {
    TradeRequest {
        wallet_address: WALLET.into(),
        from_token_address: SELL.into(),
        to_token_address: BUY.into(),
        amount: "5000000000000000000".into(),
        slippage_bps: 50,
    }
}
fn policy() -> ExecutionPolicy {
    ExecutionPolicy {
        allowed_routers: vec![ROUTER.into()],
        allowed_spenders: vec![SPENDER.into()],
        ..Default::default()
    }
}
fn balance() -> Value {
    ok(
        json!([{"tokenAssets":[{"binanceChainId":"56","tokenContractAddress":SELL,"address":WALLET,"rawBalance":"10000000000000000000"},{"binanceChainId":"56","tokenContractAddress":BUY,"address":WALLET,"rawBalance":"0"}]}]),
    )
}
fn quote() -> Value {
    json!({"binanceChainId":"56","quoteId":"quote-1","vendorName":"Pancake","executionMode":"SWAP","fromTokenAmount":"5000000000000000000","toTokenAmount":"1000000","priceImpactPercent":"-0.015","fromToken":{"tokenContractAddress":SELL,"decimal":"18","tokenUnitPrice":"1"},"toToken":{"tokenContractAddress":BUY},"approveTarget":null})
}
fn build(q: &Value) -> Value {
    ok(
        json!({"executionMode":q["executionMode"],"routerResult":q,"tx":{"from":WALLET,"to":ROUTER,"value":"0","data":"0x12345678","minReceiveAmount":"995000"}}),
    )
}
fn sim() -> Value {
    ok(json!({"status":"SUCCESS","balanceChanges":[],"allowanceChanges":[]}))
}
fn steps() -> Vec<(&'static str, Value)> {
    vec![
        ("token-balances-by-address", balance()),
        ("quote", ok(json!([quote()]))),
        ("swap", build(&quote())),
        ("simulate", sim()),
    ]
}
#[tokio::test]
async fn binds_prepared_transaction_without_interactive_confirmation() {
    let script = Script::new(steps());
    let prepared = prepare_with_transport(request(), policy(), script.clone()).await;
    assert!(prepared.ready(), "{:?}", prepared.report());
    assert_eq!(prepared.report().state, "swap_ready");
    assert_eq!(prepared.report().quote.as_ref().unwrap().notional_usd, "5");
    assert_eq!(
        prepared.report().quote.as_ref().unwrap().price_impact_bps,
        2
    );
    let id = prepared.report().confirmation_id.clone().unwrap();
    let signer = prepared.into_signer_request().unwrap();
    assert_eq!(signer.confirmation_id, id);
    assert_eq!(signer.transaction["data"], "0x12345678");
    assert_eq!(signer.chain_id, 56);
    script.done();
}
#[tokio::test]
async fn notional_uses_exact_quote_decimals_and_cannot_round_below_limit() {
    let mut q = quote();
    q["fromToken"]["tokenUnitPrice"] = json!("1.00000000000000000001");
    let mut p = policy();
    p.risk.max_notional_usd = 5.0;
    let script = Script::new(vec![
        ("token-balances-by-address", balance()),
        ("quote", ok(json!([q]))),
    ]);
    let prepared = prepare_with_transport(request(), p, script.clone()).await;
    assert!(!prepared.ready());
    assert!(prepared.report().blockers[0].contains("notional_limit"));
    script.done();
}
#[tokio::test]
async fn malformed_policy_and_request_fail_without_network() {
    let mut cases = vec![];
    let mut p = policy();
    p.risk.require_successful_simulation = false;
    cases.push((request(), p));
    let mut p = policy();
    p.risk.max_notional_usd = f64::NAN;
    cases.push((request(), p));
    let mut p = policy();
    p.max_age_seconds = 31;
    cases.push((request(), p));
    for age in [0, 301] {
        let mut p = policy();
        p.approval_max_age_seconds = age;
        cases.push((request(), p));
    }
    let mut r = request();
    r.wallet_address = "0x0".into();
    cases.push((r, policy()));
    let mut r = request();
    r.amount = "1e18".into();
    cases.push((r, policy()));
    for (r, p) in cases {
        assert!(!prepare_with_transport(r, p, Script::new(vec![]))
            .await
            .ready());
    }
}
#[tokio::test]
async fn api_business_error_is_not_http_success_and_body_is_not_in_report() {
    let script = Script::new(vec![
        ("token-balances-by-address", balance()),
        (
            "quote",
            json!({"code":40375,"msg":"DO-NOT-LOG-RAW-RESPONSE","data":null}),
        ),
    ]);
    let prepared = prepare_with_transport(request(), policy(), script.clone()).await;
    assert!(!prepared.ready());
    assert_eq!(prepared.report().stages[1].business_code, Some(40375));
    assert!(!serde_json::to_string(prepared.report())
        .unwrap()
        .contains("DO-NOT-LOG"));
    script.done();
}
#[tokio::test]
async fn rejects_quote_identity_mismatches_before_build() {
    let mut mutations = vec![];
    let mut q = quote();
    q["binanceChainId"] = json!("1");
    mutations.push(q);
    let mut q = quote();
    q["fromTokenAmount"] = json!("1");
    mutations.push(q);
    let mut q = quote();
    q["fromToken"]["tokenContractAddress"] = json!(BUY);
    mutations.push(q);
    let mut q = quote();
    q["executionMode"] = json!("UNKNOWN");
    mutations.push(q);
    let mut q = quote();
    q["priceImpactPercent"] = json!("-1.01");
    mutations.push(q);
    for q in mutations {
        let script = Script::new(vec![
            ("token-balances-by-address", balance()),
            ("quote", ok(json!([q]))),
        ]);
        assert!(!prepare_with_transport(request(), policy(), script.clone())
            .await
            .ready());
        script.done();
    }
}
#[tokio::test]
async fn rejects_build_tampering_and_untrusted_router_before_simulation() {
    let mut mutations = vec![];
    let mut b = build(&quote());
    b["data"]["tx"]["from"] = json!(BUY);
    mutations.push(b);
    let mut b = build(&quote());
    b["data"]["tx"]["to"] = json!("0x0");
    mutations.push(b);
    let mut b = build(&quote());
    b["data"]["tx"]["value"] = json!("1");
    mutations.push(b);
    let mut b = build(&quote());
    b["data"]["tx"]["minReceiveAmount"] = json!("990000");
    mutations.push(b);
    let mut b = build(&quote());
    b["data"]["routerResult"]["vendorName"] = json!("Other");
    mutations.push(b);
    for b in mutations {
        let script = Script::new(vec![
            ("token-balances-by-address", balance()),
            ("quote", ok(json!([quote()]))),
            ("swap", b),
        ]);
        assert!(!prepare_with_transport(request(), policy(), script.clone())
            .await
            .ready());
        script.done();
    }
}
#[tokio::test]
async fn failed_simulation_never_produces_signer_capability() {
    let mut s = steps();
    s[3].1 = ok(json!({"status":"FAILED"}));
    let prepared = prepare_with_transport(request(), policy(), Script::new(s)).await;
    assert!(!prepared.ready());
    assert_eq!(
        prepared.report().simulation_status.as_deref(),
        Some("FAILED")
    );
    assert!(prepared.into_signer_request().is_err());
}
#[tokio::test]
async fn rfq_is_not_misrepresented_as_simulated_or_executable() {
    let mut q = quote();
    q["executionMode"] = json!("RFQ");
    q["vendorName"] = json!("PcsXRfq");
    let b = ok(
        json!({"executionMode":"RFQ","routerResult":q,"rfq":{"vendor":"PcsXRfq","orderId":"order-1","typedDataToSign":{"domain":{"chainId":56},"message":{"secret":"NOT-IN-REPORT"}}}}),
    );
    let script = Script::new(vec![
        ("token-balances-by-address", balance()),
        ("quote", ok(json!([q]))),
        ("swap", b),
    ]);
    let prepared = prepare_with_transport(request(), policy(), script.clone()).await;
    assert_eq!(prepared.report().state, "rfq_requires_adapter");
    assert!(!prepared.ready());
    assert!(prepared.report().simulation_status.is_none());
    assert!(prepared.report().rfq_payload_sha256.is_some());
    assert!(!serde_json::to_string(prepared.report())
        .unwrap()
        .contains("NOT-IN-REPORT"));
    script.done();
}
fn approval_steps(calldata: String) -> Vec<(&'static str, Value)> {
    let mut q = quote();
    q["approveTarget"] = json!(SPENDER);
    vec![
        ("token-balances-by-address", balance()),
        ("quote", ok(json!([q]))),
        ("swap", build(&q)),
        (
            "approve-transaction",
            ok(json!([{"data":calldata,"dexContractAddress":SPENDER}])),
        ),
    ]
}
fn approve_data() -> String {
    format!(
        "0x095ea7b3{:0>64}{:064x}",
        &SPENDER[2..],
        5_000_000_000_000_000_000u128
    )
}
#[tokio::test]
async fn approval_uses_quoted_vendor_and_exact_amount_then_stops() {
    let mut steps = approval_steps(approve_data());
    steps.push(("simulate", sim()));
    let script = Script::new(steps);
    let prepared = prepare_with_transport(request(), policy(), script.clone()).await;
    assert!(prepared.ready(), "{:?}", prepared.report());
    assert_eq!(prepared.report().state, "approval_ready");
    let signed = prepared.into_signer_request().unwrap();
    assert_eq!(signed.kind, "approval");
    assert!(signed.expires_in_ms > 30_000 && signed.expires_in_ms <= 300_000);
    assert_eq!(signed.transaction["to"], SELL);
    let requests = script.requests.lock().unwrap();
    let url = url::Url::parse(&requests[3].url).unwrap();
    assert!(url
        .query_pairs()
        .any(|(k, v)| k == "vendor" && v == "Pancake"));
    let RequestBody::Json(body) = &requests[4].body else {
        panic!()
    };
    assert_eq!(
        serde_json::from_str::<Value>(body).unwrap()["evmTx"]["data"],
        approve_data()
    );
    script.done();
}
#[tokio::test]
async fn unlimited_approval_is_rejected_before_simulation() {
    let data = format!("0x095ea7b3{:0>64}{}", &SPENDER[2..], "f".repeat(64));
    let script = Script::new(approval_steps(data));
    let prepared = prepare_with_transport(request(), policy(), script.clone()).await;
    assert!(!prepared.ready());
    assert!(prepared.report().blockers[0].contains("exactly"));
    script.done();
}
#[tokio::test]
async fn missing_balances_are_unknown_not_zero() {
    let mut b = balance();
    b["data"][0]["tokenAssets"].as_array_mut().unwrap().pop();
    let mut responses = steps();
    responses[0].1 = b;
    let script = Script::new(responses);
    let prepared = prepare_with_transport(request(), policy(), script.clone()).await;
    assert!(!prepared.ready());
    assert!(prepared.report().buy_balance.is_none());
    assert!(prepared.report().blockers[0].contains("unknown is not zero"));
    script.done();
}
#[tokio::test]
async fn unfunded_wallet_can_diagnose_but_cannot_authorize() {
    let mut s = steps();
    s[0].1["data"][0]["tokenAssets"][0]["rawBalance"] = json!("0");
    let prepared = prepare_with_transport(request(), policy(), Script::new(s)).await;
    assert!(!prepared.ready());
    assert!(prepared
        .report()
        .blockers
        .iter()
        .any(|s| s.contains("insufficient")));
}
#[tokio::test]
async fn prepared_action_expires_before_signer_handoff() {
    let mut p = policy();
    p.max_age_seconds = 1;
    let prepared = prepare_with_transport(request(), p, Script::new(steps())).await;
    tokio::time::sleep(std::time::Duration::from_millis(1050)).await;
    assert!(prepared.into_signer_request().is_err());
}

const HASH: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BLOCK: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
type RpcStep = (&'static str, u64, Value);
#[derive(Clone)]
struct RpcScript(Arc<Mutex<VecDeque<RpcStep>>>);
impl HttpTransport for RpcScript {
    async fn execute(
        &self,
        request: Request,
        options: RequestOptions,
    ) -> Result<HttpResponse, HttpError> {
        assert_eq!(options.redirect_policy, RedirectPolicy::DoNotFollow);
        assert_eq!(
            request.headers,
            vec![("Content-Type".into(), "application/json".into())]
        );
        let RequestBody::Json(body) = request.body else {
            panic!()
        };
        let body: Value = serde_json::from_str(&body).unwrap();
        let (method, id, result) = self.0.lock().unwrap().pop_front().expect("unexpected RPC");
        assert_eq!(body["method"], method);
        assert_eq!(body["id"], id);
        Ok(HttpResponse::new(
            200,
            vec![],
            json!({"jsonrpc":"2.0","id":id,"result":result}).to_string(),
        ))
    }
}
fn rpc_steps() -> Vec<(&'static str, u64, Value)> {
    vec![
        ("eth_chainId", 1, json!("0x38")),
        (
            "eth_getTransactionByHash",
            1,
            json!({"hash":HASH,"chainId":"0x38","from":WALLET,"to":ROUTER,"input":"0x12345678","value":"0x0"}),
        ),
        ("eth_chainId", 1, json!("0x38")),
        (
            "eth_getTransactionReceipt",
            2,
            json!({"transactionHash":HASH,"blockHash":BLOCK,"blockNumber":"0x10","status":"0x1","gasUsed":"0x5208"}),
        ),
        ("eth_blockNumber", 3, json!("0x12")),
        (
            "eth_getBlockByNumber",
            4,
            json!({"number":"0x10","hash":BLOCK}),
        ),
    ]
}
#[tokio::test]
async fn settlement_checks_exact_transaction_receipt_and_balance_observations() {
    use flow_bnb::trade::{verify_settlement, SignerResponse};
    let p = prepare_with_transport(request(), policy(), Script::new(steps())).await;
    let report = p.report().clone();
    let id = report.confirmation_id.clone().unwrap();
    let request = p.into_signer_request().unwrap();
    let response = SignerResponse {
        confirmation_id: id,
        tx_hash: HASH.into(),
    };
    let mut after = balance();
    after["data"][0]["tokenAssets"][0]["rawBalance"] = json!("5000000000000000000");
    after["data"][0]["tokenAssets"][1]["rawBalance"] = json!("1000000");
    let api = Script::new(vec![("token-balances-by-address", after)]);
    let rpc = RpcScript(Arc::new(Mutex::new(rpc_steps().into())));
    let settled = verify_settlement(
        &report,
        &request,
        &response,
        "https://rpc.example/?key=hidden",
        api.clone(),
        rpc.clone(),
    )
    .await;
    assert!(settled.errors.is_empty(), "{:?}", settled.errors);
    assert_eq!(settled.state, "confirmed_balances_observed");
    assert_eq!(settled.sell_delta.as_deref(), Some("-5000000000000000000"));
    assert_eq!(settled.buy_delta.as_deref(), Some("1000000"));
    assert!(!serde_json::to_string(&settled)
        .unwrap()
        .contains("key=hidden"));
    api.done();
    assert!(rpc.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn substituted_transaction_hash_does_not_pass_settlement() {
    use flow_bnb::trade::{verify_settlement, SignerResponse};
    let p = prepare_with_transport(request(), policy(), Script::new(steps())).await;
    let report = p.report().clone();
    let id = report.confirmation_id.clone().unwrap();
    let request = p.into_signer_request().unwrap();
    let response = SignerResponse {
        confirmation_id: id,
        tx_hash: HASH.into(),
    };
    let mut steps = rpc_steps();
    steps.truncate(2);
    steps[1].2["input"] = json!("0xdeadbeef");
    let rpc = RpcScript(Arc::new(Mutex::new(steps.into())));
    let settled = verify_settlement(
        &report,
        &request,
        &response,
        "https://rpc.example",
        Script::new(vec![]),
        rpc.clone(),
    )
    .await;
    assert_eq!(settled.state, "unverified");
    assert!(settled.errors[0].contains("calldata mismatch"));
    assert!(settled.receipt.is_none());
    assert!(rpc.0.lock().unwrap().is_empty());
}
#[cfg(unix)]
#[tokio::test]
async fn external_signer_protocol_validates_binding_and_bounds_execution() {
    use flow_bnb::trade::{invoke_signer, SignerRequest};
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wallet");
    let request = SignerRequest {
        protocol: "flow-bnb-signer-v1".into(),
        confirmation_id: "reviewed-action".into(),
        chain_id: 56,
        kind: "swap".into(),
        transaction: json!({}),
        expires_in_ms: 1000,
    };
    for id in ["reviewed-action", "wrong-action"] {
        std::fs::write(&path,format!("#!/bin/sh\nread request\nprintf '%s\\n' '{{\"confirmation_id\":\"{id}\",\"tx_hash\":\"{HASH}\"}}'\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = invoke_signer(&path, &request).await;
        assert_eq!(result.is_ok(), id == "reviewed-action");
    }
    std::fs::write(&path, "#!/bin/sh\nexec /bin/sleep 5\n").unwrap();
    let mut request = request;
    request.expires_in_ms = 20;
    assert!(invoke_signer(&path, &request)
        .await
        .err()
        .unwrap()
        .to_string()
        .contains("outcome unknown"));
}

#[tokio::test]
async fn untrusted_router_allows_read_only_simulation_but_no_handoff() {
    let mut policy = policy();
    policy.allowed_routers.clear();
    let script = Script::new(steps());
    let prepared = prepare_with_transport(request(), policy, script.clone()).await;
    assert!(!prepared.ready());
    assert_eq!(
        prepared.report().simulation_status.as_deref(),
        Some("SUCCESS")
    );
    assert_eq!(prepared.report().state, "blocked");
    assert!(prepared
        .report()
        .blockers
        .iter()
        .any(|b| b.contains("router")));
    script.done();
}

#[tokio::test]
async fn missing_balance_is_recovered_from_bsc_balance_of_with_source() {
    use flow_bnb::trade::BalanceFallback;
    let mut s = steps();
    s[0].1["data"][0]["tokenAssets"]
        .as_array_mut()
        .unwrap()
        .pop();
    let rpc = RpcScript(Arc::new(Mutex::new(
        vec![
            ("eth_chainId", 1, json!("0x38")),
            ("eth_call", 1, json!(format!("0x{}", "0".repeat(64)))),
        ]
        .into(),
    )));
    let api = Script::new(s);
    let transport = BalanceFallback {
        api: api.clone(),
        rpc: rpc.clone(),
        rpc_url: Some("https://rpc.example".into()),
    };
    let prepared = prepare_with_transport(request(), policy(), transport).await;
    assert!(prepared.ready(), "{:?}", prepared.report());
    assert_eq!(prepared.report().buy_balance.as_deref(), Some("0"));
    assert_eq!(prepared.report().balance_sources["buy"], "rpc_balanceOf");
    assert_eq!(prepared.report().balance_sources["sell"], "wallet_api");
    api.done();
    assert!(rpc.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn balance_fallback_wrong_chain_or_invalid_abi_cannot_become_zero() {
    use flow_bnb::trade::BalanceFallback;
    for wrong_chain in [true, false] {
        let mut b = balance();
        b["data"][0]["tokenAssets"].as_array_mut().unwrap().pop();
        let rpc = if wrong_chain {
            vec![("eth_chainId", 1, json!("0x1"))]
        } else {
            vec![
                ("eth_chainId", 1, json!("0x38")),
                ("eth_call", 1, json!("0x")),
            ]
        };
        let rpc = RpcScript(Arc::new(Mutex::new(rpc.into())));
        let api = Script::new(vec![("token-balances-by-address", b)]);
        let transport = BalanceFallback {
            api: api.clone(),
            rpc: rpc.clone(),
            rpc_url: Some("https://rpc.example/?key=do-not-log".into()),
        };
        let prepared = prepare_with_transport(request(), policy(), transport).await;
        assert!(!prepared.ready());
        assert!(prepared.report().buy_balance.is_none());
        assert!(!serde_json::to_string(prepared.report())
            .unwrap()
            .contains("do-not-log"));
        api.done();
        assert!(rpc.0.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn allowance_preparation_has_its_own_deadline() {
    let mut approval = approval_steps(approve_data());
    approval.push(("simulate", sim()));
    let mut p = policy();
    p.max_age_seconds = 1;
    let long = prepare_with_transport(request(), p.clone(), Script::new(approval.clone())).await;
    let swap = prepare_with_transport(request(), p.clone(), Script::new(steps())).await;
    p.approval_max_age_seconds = 1;
    let short = prepare_with_transport(request(), p, Script::new(approval)).await;
    assert_eq!(long.report().max_age_seconds, 300);
    assert_eq!(swap.report().max_age_seconds, 1);
    tokio::time::sleep(std::time::Duration::from_millis(1050)).await;
    assert!(long.into_signer_request().unwrap().expires_in_ms > 30_000);
    assert!(short.into_signer_request().is_err());
    assert!(swap.into_signer_request().is_err());
}
