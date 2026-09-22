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

use crate::policy::{Evaluation, ExecutionMode, TradeIntent, TradePolicy};

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
        name = "list_bnb_capabilities",
        description = "List validated BNB Chain workflow templates, official API surfaces, and the safety boundary for Agent use."
    )]
    fn list_bnb_capabilities(&self) -> Json<Capabilities> {
        Json(Capabilities {
            chain_id: "56".to_owned(),
            network: "BNB Smart Chain mainnet".to_owned(),
            templates: vec![
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
                    "Quote, build unsigned calldata, and simulate a spot swap without signing or broadcasting.",
                ),
            ],
            official_api_paths: vec![
                "/api/v1/dex/market/rwa/*".to_owned(),
                "/api/v1/dex/balance/all-token-balances-by-address".to_owned(),
                "/api/v1/dex/aggregator/quote".to_owned(),
                "/api/v1/dex/aggregator/swap".to_owned(),
                "/api/v1/dex/pre-transaction/simulate".to_owned(),
            ],
            safety_boundary: vec![
                "MCP tools never accept or return API secrets or private keys.".to_owned(),
                "Generated trade flows stop after simulation; they do not sign or broadcast.".to_owned(),
                "Execution policy requires BSC, size/slippage/impact limits, successful simulation, and explicit operator confirmation.".to_owned(),
                "Use an isolated signer or Binance Agentic Wallet only after policy approval.".to_owned(),
            ],
        })
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
        let can_handoff_to_signer =
            arguments.intent.mode == ExecutionMode::Execute && evaluation.allowed;
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
                    "Hand approved calldata to an isolated signer or Agentic Wallet",
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
                "Start with list_bnb_capabilities. Generate and validate a template, evaluate the trade policy, then build an execution plan. Never ask for private keys or API secrets. This MCP prepares and simulates trades but does not sign or broadcast them.",
            )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FlowTemplate {
    RwaDiscovery,
    WalletSnapshot,
    SafeSwapPreparation,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn all_embedded_templates_compile() {
        for template in [
            FlowTemplate::RwaDiscovery,
            FlowTemplate::WalletSnapshot,
            FlowTemplate::SafeSwapPreparation,
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
