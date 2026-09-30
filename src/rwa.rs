//! Exact comparison of Binance RWA token price with its API reference price.
//! The reference is token-derived per-share data, not an independent equity quote.
use crate::agentic::{self, Config, Intent};
use anyhow::{bail, ensure, Context, Result};
use num_bigint::{BigInt, Sign};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Price {
    binance_chain_id: String,
    token_contract_address: String,
    platform_id: String,
    token_price: String,
    reference_price: String,
    token_price_updated_at: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spread {
    prices: Vec<Price>,
    stock_token: String,
    intent: Intent,
    operator: String,
    threshold_bps: i32,
    max_age_seconds: u32,
}

pub(crate) fn spread(config: &Config, value: Value) -> Result<Value> {
    let request: Spread = serde_json::from_value(value)?;
    config.rules(&request.intent)?;
    evaluate(request, chrono::Utc::now().timestamp_millis())
}

fn evaluate(request: Spread, now_ms: i64) -> Result<Value> {
    ensure!(
        request.prices.len() == 1,
        "RWA spread requires exactly one price row"
    );
    let price = &request.prices[0];
    ensure!(price.binance_chain_id == "56", "RWA price must be on BSC");
    ensure!(
        price
            .token_contract_address
            .eq_ignore_ascii_case(&request.stock_token),
        "RWA price does not match the monitored stock token"
    );
    ensure!(
        request
            .stock_token
            .eq_ignore_ascii_case(&request.intent.from_token)
            || request
                .stock_token
                .eq_ignore_ascii_case(&request.intent.to_token),
        "monitored stock token is outside the trade intent"
    );
    ensure!(
        (1..=86_400).contains(&request.max_age_seconds),
        "max_age_seconds must be 1..=86400"
    );
    ensure!(
        price.token_price_updated_at > 0 && price.token_price_updated_at <= now_ms,
        "RWA token price timestamp is invalid or in the future"
    );
    let age_ms = now_ms - price.token_price_updated_at;
    ensure!(
        age_ms <= i64::from(request.max_age_seconds) * 1000,
        "RWA token price is stale"
    );

    let token =
        BigInt::from(agentic::units(&price.token_price, 36).context("invalid RWA token price")?);
    let reference = BigInt::from(
        agentic::units(&price.reference_price, 36).context("invalid RWA reference price")?,
    );
    ensure!(
        token.sign() == Sign::Plus && reference.sign() == Sign::Plus,
        "RWA token and reference prices must be greater than zero"
    );

    // Compare by cross multiplication. Display rounding never affects a trigger.
    let numerator = (&token - &reference) * 10_000;
    let threshold = &reference * request.threshold_bps;
    let matched = match request.operator.as_str() {
        "eq" => numerator == threshold,
        "gt" => numerator > threshold,
        "gte" => numerator >= threshold,
        "lt" => numerator < threshold,
        "lte" => numerator <= threshold,
        _ => bail!("comparison operator must be eq/gt/gte/lt/lte"),
    };
    Ok(json!({
        "metric": "token_price_vs_api_reference",
        "reference_price_basis": "token_derived_per_share",
        "reference_price_updated_at": null,
        "stock_token": price.token_contract_address,
        "chain_id": price.binance_chain_id,
        "platform_id": price.platform_id,
        "token_price": price.token_price,
        "reference_price": price.reference_price,
        "token_price_updated_at": price.token_price_updated_at,
        "token_price_age_ms": age_ms,
        "max_age_seconds": request.max_age_seconds,
        "spread_bps": ratio(&numerator, &reference),
        "spread_percent": ratio(&numerator, &(&reference * 100)),
        "position": match numerator.sign() {
            Sign::Minus => "below_reference",
            Sign::NoSign => "at_reference",
            Sign::Plus => "above_reference",
        },
        "operator": request.operator,
        "threshold_bps": request.threshold_bps,
        "matched": matched,
    }))
}

// Display only: truncate towards zero at six decimal places.
fn ratio(numerator: &BigInt, denominator: &BigInt) -> String {
    let scaled: BigInt = numerator * 1_000_000 / denominator;
    let magnitude = agentic::decimal(scaled.magnitude(), 6);
    if scaled.sign() == Sign::Minus {
        format!("-{magnitude}")
    } else {
        magnitude
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_000_000_000;
    const STOCK: &str = "0x390a684ef9cade28a7ad0dfa61ab1eb3842618c4";

    fn request(token_price: &str) -> Value {
        json!({
            "prices": [{"binanceChainId":"56","tokenContractAddress":STOCK,
                "platformId":"ondo","tokenPrice":token_price,"referencePrice":"100",
                "tokenPriceUpdatedAt":NOW - 1000}],
            "stock_token":STOCK,
            "intent":{"from_token":"0x55d398326f99059fF775485246999027B3197955",
                "to_token":STOCK,"amount":"6","slippage_bps":50},
            "operator":"lte","threshold_bps":-100,"max_age_seconds":300,
        })
    }

    fn check(value: Value) -> Result<Value> {
        evaluate(serde_json::from_value(value)?, NOW)
    }

    #[test]
    fn discount_and_premium_boundaries_use_unrounded_signed_ratios() {
        for (price, matched) in [
            ("98.999999999999999999", true),
            ("99", true),
            ("99.000000000000000001", false),
            ("100", false),
            ("101", false),
        ] {
            let result = check(request(price)).unwrap();
            assert_eq!(result["matched"], matched, "{price}");
        }
        let discount = check(request("99")).unwrap();
        assert_eq!(discount["spread_bps"], "-100");
        assert_eq!(discount["spread_percent"], "-1");
        assert_eq!(discount["position"], "below_reference");
        for (price, matched) in [
            ("100.999999999999999999", false),
            ("101", true),
            ("101.000000000000000001", true),
        ] {
            let mut value = request(price);
            value["operator"] = json!("gte");
            value["threshold_bps"] = json!(100);
            assert_eq!(check(value).unwrap()["matched"], matched, "{price}");
        }
        let parity = check(request("100")).unwrap();
        assert_eq!(parity["spread_bps"], "0");
        assert_eq!(parity["position"], "at_reference");
    }

    #[test]
    fn relative_difference_uses_reference_as_denominator_and_reports_its_basis() {
        let mut value = request("3");
        value["prices"][0]["referencePrice"] = json!("2");
        value["operator"] = json!("eq");
        value["threshold_bps"] = json!(5000);
        let result = check(value).unwrap();
        assert_eq!(result["spread_bps"], "5000");
        assert_eq!(result["spread_percent"], "50");
        assert_eq!(result["matched"], true);
        assert_eq!(result["reference_price_basis"], "token_derived_per_share");
        assert!(result["reference_price_updated_at"].is_null());
        assert_eq!(result["token_price_age_ms"], 1000);
    }

    #[test]
    fn invalid_or_missing_prices_never_become_zero_or_a_signal() {
        for field in ["tokenPrice", "referencePrice"] {
            for bad in [
                json!(""),
                json!("0"),
                json!("-1"),
                json!("NaN"),
                json!("1e2"),
                json!("0.0000000000000000000000000000000000001"),
                json!(100),
                Value::Null,
            ] {
                let mut value = request("99");
                value["prices"][0][field] = bad;
                assert!(check(value).is_err(), "{field}");
            }
            let mut value = request("99");
            value["prices"][0].as_object_mut().unwrap().remove(field);
            assert!(check(value).is_err());
        }
    }

    #[test]
    fn stale_future_and_unusable_timestamps_fail_closed() {
        for (timestamp, allowed) in [
            (NOW, true),
            (NOW - 300_000, true),
            (NOW - 300_001, false),
            (NOW + 1, false),
            (0, false),
            (-1, false),
            (i64::MIN, false),
        ] {
            let mut value = request("99");
            value["prices"][0]["tokenPriceUpdatedAt"] = json!(timestamp);
            assert_eq!(check(value).is_ok(), allowed, "{timestamp}");
        }
        for age in [0, 86_401] {
            let mut value = request("99");
            value["max_age_seconds"] = json!(age);
            assert!(check(value).is_err());
        }
    }

    #[test]
    fn wrong_or_ambiguous_assets_and_unrecognized_conditions_fail_closed() {
        let base = request("99");
        for (pointer, replacement) in [
            ("/prices", json!([])),
            ("/prices", json!([base["prices"][0], base["prices"][0]])),
            ("/prices/0/binanceChainId", json!("1")),
            (
                "/prices/0/tokenContractAddress",
                json!(base["intent"]["from_token"]),
            ),
            (
                "/intent/to_token",
                json!("0x1111111111111111111111111111111111111111"),
            ),
            ("/operator", json!("contains")),
            ("/threshold_bps", json!(-100.1)),
        ] {
            let mut value = base.clone();
            *value.pointer_mut(pointer).unwrap() = replacement;
            assert!(check(value).is_err(), "{pointer}");
        }
    }
}
