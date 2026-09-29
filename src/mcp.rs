use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use postman_flow::{
    compile_flow, parse_flow_yaml, write_flow_yaml, CompileEnvironment, FlowDocument,
};
use rmcp::{
    handler::server::wrapper::{Json, Parameters},
    model::{Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router, ServerHandler,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::receipt::{watch_transaction, WatchOptions, WatchReport, TRANSACTION_RECEIPT};

use crate::policy::{Evaluation, TradeIntent, TradePolicy};

const RWA_DISCOVERY: &str = include_str!("../flows/rwa_discovery.http.yml");
const WALLET_SNAPSHOT: &str = include_str!("../flows/wallet_snapshot.http.yml");
const SAFE_SWAP_PREPARATION: &str = include_str!("../flows/safe_swap_preparation.http.yml");

#[derive(Clone)]
pub struct FlowBnbMcpServer {
    root: Arc<PathBuf>,
}

impl FlowBnbMcpServer {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = root.as_ref().canonicalize().map_err(|error| {
            format!(
                "cannot resolve flow-bnb MCP root {}: {error}",
                root.as_ref().display()
            )
        })?;
        if !root.is_dir() {
            return Err(format!("MCP root is not a directory: {}", root.display()));
        }
        Ok(Self {
            root: Arc::new(root),
        })
    }

    fn template_source(template: FlowTemplate) -> &'static str {
        match template {
            FlowTemplate::RwaDiscovery => RWA_DISCOVERY,
            FlowTemplate::WalletSnapshot => WALLET_SNAPSHOT,
            FlowTemplate::SafeSwapPreparation => SAFE_SWAP_PREPARATION,
            FlowTemplate::TransactionReceipt => TRANSACTION_RECEIPT,
            FlowTemplate::AgenticStage => crate::agentic::STAGE,
            FlowTemplate::AgenticOrder => crate::agentic::ORDER,
        }
    }

    fn compile_source(source: &str) -> Result<(FlowDocument, FlowSummary, String), String> {
        let document = parse_flow_yaml(source).map_err(|error| error.to_string())?;
        let plan = compile_flow(
            &document.flow,
            &document.apis,
            &CompileEnvironment::default(),
        )
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| {
                    format!(
                        "{:?} at {}: {}",
                        diagnostic.code, diagnostic.location.field, diagnostic.message
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })?;
        let summary = FlowSummary {
            name: plan.name().to_owned(),
            step_count: plan.step_count(),
            required_inputs: document
                .flow
                .inputs
                .iter()
                .filter(|input| input.default.is_none())
                .map(|input| input.name.clone())
                .collect(),
            outputs: plan
                .outputs()
                .iter()
                .map(|output| output.name.clone())
                .collect(),
        };
        let canonical_yaml = write_flow_yaml(&document).map_err(|error| error.to_string())?;
        Ok((document, summary, canonical_yaml))
    }

    fn resolve_destination(&self, relative: &str) -> Result<PathBuf, String> {
        let relative = Path::new(relative);
        if relative.as_os_str().is_empty() || relative.is_absolute() {
            return Err("path must be a non-empty relative path".to_owned());
        }
        let name = relative
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if !name.ends_with(".http.yml") && !name.ends_with(".http.yaml") {
            return Err("path must end in .http.yml or .http.yaml".to_owned());
        }
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err("path must not contain `.`, `..`, root, or platform prefixes".to_owned());
        }
        let path = self.root.join(relative);
        self.reject_symlinks(&path)?;
        Ok(path)
    }

    fn reject_symlinks(&self, path: &Path) -> Result<(), String> {
        let relative = path
            .strip_prefix(self.root.as_ref())
            .map_err(|_| "path escaped the configured MCP root".to_owned())?;
        let mut current = self.root.as_ref().clone();
        for component in relative.components() {
            current.push(component);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(format!(
                        "refusing symlinked path inside MCP root: {}",
                        current.display()
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => {
                    return Err(format!("cannot inspect {}: {error}", current.display()));
                }
            }
        }
        Ok(())
    }

    fn save(&self, relative: &str, source: &str, overwrite: bool) -> Result<String, String> {
        let path = self.resolve_destination(relative)?;
        let parent = path.parent().expect("validated path has a parent");
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        self.reject_symlinks(&path)?;

        let mut options = fs::OpenOptions::new();
        options.write(true);
        if overwrite {
            options.create(true).truncate(true);
        } else {
            options.create_new(true);
        }
        let mut file = options.open(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                format!("{} already exists; pass overwrite=true", path.display())
            } else {
                format!("cannot write {}: {error}", path.display())
            }
        })?;
        file.write_all(source.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("cannot persist {}: {error}", path.display()))?;
        Ok(path
            .strip_prefix(self.root.as_ref())
            .expect("destination is rooted")
            .display()
            .to_string())
    }
}

#[tool_router]
impl FlowBnbMcpServer {
    #[tool(
        name = "request_agentic_execution",
        description = "Queue one native Agentic Wallet trade for an operator. Requires a stable request_id: reuse it with identical arguments on transport retries; never create a replacement to retry a trade. Uses operator-owned configuration and local token limits. Does not submit or approve; an operator's agentic-operator terminal must prepare afresh and confirm.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    fn request_agentic_execution(
        &self,
        Parameters(args): Parameters<AgenticExecutionArgs>,
    ) -> Result<Json<serde_json::Value>, String> {
        agentic_inbox()?
            .enqueue(args.request_id, args.intent)
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(
        name = "get_agentic_execution",
        description = "Read the durable state/result of a queued native trade, including order ID, receipt and actual transfer amounts. No wallet/network calls. Never retry execution when pending, unknown or settled_with_discrepancy; that state means a real trade with a discrepancy requiring review.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    fn get_agentic_execution(
        &self,
        Parameters(args): Parameters<ExecutionIntentId>,
    ) -> Result<Json<serde_json::Value>, String> {
        agentic_inbox()?
            .status(&args.intent_id)
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(
        name = "cancel_agentic_execution",
        description = "Cancel a queued native trade only before an operator claims it. Does not revoke, cancel or undo a submitted on-chain order.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    fn cancel_agentic_execution(
        &self,
        Parameters(args): Parameters<ExecutionIntentId>,
    ) -> Result<Json<serde_json::Value>, String> {
        agentic_inbox()?
            .cancel(&args.intent_id)
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(
        name = "refresh_agentic_execution",
        description = "Resume read-only order/receipt verification for a queued native trade that already has an order ID; updates its local report. Never submits, signs, approves or automatically retries a trade. May take up to the bounded polling deadline; do not use a new trade request as a retry.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn refresh_agentic_execution(
        &self,
        Parameters(args): Parameters<ExecutionIntentId>,
    ) -> Result<Json<serde_json::Value>, String> {
        agentic_inbox()?
            .refresh(&args.intent_id)
            .await
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(
        name = "prepare_agentic_trade",
        description = "Read-only native Agentic Wallet preparation through Flow: account binding, local token amount/slippage limits, on-chain balances, quote and token audit. Does not submit. Uses operator-configured FLOW_BNB_AGENTIC_CONFIG. Limits differ from legacy Web3 execution policy.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn prepare_agentic_trade(
        &self,
        Parameters(intent): Parameters<crate::agentic::Intent>,
    ) -> Result<Json<serde_json::Value>, String> {
        let c = agentic_config()?;
        let report = crate::agentic::prepare(&c, intent)
            .await
            .map_err(|e| e.to_string())?;
        serde_json::to_value(report)
            .map(Json)
            .map_err(|e| e.to_string())
    }
    #[tool(
        name = "inspect_agentic_order",
        description = "Read-only Flow order polling and BSC receipt/Transfer-log reconciliation for an existing Agentic Wallet order. Compares reported output with actual wallet transfers. Never submits or retries a trade.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn inspect_agentic_order(
        &self,
        Parameters(args): Parameters<AgenticOrderArgs>,
    ) -> Result<Json<serde_json::Value>, String> {
        let report = crate::agentic::inspect_order(agentic_config()?, args.intent, args.order_id)
            .await
            .map_err(|e| e.to_string())?;
        serde_json::to_value(report)
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(
        name = "list_bnb_capabilities",
        description = "List validated BNB Chain workflow templates, official API surfaces, and the safety boundary for Agent use."
    )]
    fn list_bnb_capabilities(&self) -> Json<Capabilities> {
        Json(Capabilities {
            chain_id: "56".to_owned(),
            network: "BNB Smart Chain mainnet".to_owned(),
            templates: vec![
                TemplateInfo::new(FlowTemplate::AgenticStage, "Native wallet checks, quotes and operator-only order submission through a local process adapter; requires agentic-trade, not generic HTTP run."),
                TemplateInfo::new(FlowTemplate::AgenticOrder, "Bounded native order polling through the local adapter; use agentic-track or inspect_agentic_order."),
                TemplateInfo::new(FlowTemplate::TransactionReceipt, "Track an existing transaction via read-only JSON-RPC with bounded polling and confirmation checks; requires the flow-bnb receipt adapter."),
                TemplateInfo::new(
                    FlowTemplate::RwaDiscovery,
                    "Discover Ondo, bStocks, or xStocks assets and compare token/reference prices.",
                ),
                TemplateInfo::new(
                    FlowTemplate::WalletSnapshot,
                    "Read BSC balances and recent transactions while excluding risk tokens.",
                ),
                TemplateInfo::new(
                    FlowTemplate::SafeSwapPreparation,
                    "Ordinary SWAP routes only: quote, build and simulate; RFQ stock orders require prepare_trade diagnosis.",
                ),
            ],
            rpc_methods: vec!["eth_chainId", "eth_getTransactionReceipt", "eth_blockNumber", "eth_getBlockByNumber"].into_iter().map(str::to_owned).collect(),
            official_api_paths: vec![
                "/api/v1/dex/market/rwa/*".to_owned(),
                "/api/v1/dex/balance/all-token-balances-by-address".to_owned(),
                "/api/v1/dex/aggregator/quote".to_owned(),
                "/api/v1/dex/aggregator/swap".to_owned(),
                "/api/v1/dex/pre-transaction/simulate".to_owned(),
            ],
            safety_boundary: vec![
                "MCP tools never accept Binance API credentials or wallet private keys; receipt reports omit the provider RPC URL.".to_owned(),
                "MCP may queue intents for operator review, but cannot approve them. Generated flows do not sign or broadcast.".to_owned(),
                "Legacy Web3 execution policy requires BSC, size/slippage/impact limits, successful simulation, and explicit operator confirmation.".to_owned(),
                "Native Agentic Wallet uses separate token-quantity/slippage limits and account/balance/audit gates; it does not inherit legacy USD, price-impact or simulation guarantees. Native execution tools durably queue, query, cancel unclaimed requests and refresh existing orders; agentic-operator alone submits after fresh terminal confirmation.".to_owned(),
                "Use an isolated signer or Binance Agentic Wallet only after policy approval.".to_owned(),
            ],
        })
    }

    #[tool(
        name = "request_trade_execution",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        ),
        description = "Queue a trade intent for an OPERATOR to review. Does not approve, sign or broadcast. Uses operator-configured FLOW_BNB_HANDOFF_DIR. The operator CLI obtains fresh evidence and requests terminal confirmation; no caller-supplied policy, signer or approval is accepted."
    )]
    fn request_trade_execution(
        &self,
        Parameters(request): Parameters<crate::trade::TradeRequest>,
    ) -> Result<Json<crate::handoff::Queued>, String> {
        execution_inbox()?
            .enqueue(request)
            .map(Json)
            .map_err(|e| e.to_string())
    }
    #[tool(
        name = "get_trade_execution",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        ),
        description = "Read a queued trade's durable operator/execution status. Unknown or interrupted outcomes must be inspected, never automatically resubmitted."
    )]
    fn get_trade_execution(
        &self,
        Parameters(args): Parameters<ExecutionIntentId>,
    ) -> Result<Json<crate::handoff::Status>, String> {
        execution_inbox()?
            .status(&args.intent_id)
            .map(Json)
            .map_err(|e| e.to_string())
    }
    #[tool(
        name = "cancel_trade_execution",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        ),
        description = "Cancel an unclaimed intent. Cannot cancel a transaction after the operator has claimed it or the wallet has received it."
    )]
    fn cancel_trade_execution(
        &self,
        Parameters(args): Parameters<ExecutionIntentId>,
    ) -> Result<Json<crate::handoff::Status>, String> {
        execution_inbox()?
            .cancel(&args.intent_id)
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(
        name = "prepare_trade",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = true
        ),
        description = "Read-only evidence-bound trade preparation: wallet, quote-derived risk, approval calldata validation and simulation. Uses operator-configured FLOW_BNB_POLICY_FILE and environment API credentials. Reports RFQ requirements explicitly; never signs or submits."
    )]
    async fn prepare_trade(
        &self,
        Parameters(request): Parameters<crate::trade::TradeRequest>,
    ) -> Result<Json<crate::trade::TradeReport>, String> {
        let path = std::env::var("FLOW_BNB_POLICY_FILE")
            .map_err(|_| "operator must configure FLOW_BNB_POLICY_FILE")?;
        let policy =
            serde_json::from_slice(&fs::read(path).map_err(|_| "cannot read operator policy")?)
                .map_err(|_| "invalid operator policy")?;
        let key = std::env::var("BINANCE_WEB3_API_KEY").map_err(|_| "API key not configured")?;
        let secret =
            std::env::var("BINANCE_WEB3_SECRET_KEY").map_err(|_| "API secret not configured")?;
        let api = crate::BinanceWeb3Transport::new(key, secret)
            .map_err(|_| "cannot initialize API transport")?;
        let api = crate::trade::BalanceFallback {
            api,
            rpc: postman_request::RequestClient::try_new("flow-bnb-balance")
                .map_err(|_| "cannot initialize RPC transport")?,
            rpc_url: std::env::var("FLOW_BNB_RPC_URL").ok(),
        };
        Ok(Json(
            crate::trade::prepare_with_transport(request, policy, api)
                .await
                .into_report(),
        ))
    }

    #[tool(
        name = "watch_transaction",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = true
        ),
        description = "Track an already-broadcast transaction via read-only HTTP(S) JSON-RPC. Verify chain ID, canonical receipt block and confirmation count with bounded polling. Returns a structured result for confirmed, reverted, timeout, iteration exhaustion or RPC error. Never signs or broadcasts; no Binance API credentials required. RPC URL may use a provider key but must not contain private keys."
    )]
    async fn watch_transaction(
        &self,
        Parameters(options): Parameters<WatchOptions>,
    ) -> Result<Json<WatchReport>, String> {
        watch_transaction(options)
            .await
            .map(Json)
            .map_err(|error| error.to_string())
    }

    #[tool(
        name = "generate_bnb_flow",
        description = "Return a canonical, compiled BNB workflow template and optionally save it under the configured workspace root."
    )]
    fn generate_bnb_flow(
        &self,
        Parameters(arguments): Parameters<GenerateFlowArguments>,
    ) -> Result<Json<GeneratedFlow>, String> {
        let (_, summary, canonical_yaml) =
            Self::compile_source(Self::template_source(arguments.template))?;
        let saved_path = arguments
            .path
            .as_deref()
            .map(|path| self.save(path, &canonical_yaml, arguments.overwrite))
            .transpose()?;
        Ok(Json(GeneratedFlow {
            template: arguments.template,
            summary,
            canonical_yaml,
            saved_path,
        }))
    }

    #[tool(
        name = "validate_bnb_flow",
        description = "Parse and compile a BNB Flow YAML document without network access, signing, or execution."
    )]
    fn validate_bnb_flow(
        &self,
        Parameters(arguments): Parameters<ValidateFlowArguments>,
    ) -> Result<Json<ValidatedFlow>, String> {
        let (_, summary, canonical_yaml) = Self::compile_source(&arguments.yaml)?;
        Ok(Json(ValidatedFlow {
            valid: true,
            summary,
            canonical_yaml,
        }))
    }

    #[tool(
        name = "evaluate_trade_policy",
        description = "Evaluate a BSC trade intent against deterministic size, slippage, price-impact, token allowlist, simulation, and confirmation gates."
    )]
    fn evaluate_trade_policy(
        &self,
        Parameters(arguments): Parameters<EvaluatePolicyArguments>,
    ) -> Json<Evaluation> {
        Json(arguments.policy.evaluate(&arguments.intent))
    }

    #[tool(
        name = "build_execution_plan",
        description = "Build an auditable Agent execution plan. This tool plans handoff to an external signer but never signs or broadcasts a transaction."
    )]
    fn build_execution_plan(
        &self,
        Parameters(arguments): Parameters<EvaluatePolicyArguments>,
    ) -> Json<ExecutionPlan> {
        let evaluation = arguments.policy.evaluate(&arguments.intent);
        // This caller-supplied intent is advisory, never an execution capability.
        let can_handoff_to_signer = false;
        Json(ExecutionPlan {
            intent: arguments.intent,
            policy: arguments.policy,
            evaluation,
            stages: vec![
                PlanStage::new("quote", "Fetch an aggregated BSC spot quote", "flow"),
                PlanStage::new("policy", "Apply local deterministic risk limits", "local"),
                PlanStage::new("build", "Build unsigned transaction calldata", "flow"),
                PlanStage::new(
                    "simulate",
                    "Simulate transaction and inspect state changes",
                    "flow",
                ),
                PlanStage::new(
                    "confirm",
                    "Request explicit operator confirmation",
                    "operator",
                ),
                PlanStage::new(
                    "sign",
                    "Hand approved calldata to an isolated signer; native Agentic orders use a separate workflow",
                    "external",
                ),
                PlanStage::new(
                    "broadcast",
                    "Broadcast and verify the receipt in a separate execution adapter",
                    "external",
                ),
            ],
            can_handoff_to_signer,
            mcp_signs_or_broadcasts: false,
        })
    }
}

#[tool_handler]
impl ServerHandler for FlowBnbMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("flow-bnb-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Start with list_bnb_capabilities. Generate and validate a template, evaluate the trade policy, then build an execution plan. Never ask for private keys or API secrets. For native Agentic Wallet use prepare_agentic_trade and request_agentic_execution with a stable request_id, then get_agentic_execution to retrieve the operator result. Never generate a replacement ID to retry a submitted or unknown order. The operator terminal authorizes execution; this MCP does not sign or broadcast.",
            )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FlowTemplate {
    AgenticStage,
    AgenticOrder,
    RwaDiscovery,
    WalletSnapshot,
    SafeSwapPreparation,
    TransactionReceipt,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GenerateFlowArguments {
    pub template: FlowTemplate,
    /// Optional relative destination such as flows/generated.http.yml.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ValidateFlowArguments {
    pub yaml: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EvaluatePolicyArguments {
    #[serde(default)]
    pub policy: TradePolicy,
    pub intent: TradeIntent,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Capabilities {
    pub chain_id: String,
    pub network: String,
    pub templates: Vec<TemplateInfo>,
    pub official_api_paths: Vec<String>,
    pub rpc_methods: Vec<String>,
    pub safety_boundary: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TemplateInfo {
    pub id: FlowTemplate,
    pub description: String,
}

impl TemplateInfo {
    fn new(id: FlowTemplate, description: impl Into<String>) -> Self {
        Self {
            id,
            description: description.into(),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GeneratedFlow {
    pub template: FlowTemplate,
    pub summary: FlowSummary,
    pub canonical_yaml: String,
    pub saved_path: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ValidatedFlow {
    pub valid: bool,
    pub summary: FlowSummary,
    pub canonical_yaml: String,
}

#[derive(Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct FlowSummary {
    pub name: String,
    pub step_count: usize,
    pub required_inputs: Vec<String>,
    pub outputs: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExecutionPlan {
    pub intent: TradeIntent,
    pub policy: TradePolicy,
    pub evaluation: Evaluation,
    pub stages: Vec<PlanStage>,
    pub can_handoff_to_signer: bool,
    pub mcp_signs_or_broadcasts: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PlanStage {
    pub id: String,
    pub action: String,
    pub executor: String,
}

impl PlanStage {
    fn new(id: impl Into<String>, action: impl Into<String>, executor: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            action: action.into(),
            executor: executor.into(),
        }
    }
}

fn execution_inbox() -> Result<crate::handoff::Inbox, String> {
    let path = std::env::var("FLOW_BNB_HANDOFF_DIR")
        .map_err(|_| "operator must configure FLOW_BNB_HANDOFF_DIR")?;
    crate::handoff::Inbox::open(path).map_err(|e| e.to_string())
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecutionIntentId {
    pub intent_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn all_embedded_templates_compile() {
        for template in [
            FlowTemplate::AgenticStage,
            FlowTemplate::AgenticOrder,
            FlowTemplate::RwaDiscovery,
            FlowTemplate::WalletSnapshot,
            FlowTemplate::SafeSwapPreparation,
            FlowTemplate::TransactionReceipt,
        ] {
            let (_, summary, yaml) =
                FlowBnbMcpServer::compile_source(FlowBnbMcpServer::template_source(template))
                    .unwrap();
            assert!(summary.step_count > 0);
            parse_flow_yaml(&yaml).unwrap();
        }
    }

    #[test]
    fn saves_only_below_the_configured_root() {
        let directory = tempdir().unwrap();
        let server = FlowBnbMcpServer::new(directory.path()).unwrap();
        let (_, _, yaml) = FlowBnbMcpServer::compile_source(WALLET_SNAPSHOT).unwrap();
        let path = server
            .save("generated/wallet.http.yml", &yaml, false)
            .unwrap();
        assert_eq!(path, "generated/wallet.http.yml");
        assert!(directory.path().join(path).is_file());
        assert!(server.save("../escape.http.yml", &yaml, false).is_err());
        assert!(server.save("/tmp/escape.http.yml", &yaml, false).is_err());
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgenticOrderArgs {
    pub intent: crate::agentic::Intent,
    pub order_id: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgenticExecutionArgs {
    /// Stable caller-generated key, reused for retries of this same intent.
    pub request_id: String,
    pub intent: crate::agentic::Intent,
}
fn agentic_inbox() -> Result<crate::agentic_handoff::Inbox, String> {
    crate::agentic_handoff::Inbox::open(agentic_config()?).map_err(|e| e.to_string())
}
fn agentic_config() -> Result<crate::agentic::Config, String> {
    let p = std::env::var("FLOW_BNB_AGENTIC_CONFIG")
        .map_err(|_| "operator must configure FLOW_BNB_AGENTIC_CONFIG".to_string())?;
    crate::agentic::Config::read(Path::new(&p)).map_err(|e| e.to_string())
}
