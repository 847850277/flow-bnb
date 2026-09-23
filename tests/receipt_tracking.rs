use flow_bnb::receipt::{watch_with_transport, ReceiptState, WatchOptions, WatchOutcome};
use postman_http::{
    request::{RedirectPolicy, Request, RequestBody, RequestOptions},
    HttpError, HttpResponse, HttpTransport,
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

const TX: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BLOCK: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

type ExpectedRpc = (&'static str, Result<HttpResponse, HttpError>);

#[derive(Clone)]
struct Script(Arc<Mutex<VecDeque<ExpectedRpc>>>);
impl HttpTransport for Script {
    async fn execute(
        &self,
        request: Request,
        options: RequestOptions,
    ) -> Result<HttpResponse, HttpError> {
        assert_eq!(options.redirect_policy, RedirectPolicy::DoNotFollow);
        assert_eq!(
            request.headers,
            [("Content-Type".into(), "application/json".into())]
        );
        let RequestBody::Json(body) = request.body else {
            panic!("expected JSON")
        };
        let body: Value = serde_json::from_str(&body).unwrap();
        let (method, response) = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected extra RPC request");
        assert_eq!(body["method"], method);
        if method == "eth_getTransactionReceipt" {
            assert_eq!(body["params"], json!([TX]));
        }
        if method == "eth_getBlockByNumber" {
            assert_eq!(body["params"], json!(["0x10", false]));
        }
        response
    }
}
fn options() -> WatchOptions {
    serde_json::from_value(
        json!({"rpc_url":"https://rpc.example/?key=must-not-leak", "tx_hash":TX,
        "interval_ms":1, "timeout_ms":2000, "max_iterations":5, "confirmations":2}),
    )
    .unwrap()
}
fn rpc(id: u64, value: Value) -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse::new(
        200,
        vec![],
        json!({"jsonrpc":"2.0","id":id,"result":value}).to_string(),
    ))
}
fn receipt(status: &str) -> Value {
    json!({"transactionHash":TX, "blockHash":BLOCK, "blockNumber":"0x10", "status":status, "gasUsed":"0x5208"})
}
fn canonical() -> Value {
    json!({"number":"0x10", "hash":BLOCK})
}
async fn run(options: WatchOptions, steps: Vec<ExpectedRpc>) -> flow_bnb::receipt::WatchReport {
    let script = Script(Arc::new(Mutex::new(steps.into())));
    let report = watch_with_transport(options, script.clone()).await.unwrap();
    assert!(script.0.lock().unwrap().is_empty());
    assert!(!serde_json::to_string(&report)
        .unwrap()
        .contains("must-not-leak"));
    report
}

#[tokio::test]
async fn pending_then_included_then_confirmed_uses_flow_loop() {
    let report = run(
        options(),
        vec![
            ("eth_chainId", rpc(1, json!("0x38"))),
            ("eth_getTransactionReceipt", rpc(2, Value::Null)),
            ("eth_getTransactionReceipt", rpc(2, receipt("0x1"))),
            ("eth_blockNumber", rpc(3, json!("0x10"))),
            ("eth_getBlockByNumber", rpc(4, canonical())),
            ("eth_getTransactionReceipt", rpc(2, receipt("0x1"))),
            ("eth_blockNumber", rpc(3, json!("0x11"))),
            ("eth_getBlockByNumber", rpc(4, canonical())),
        ],
    )
    .await;
    assert!(report.success);
    assert_eq!(report.outcome, WatchOutcome::Confirmed);
    assert_eq!(report.iterations, 3);
    assert_eq!(report.observation.confirmations, 2);
    assert_eq!(report.observation.block_number, Some(16));
}

#[tokio::test]
async fn reverted_transaction_fails_even_with_successful_http_and_rpc() {
    let report = run(
        options(),
        vec![
            ("eth_chainId", rpc(1, json!("0x38"))),
            ("eth_getTransactionReceipt", rpc(2, receipt("0x0"))),
            ("eth_blockNumber", rpc(3, json!("0x11"))),
            ("eth_getBlockByNumber", rpc(4, canonical())),
        ],
    )
    .await;
    assert!(!report.success);
    assert_eq!(report.outcome, WatchOutcome::Reverted);
    assert_eq!(report.observation.state, ReceiptState::Reverted);
}

#[tokio::test]
async fn reorg_and_disappearing_receipt_do_not_retain_confirmation_progress() {
    let mut config = options();
    config.max_iterations = 4;
    let report = run(
        config,
        vec![
            ("eth_chainId", rpc(1, json!("0x38"))),
            ("eth_getTransactionReceipt", rpc(2, receipt("0x1"))),
            ("eth_blockNumber", rpc(3, json!("0x10"))),
            ("eth_getBlockByNumber", rpc(4, canonical())),
            ("eth_getTransactionReceipt", rpc(2, Value::Null)),
            ("eth_getTransactionReceipt", rpc(2, receipt("0x1"))),
            ("eth_blockNumber", rpc(3, json!("0x11"))),
            (
                "eth_getBlockByNumber",
                rpc(4, json!({"number":"0x10", "hash":TX})),
            ),
            ("eth_getTransactionReceipt", rpc(2, Value::Null)),
        ],
    )
    .await;
    assert!(!report.success);
    assert_eq!(report.outcome, WatchOutcome::MaxIterations);
    assert_eq!(report.observation.state, ReceiptState::Pending);
    assert_eq!(report.observation.confirmations, 0);
    assert_eq!(report.observation.block_hash, None);
}

#[tokio::test]
async fn chain_mismatch_fails_before_receipt_lookup() {
    let report = run(options(), vec![("eth_chainId", rpc(1, json!("0x1")))]).await;
    assert!(!report.success);
    assert_eq!(report.outcome, WatchOutcome::RpcError);
    assert_eq!(report.iterations, 0);
    assert!(report.error.unwrap().contains("chain ID"));
}

#[tokio::test]
async fn pending_receipt_stops_at_timeout_or_iteration_limit() {
    for timeout in [true, false] {
        let mut config = options();
        if timeout {
            config.timeout_ms = 100;
            config.interval_ms = 1000;
        } else {
            config.max_iterations = 1;
        }
        let report = run(
            config,
            vec![
                ("eth_chainId", rpc(1, json!("0x38"))),
                ("eth_getTransactionReceipt", rpc(2, Value::Null)),
            ],
        )
        .await;
        assert!(!report.success);
        assert_eq!(
            report.outcome,
            if timeout {
                WatchOutcome::Timeout
            } else {
                WatchOutcome::MaxIterations
            }
        );
    }
}

#[tokio::test]
async fn malformed_and_error_responses_never_become_pending_or_success() {
    let cases = [
        HttpResponse::new(
            200,
            vec![],
            json!({"jsonrpc":"2.0","id":2,"error":{"code":-32603,"message":"must-not-leak"}})
                .to_string(),
        ),
        HttpResponse::new(200, vec![], json!({"jsonrpc":"2.0","id":2}).to_string()),
        HttpResponse::new(
            200,
            vec![],
            json!({"jsonrpc":"2.0","id":999,"result":null}).to_string(),
        ),
        HttpResponse::new(200, vec![], "not json must-not-leak".into()),
        HttpResponse::new(503, vec![], "must-not-leak".into()),
        rpc(2, json!({"status":"0x1"})).unwrap(),
        rpc(2, {
            let mut r = receipt("0x1");
            r["transactionHash"] = json!(BLOCK);
            r
        })
        .unwrap(),
        rpc(2, receipt("0x2")).unwrap(),
    ];
    for response in cases {
        let report = run(
            options(),
            vec![
                ("eth_chainId", rpc(1, json!("0x38"))),
                ("eth_getTransactionReceipt", Ok(response)),
            ],
        )
        .await;
        assert!(!report.success);
        assert_eq!(report.outcome, WatchOutcome::RpcError);
        assert!(report.error.is_some());
    }
}

#[tokio::test]
async fn cancellation_and_network_errors_keep_distinct_outcomes_and_redact_provider_details() {
    for (error, outcome) in [
        (HttpError::Cancelled, WatchOutcome::Cancelled),
        (
            HttpError::network("https://rpc.example/must-not-leak"),
            WatchOutcome::RpcError,
        ),
    ] {
        let report = run(
            options(),
            vec![
                ("eth_chainId", rpc(1, json!("0x38"))),
                ("eth_getTransactionReceipt", Err(error)),
            ],
        )
        .await;
        assert!(!report.success);
        assert_eq!(report.outcome, outcome);
    }
}

#[tokio::test]
async fn invalid_arguments_fail_before_network_io() {
    for field in [
        "rpc_url",
        "tx_hash",
        "confirmations",
        "timeout_ms",
        "interval_ms",
        "max_iterations",
        "request_timeout_ms",
    ] {
        let mut config = options();
        match field {
            "rpc_url" => config.rpc_url = "file:///must-not-leak".into(),
            "tx_hash" => config.tx_hash = "0x123".into(),
            "confirmations" => config.confirmations = 0,
            "timeout_ms" => config.timeout_ms = 0,
            "interval_ms" => config.interval_ms = 0,
            "max_iterations" => config.max_iterations = 0,
            _ => config.request_timeout_ms = 0,
        }
        let err = watch_with_transport(config, Script(Arc::default()))
            .await
            .unwrap_err();
        assert!(!err.to_string().contains("must-not-leak"));
    }
}

#[tokio::test]
async fn loop_deadline_interrupts_an_in_flight_rpc_request() {
    struct StalledReceipt;
    impl HttpTransport for StalledReceipt {
        async fn execute(
            &self,
            request: Request,
            _: RequestOptions,
        ) -> Result<HttpResponse, HttpError> {
            let RequestBody::Json(body) = request.body else {
                panic!("expected JSON")
            };
            let body: Value = serde_json::from_str(&body).unwrap();
            if body["method"] == "eth_chainId" {
                rpc(1, json!("0x38"))
            } else {
                futures::future::pending().await
            }
        }
    }
    let mut config = options();
    config.timeout_ms = 100;
    let report = watch_with_transport(config, StalledReceipt).await.unwrap();
    assert!(!report.success);
    assert_eq!(report.outcome, WatchOutcome::Timeout);
    assert_eq!(report.iterations, 1);
    assert_eq!(report.observation.state, ReceiptState::Pending);
}
