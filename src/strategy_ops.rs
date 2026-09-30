//! Read-only observations and exact relative comparisons for user-authored flows.
use crate::agentic::{self, Config, Intent, TokenRule};
use anyhow::{ensure, Context, Result};
use num_bigint::{BigInt, Sign};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ObservedToken {
    pub address: String,
    pub symbol: String,
    pub decimals: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Observation {
    pub intent: Intent,
    pub token: ObservedToken,
}

/// The extra token exists only in a temporary READ-ONLY quote configuration.
/// It never extends the persisted allowlist used by trade execution.
pub(crate) fn observation_config(c: &Config, o: &Observation) -> Result<Config> {
    ensure!(
        o.token.decimals <= 36 && !o.token.symbol.is_empty() && o.token.symbol.len() <= 64,
        "invalid observed token metadata"
    );
    ensure!(
        o.intent.to_token.eq_ignore_ascii_case(&o.token.address),
        "observed token must be the quote output"
    );
    let mut observed = c.clone();
    if let Some(t) = c
        .tokens
        .iter()
        .find(|t| t.address.eq_ignore_ascii_case(&o.token.address))
    {
        ensure!(
            t.symbol == o.token.symbol && t.decimals == o.token.decimals,
            "observed token metadata differs from configuration"
        );
    } else {
        observed.tokens.push(TokenRule {
            address: o.token.address.clone(),
            symbol: o.token.symbol.clone(),
            decimals: o.token.decimals,
            max_sell_amount: "1".into(),
        });
    }
    observed.rules(&o.intent)?;
    Ok(observed)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ratio {
    numerator: String,
    denominator: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RelativeChange {
    current: Ratio,
    baseline: Ratio,
    operator: String,
    threshold_bps: i32,
}

pub(crate) fn relative_change(value: Value) -> Result<Value> {
    let request: RelativeChange = serde_json::from_value(value)?;
    let positive = |s: &str| -> Result<BigInt> {
        let v = BigInt::from(agentic::units(s, 36)?);
        ensure!(v.sign() == Sign::Plus, "ratio components must be positive");
        Ok(v)
    };
    let current = positive(&request.current.numerator)? * positive(&request.baseline.denominator)?;
    let baseline = positive(&request.baseline.numerator)? * positive(&request.current.denominator)?;
    // Cross multiplication retains precision at the trigger boundary.
    let delta = (&current - &baseline) * 10_000;
    let threshold = &baseline * request.threshold_bps;
    let matched = match request.operator.as_str() {
        "lte" => delta <= threshold,
        "lt" => delta < threshold,
        "gte" => delta >= threshold,
        "gt" => delta > threshold,
        "eq" => delta == threshold,
        _ => anyhow::bail!("relative comparison requires eq/gt/gte/lt/lte"),
    };
    let display: BigInt = &delta * 10_000 / &baseline;
    let magnitude = agentic::decimal(display.magnitude(), 6);
    Ok(
        json!({"matched":matched,"change_percent":if display.sign()==Sign::Minus {
        format!("-{magnitude}")
    } else { magnitude },"threshold_bps":request.threshold_bps}),
    )
}

pub(crate) fn field<'a>(v: &'a Value, name: &str) -> Result<&'a str> {
    v[name]
        .as_str()
        .with_context(|| format!("missing string field {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_relative_change_boundaries_and_invalid_ratios() {
        for (current, hit) in [
            ("5.099999999999999999", false),
            ("5.10", true),
            ("5.100000000000000001", true),
        ] {
            let r=relative_change(json!({"current":{"numerator":current,"denominator":"1"},
                "baseline":{"numerator":"5","denominator":"1"},"operator":"gte","threshold_bps":200})).unwrap();
            assert_eq!(r["matched"], hit);
        }
        for (current, hit) in [
            ("0.024999999999999999", false),
            ("0.025", true),
            ("0.025000000000000001", true),
        ] {
            let r=relative_change(json!({"current":{"numerator":"1","denominator":current},
                "baseline":{"numerator":"1","denominator":"0.0245"},"operator":"lte","threshold_bps":-200})).unwrap();
            assert_eq!(r["matched"], hit);
        }
        for bad in ["0", "-1", "NaN"] {
            assert!(relative_change(json!({"current":{"numerator":"1","denominator":bad},
                "baseline":{"numerator":"1","denominator":"1"},"operator":"lte","threshold_bps":-200})).is_err());
        }
    }
}
