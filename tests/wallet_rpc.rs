use flow_bnb::{
    trade::SignerRequest,
    wallet::{send_with_transport, DevWalletConfig},
};
use postman_http::{
    request::{RedirectPolicy, Request, RequestBody, RequestOptions},
    HttpError, HttpResponse, HttpTransport,
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
const WALLET: &str = flow_bnb::demo::WALLET;
const ROUTER: &str = flow_bnb::demo::ROUTER;
#[derive(Clone)]
struct Rpc {
    steps: Arc<Mutex<VecDeque<(&'static str, Value)>>>,
    sent: Arc<Mutex<Vec<Value>>>,
}
impl Rpc {
    fn new(steps: Vec<(&'static str, Value)>) -> Self {
        Self {
            steps: Arc::new(Mutex::new(steps.into())),
            sent: Default::default(),
        }
    }
}
impl HttpTransport for Rpc {
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
        let (method, result) = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected RPC");
        assert_eq!(body["method"], method);
        self.sent.lock().unwrap().push(body);
        Ok(HttpResponse::new(
            200,
            vec![],
            json!({"jsonrpc":"2.0","id":1,"result":result}).to_string(),
        ))
    }
}
fn config() -> DevWalletConfig {
    DevWalletConfig {
        development_only: true,
        rpc_url: "http://127.0.0.1:8545".into(),
        account: WALLET.into(),
        max_gas: 200000,
        max_gas_price_wei: 2_000_000_000,
        max_fee_wei: "400000000000000".into(),
    }
}
fn request() -> SignerRequest {
    SignerRequest {
        protocol: "flow-bnb-signer-v1".into(),
        confirmation_id: "a".repeat(64),
        chain_id: 56,
        kind: "swap".into(),
        transaction: json!({"from":WALLET,"to":ROUTER,"value":"0","data":"0x12345678"}),
        expires_in_ms: 1000,
    }
}
fn steps() -> Vec<(&'static str, Value)> {
    vec![
        ("web3_clientVersion", json!("anvil/v1")),
        ("eth_chainId", json!("0x38")),
        ("eth_accounts", json!([WALLET])),
        ("eth_estimateGas", json!("0x186a0")),
        ("eth_gasPrice", json!("0x3b9aca00")),
        (
            "eth_sendTransaction",
            json!(format!("0x{}", "b".repeat(64))),
        ),
    ]
}
#[tokio::test]
async fn applies_gas_caps_and_preserves_reviewed_payload() {
    let rpc = Rpc::new(steps());
    let response = send_with_transport(&config(), &request(), rpc.clone())
        .await
        .unwrap();
    assert_eq!(response.confirmation_id, "a".repeat(64));
    let sent = rpc.sent.lock().unwrap();
    let tx = &sent.last().unwrap()["params"][0];
    assert_eq!(tx["from"], WALLET);
    assert_eq!(tx["to"], ROUTER);
    assert_eq!(tx["data"], "0x12345678");
    assert_eq!(tx["value"], "0x0");
    assert_eq!(tx["gas"], "0x1d4c0");
    assert_eq!(tx["gasPrice"], "0x3b9aca00");
    assert!(tx.get("nonce").is_none());
    assert!(rpc.steps.lock().unwrap().is_empty());
}
#[tokio::test]
async fn public_endpoints_and_wrong_payload_fail_before_io() {
    for url in [
        "https://bsc-dataseed.bnbchain.org",
        "http://example.com",
        "http://127.0.0.1.evil.test",
        "http://user:secret@127.0.0.1",
        "http://localhost:8545",
    ] {
        let mut c = config();
        c.rpc_url = url.into();
        assert!(send_with_transport(&c, &request(), Rpc::new(vec![]))
            .await
            .is_err());
    }
    let mut requests = vec![];
    let mut r = request();
    r.transaction["from"] = json!(ROUTER);
    requests.push(r);
    let mut r = request();
    r.transaction["value"] = json!("1");
    requests.push(r);
    let mut r = request();
    r.transaction["nonce"] = json!("0x0");
    requests.push(r);
    let mut r = request();
    r.expires_in_ms = 0;
    requests.push(r);
    for r in requests {
        assert!(send_with_transport(&config(), &r, Rpc::new(vec![]))
            .await
            .is_err());
    }
}
#[tokio::test]
async fn wrong_chain_account_and_limits_never_send() {
    for case in 0..6 {
        let mut s = steps();
        let mut c = config();
        let end = match case {
            0 => {
                s[0].1 = json!("Geth/mainnet");
                1
            }
            1 => {
                s[1].1 = json!("0x1");
                2
            }
            2 => {
                s[2].1 = json!([ROUTER]);
                3
            }
            3 => {
                c.max_gas = 100000;
                4
            }
            4 => {
                c.max_gas_price_wei = 1;
                5
            }
            _ => {
                c.max_fee_wei = "1".into();
                5
            }
        };
        s.truncate(end);
        let rpc = Rpc::new(s);
        assert!(send_with_transport(&c, &request(), rpc.clone())
            .await
            .is_err());
        assert!(rpc.steps.lock().unwrap().is_empty());
        assert!(!rpc
            .sent
            .lock()
            .unwrap()
            .iter()
            .any(|b| b["method"] == "eth_sendTransaction"));
    }
}
#[tokio::test]
async fn zero_or_invalid_transaction_hash_is_not_success() {
    for hash in [format!("0x{}", "0".repeat(64)), "0x123".into()] {
        let mut s = steps();
        s[5].1 = json!(hash);
        let rpc = Rpc::new(s);
        assert!(send_with_transport(&config(), &request(), rpc.clone())
            .await
            .is_err());
        assert_eq!(
            rpc.sent
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r["method"] == "eth_sendTransaction")
                .count(),
            1
        );
    }
}
