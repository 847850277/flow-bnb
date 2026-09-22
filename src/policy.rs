use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const BSC_CHAIN_ID: &str = "56";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct TradePolicy {
    #[serde(default = "default_chain_id")]
    pub chain_id: String,
    #[serde(default = "default_max_notional_usd")]
    pub max_notional_usd: f64,
    #[serde(default = "default_max_slippage_bps")]
    pub max_slippage_bps: u32,
    #[serde(default = "default_max_price_impact_bps")]
    pub max_price_impact_bps: u32,
    #[serde(default)]
    pub allowed_token_addresses: Vec<String>,
    #[serde(default = "default_true")]
    pub require_successful_simulation: bool,
    #[serde(default = "default_true")]
    pub require_operator_confirmation: bool,
}

impl Default for TradePolicy {
    fn default() -> Self {
        Self {
            chain_id: default_chain_id(),
            max_notional_usd: default_max_notional_usd(),
            max_slippage_bps: default_max_slippage_bps(),
            max_price_impact_bps: default_max_price_impact_bps(),
            allowed_token_addresses: Vec::new(),
            require_successful_simulation: true,
            require_operator_confirmation: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Prepare,
    Execute,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SimulationStatus {
    Success,
    Failed,
    NotRun,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct TradeIntent {
    pub chain_id: String,
    pub from_token_address: String,
    pub to_token_address: String,
    pub notional_usd: f64,
    pub slippage_bps: u32,
    pub price_impact_bps: u32,
    pub mode: ExecutionMode,
    #[serde(default)]
    pub simulation_status: Option<SimulationStatus>,
    #[serde(default)]
    pub operator_confirmed: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub struct Violation {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub struct Evaluation {
    pub allowed: bool,
    pub phase: String,
    pub violations: Vec<Violation>,
}

impl TradePolicy {
    pub fn evaluate(&self, intent: &TradeIntent) -> Evaluation {
        let mut violations = Vec::new();

        if self.chain_id != BSC_CHAIN_ID {
            push(
                &mut violations,
                "policy_chain_not_bsc",
                "policy chain_id must be 56 for the BSC mainnet track",
            );
        }
        if intent.chain_id != self.chain_id {
            push(
                &mut violations,
                "chain_mismatch",
                format!(
                    "intent chain_id {} does not match policy chain_id {}",
                    intent.chain_id, self.chain_id
                ),
            );
        }
        validate_address(
            &intent.from_token_address,
            "invalid_from_token",
            &mut violations,
        );
        validate_address(
            &intent.to_token_address,
            "invalid_to_token",
            &mut violations,
        );
        if intent
            .from_token_address
            .eq_ignore_ascii_case(&intent.to_token_address)
        {
            push(
                &mut violations,
                "identical_tokens",
                "from and to token addresses must differ",
            );
        }
        if !intent.notional_usd.is_finite() || intent.notional_usd <= 0.0 {
            push(
                &mut violations,
                "invalid_notional",
                "notional_usd must be a positive finite number",
            );
        } else if intent.notional_usd > self.max_notional_usd {
            push(
                &mut violations,
                "notional_limit",
                format!(
                    "notional_usd {} exceeds limit {}",
                    intent.notional_usd, self.max_notional_usd
                ),
            );
        }
        if intent.slippage_bps > self.max_slippage_bps {
            push(
                &mut violations,
                "slippage_limit",
                format!(
                    "slippage {} bps exceeds limit {} bps",
                    intent.slippage_bps, self.max_slippage_bps
                ),
            );
        }
        if intent.price_impact_bps > self.max_price_impact_bps {
            push(
                &mut violations,
                "price_impact_limit",
                format!(
                    "price impact {} bps exceeds limit {} bps",
                    intent.price_impact_bps, self.max_price_impact_bps
                ),
            );
        }
        if !self.allowed_token_addresses.is_empty() {
            for (side, address) in [
                ("from", &intent.from_token_address),
                ("to", &intent.to_token_address),
            ] {
                if !self
                    .allowed_token_addresses
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(address))
                {
                    push(
                        &mut violations,
                        "token_not_allowed",
                        format!("{side} token {address} is not in the allowlist"),
                    );
                }
            }
        }
        if intent.mode == ExecutionMode::Execute {
            if self.require_successful_simulation
                && intent.simulation_status != Some(SimulationStatus::Success)
            {
                push(
                    &mut violations,
                    "simulation_required",
                    "a successful simulation is required before execution",
                );
            }
            if self.require_operator_confirmation && !intent.operator_confirmed {
                push(
                    &mut violations,
                    "confirmation_required",
                    "explicit operator confirmation is required before execution",
                );
            }
        }

        Evaluation {
            allowed: violations.is_empty(),
            phase: match intent.mode {
                ExecutionMode::Prepare => "prepare",
                ExecutionMode::Execute => "execute",
            }
            .to_owned(),
            violations,
        }
    }
}

fn validate_address(address: &str, code: &str, violations: &mut Vec<Violation>) {
    if address.len() != 42
        || !address.starts_with("0x")
        || !address[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        push(
            violations,
            code,
            format!("{address} is not a valid EVM contract address"),
        );
    }
}

fn push(violations: &mut Vec<Violation>, code: impl Into<String>, message: impl Into<String>) {
    violations.push(Violation {
        code: code.into(),
        message: message.into(),
    });
}

fn default_chain_id() -> String {
    BSC_CHAIN_ID.to_owned()
}

fn default_max_notional_usd() -> f64 {
    100.0
}

fn default_max_slippage_bps() -> u32 {
    50
}

fn default_max_price_impact_bps() -> u32 {
    100
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(byte: &str) -> String {
        format!("0x{}", byte.repeat(40))
    }

    fn safe_intent(mode: ExecutionMode) -> TradeIntent {
        TradeIntent {
            chain_id: BSC_CHAIN_ID.to_owned(),
            from_token_address: address("1"),
            to_token_address: address("2"),
            notional_usd: 25.0,
            slippage_bps: 30,
            price_impact_bps: 40,
            mode,
            simulation_status: Some(SimulationStatus::Success),
            operator_confirmed: true,
        }
    }

    #[test]
    fn prepare_mode_checks_market_risk_without_requiring_confirmation() {
        let mut intent = safe_intent(ExecutionMode::Prepare);
        intent.simulation_status = None;
        intent.operator_confirmed = false;
        assert!(TradePolicy::default().evaluate(&intent).allowed);
    }

    #[test]
    fn execution_requires_simulation_and_confirmation() {
        let mut intent = safe_intent(ExecutionMode::Execute);
        intent.simulation_status = Some(SimulationStatus::Failed);
        intent.operator_confirmed = false;

        let result = TradePolicy::default().evaluate(&intent);
        assert!(!result.allowed);
        assert_eq!(result.violations.len(), 2);
        assert!(result
            .violations
            .iter()
            .any(|violation| violation.code == "simulation_required"));
        assert!(result
            .violations
            .iter()
            .any(|violation| violation.code == "confirmation_required"));
    }

    #[test]
    fn limits_and_allowlist_are_enforced() {
        let policy = TradePolicy {
            allowed_token_addresses: vec![address("1")],
            ..TradePolicy::default()
        };
        let mut intent = safe_intent(ExecutionMode::Prepare);
        intent.notional_usd = 101.0;
        intent.slippage_bps = 51;
        intent.price_impact_bps = 101;

        let result = policy.evaluate(&intent);
        let codes = result
            .violations
            .iter()
            .map(|violation| violation.code.as_str())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"notional_limit"));
        assert!(codes.contains(&"slippage_limit"));
        assert!(codes.contains(&"price_impact_limit"));
        assert!(codes.contains(&"token_not_allowed"));
    }
}
