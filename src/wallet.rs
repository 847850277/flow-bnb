//! Development wallet adapter for a loopback Anvil/Hardhat node. No private keys
//! cross this process boundary. This adapter is not a mainnet wallet integration.
use crate::trade::{SignerRequest, SignerResponse};
use anyhow::{anyhow, ensure, Context, Result};
use num_bigint::BigUint;
use postman_http::{
    request::{HttpMethod, RedirectPolicy, Request, RequestBody, RequestOptions},
    HttpTransport,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevWalletConfig {
    pub development_only: bool,
    pub rpc_url: String,
    pub account: String,
    pub max_gas: u64,
    pub max_gas_price_wei: u64,
    pub max_fee_wei: String,
}
impl DevWalletConfig {
    pub fn validate(&self) -> Result<()> {
        let url = url::Url::parse(&self.rpc_url).map_err(|_| anyhow!("invalid wallet RPC URL"))?;
        ensure!(
            self.development_only
                && url.scheme() == "http"
                && matches!(url.host(),Some(url::Host::Ipv4(ip)) if ip.is_loopback())
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none(),
            "development wallet requires explicit development_only and numeric IPv4 loopback HTTP"
        );
        ensure!(
            address(&self.account) && self.max_gas > 0 && self.max_gas_price_wei > 0,
            "invalid wallet account or gas limits"
        );
        ensure!(
            decimal(&self.max_fee_wei)? > BigUint::from(0u8),
            "max_fee_wei must be positive"
        );
        Ok(())
    }
}
pub async fn send_with_transport<T: HttpTransport>(
    config: &DevWalletConfig,
    request: &SignerRequest,
    rpc: T,
) -> Result<SignerResponse> {
    config.validate()?;
    ensure!(
        request.protocol == "flow-bnb-signer-v1"
            && request.chain_id == 56
            && matches!(request.kind.as_str(), "swap" | "approval"),
        "unsupported signer request"
    );
    ensure!(
        request.confirmation_id.len() == 64
            && request
                .confirmation_id
                .bytes()
                .all(|b| b.is_ascii_hexdigit()),
        "invalid confirmation ID"
    );
    ensure!(
        (1..=30_000).contains(&request.expires_in_ms),
        "invalid or expired signer deadline"
    );
    let tx = request
        .transaction
        .as_object()
        .context("transaction must be an object")?;
    ensure!(
        tx.len() == 4
            && ["from", "to", "value", "data"]
                .iter()
                .all(|k| tx.contains_key(*k)),
        "unexpected transaction fields"
    );
    ensure!(
        tx["from"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case(&config.account)),
        "wallet account mismatch"
    );
    ensure!(
        tx["to"].as_str().is_some_and(address) && tx["value"] == "0",
        "only zero-native-value ERC-20 transactions are supported"
    );
    let calldata = tx["data"].as_str().context("calldata missing")?;
    ensure!(
        calldata.len() >= 10
            && calldata.len() <= 131074
            && calldata.len() % 2 == 0
            && calldata.starts_with("0x")
            && calldata[2..].bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid calldata"
    );
    let started = Instant::now();
    let run = async {
        let client = call(&rpc, &config.rpc_url, "web3_clientVersion", json!([])).await?;
        let version = client
            .as_str()
            .context("client version missing")?
            .to_ascii_lowercase();
        ensure!(
            version.starts_with("anvil/")
                || version.starts_with("hardhatnetwork/")
                || version.starts_with("flow-bnb-mock/"),
            "wallet RPC is not an identified local development node"
        );
        let chain = call(&rpc, &config.rpc_url, "eth_chainId", json!([])).await?;
        ensure!(
            quantity(&chain)? == BigUint::from(56u8),
            "development node chain ID must be 56"
        );
        let accounts = call(&rpc, &config.rpc_url, "eth_accounts", json!([])).await?;
        ensure!(
            accounts.as_array().is_some_and(|a| a.iter().any(|a| a
                .as_str()
                .is_some_and(|a| a.eq_ignore_ascii_case(&config.account)))),
            "configured account unavailable in wallet"
        );
        let mut transaction = request.transaction.clone();
        transaction["value"] = json!("0x0");
        let estimate = quantity(
            &call(
                &rpc,
                &config.rpc_url,
                "eth_estimateGas",
                json!([transaction]),
            )
            .await?,
        )?;
        let gas = (&estimate * BigUint::from(120u8) + BigUint::from(99u8)) / BigUint::from(100u8);
        ensure!(
            estimate > BigUint::from(0u8) && gas <= BigUint::from(config.max_gas),
            "gas limit exceeded"
        );
        let price = quantity(&call(&rpc, &config.rpc_url, "eth_gasPrice", json!([])).await?)?;
        ensure!(
            price > BigUint::from(0u8)
                && price <= BigUint::from(config.max_gas_price_wei)
                && &gas * &price <= decimal(&config.max_fee_wei)?,
            "gas price or total fee limit exceeded"
        );
        transaction["gas"] = json!(format!("0x{}", gas.to_str_radix(16)));
        transaction["gasPrice"] = json!(format!("0x{}", price.to_str_radix(16)));
        // Wallet/node owns nonce assignment. No automatic resend after any failure.
        ensure!(
            started.elapsed() < Duration::from_millis(request.expires_in_ms),
            "signer request expired"
        );
        let hash = call(
            &rpc,
            &config.rpc_url,
            "eth_sendTransaction",
            json!([transaction]),
        )
        .await?;
        let hash = hash.as_str().context("wallet transaction hash missing")?;
        ensure!(
            hash.len() == 66
                && hash.starts_with("0x")
                && hash[2..].bytes().all(|b| b.is_ascii_hexdigit())
                && hash[2..].bytes().any(|b| b != b'0'),
            "invalid or unavailable transaction hash; outcome unknown"
        );
        Ok(SignerResponse {
            confirmation_id: request.confirmation_id.clone(),
            tx_hash: hash.into(),
        })
    };
    tokio::time::timeout(Duration::from_millis(request.expires_in_ms), run)
        .await
        .map_err(|_| {
            anyhow!(
                "wallet timed out; transaction outcome may be unknown, do not retry automatically"
            )
        })?
}
async fn call<T: HttpTransport>(rpc: &T, url: &str, method: &str, params: Value) -> Result<Value> {
    let mut request = Request::new(HttpMethod::POST, url);
    request.headers = vec![("Content-Type".into(), "application/json".into())];
    request.body = RequestBody::Json(
        json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string(),
    );
    let response = rpc
        .execute(
            request,
            RequestOptions {
                timeout_ms: Some(5000),
                redirect_policy: RedirectPolicy::DoNotFollow,
                ..Default::default()
            },
        )
        .await
        .map_err(|_| anyhow!("wallet transport failure; inspect wallet before retrying"))?;
    ensure!(response.status == 200, "wallet HTTP failure");
    let body: Value =
        serde_json::from_str(&response.body).map_err(|_| anyhow!("invalid wallet response"))?;
    ensure!(
        body["jsonrpc"] == "2.0" && body["id"] == 1,
        "wallet RPC envelope mismatch"
    );
    ensure!(
        body.get("error").is_none(),
        "wallet rejected or failed the request; inspect wallet before retrying"
    );
    body.get("result")
        .cloned()
        .context("wallet RPC result missing")
}
fn address(s: &str) -> bool {
    s.len() == 42
        && s.starts_with("0x")
        && s[2..].bytes().all(|b| b.is_ascii_hexdigit())
        && s[2..].bytes().any(|b| b != b'0')
}
fn quantity(v: &Value) -> Result<BigUint> {
    let s = v.as_str().context("invalid RPC quantity")?;
    ensure!(
        s.starts_with("0x")
            && s.len() > 2
            && s.len() <= 66
            && s[2..].bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid RPC quantity"
    );
    BigUint::parse_bytes(&s.as_bytes()[2..], 16).context("invalid RPC quantity")
}
fn decimal(s: &str) -> Result<BigUint> {
    ensure!(
        !s.is_empty() && s.len() <= 78 && s.bytes().all(|b| b.is_ascii_digit()),
        "invalid fee limit"
    );
    BigUint::parse_bytes(s.as_bytes(), 10).context("invalid fee limit")
}
