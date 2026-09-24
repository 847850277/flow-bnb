//! Explicit, deterministic API fixture. No network I/O, keys or real market data.
use postman_http::{
    request::{Request, RequestOptions},
    HttpError, HttpResponse, HttpTransport,
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
pub const WALLET: &str = "0x1111111111111111111111111111111111111111";
pub const SELL: &str = "0x2222222222222222222222222222222222222222";
pub const BUY: &str = "0x3333333333333333333333333333333333333333";
pub const ROUTER: &str = "0x4444444444444444444444444444444444444444";
#[derive(Clone, Default)]
pub struct DemoApi {
    balances: Arc<AtomicUsize>,
}
impl HttpTransport for DemoApi {
    async fn execute(
        &self,
        request: Request,
        _options: RequestOptions,
    ) -> Result<HttpResponse, HttpError> {
        let url = url::Url::parse(&request.url)
            .map_err(|_| HttpError::invalid_request("invalid demo URL"))?;
        let quote = json!({"binanceChainId":"56","quoteId":"simulated-quote","vendorName":"SIMULATED","executionMode":"SWAP","fromTokenAmount":"5000000000000000000","toTokenAmount":"1000000","priceImpactPercent":"0.01","fromToken":{"tokenContractAddress":SELL,"decimal":"18","tokenUnitPrice":"1"},"toToken":{"tokenContractAddress":BUY},"approveTarget":null});
        let data = match url.path() {
            "/build/api/v1/dex/balance/token-balances-by-address" => {
                let after = self.balances.fetch_add(1, Ordering::SeqCst) > 0;
                json!([{"tokenAssets":[{"binanceChainId":"56","tokenContractAddress":SELL,"address":WALLET,"rawBalance":if after {"5000000000000000000"} else {"10000000000000000000"}},{"binanceChainId":"56","tokenContractAddress":BUY,"address":WALLET,"rawBalance":if after {"1000000"} else {"0"}}]}])
            }
            "/build/api/v1/dex/aggregator/quote" => json!([quote]),
            "/build/api/v1/dex/aggregator/swap" => {
                json!({"executionMode":"SWAP","routerResult":quote,"tx":{"from":WALLET,"to":ROUTER,"value":"0","data":"0x12345678","minReceiveAmount":"995000"}})
            }
            "/build/api/v1/dex/pre-transaction/simulate" => {
                json!({"status":"SUCCESS","balanceChanges":[],"allowanceChanges":[]})
            }
            _ => return Err(HttpError::invalid_request("unsupported demo request")),
        };
        Ok(HttpResponse::new(
            200,
            vec![],
            json!({"code":0,"data":data}).to_string(),
        ))
    }
}
