use std::{
    collections::BTreeMap,
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
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::receipt::{watch_transaction, WatchOptions, WatchReport, TRANSACTION_RECEIPT};

use crate::policy::{Evaluation, TradeIntent, TradePolicy};

const RWA_DISCOVERY: &str = include_str!("../flows/rwa_discovery.http.yml");
const WALLET_SNAPSHOT: &str = include_str!("../flows/wallet_snapshot.http.yml");
const SAFE_SWAP_PREPARATION: &str = include_str!("../flows/safe_swap_preparation.http.yml");

#[derive(Clone)]
pub struct FlowBnbMcpServer {
    root: Arc<PathBuf>,
    connection: crate::setup::WalletProgress,
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
            connection: Arc::new(std::sync::Mutex::new(serde_json::json!({"phase":"idle"}))),
        })
    }

    fn template_source(template: FlowTemplate) -> &'static str {
        match template {
            FlowTemplate::StockStrategy => crate::strategy::TEMPLATE,
            FlowTemplate::StockSpreadStrategy => crate::strategy::SPREAD_TEMPLATE,
            FlowTemplate::RwaDiscovery => RWA_DISCOVERY,
            FlowTemplate::WalletSnapshot => WALLET_SNAPSHOT,
            FlowTemplate::SafeSwapPreparation => SAFE_SWAP_PREPARATION,
            FlowTemplate::TransactionReceipt => TRANSACTION_RECEIPT,
            FlowTemplate::AgenticStage => crate::agentic::STAGE,
            FlowTemplate::AgenticOrder => crate::agentic::ORDER,
        }
    }

    fn compile_source(source: &str) -> Result<(FlowDocument, FlowSummary, String), String> {
        if source.len() > 65_536 {
            return Err("flow YAML exceeds 64 KiB".into());
        }
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
        if canonical_yaml.len() > 65_536 {
            return Err("canonical flow YAML exceeds 64 KiB".into());
        }
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

    fn read_flow(&self, relative: &str, expected: Option<&str>) -> Result<String, String> {
        let path = self.resolve_destination(relative)?;
        let m = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !m.is_file() || m.len() > 65_536 {
            return Err("flow must be a regular file of at most 64 KiB".into());
        }
        let source = fs::read_to_string(path).map_err(|e| e.to_string())?;
        if expected.is_some_and(|hash| hash != source_hash(&source)) {
            return Err("flow changed since review; read and validate it again".into());
        }
        Ok(source)
    }

    fn save(&self, relative: &str, source: &str, overwrite: bool) -> Result<String, String> {
        let path = self.resolve_destination(relative)?;
        let parent = path.parent().expect("validated path has a parent");
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        self.reject_symlinks(&path)?;

        use std::os::unix::fs::OpenOptionsExt;
        let temp = parent.join(format!(
            ".flow-save-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let result = (|| -> std::io::Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(source.as_bytes())?;
            file.sync_all()?;
            if overwrite {
                fs::rename(&temp, &path)?;
            } else {
                fs::hard_link(&temp, &path)?;
            }
            fs::File::open(parent)?.sync_all()?;
            Ok(())
        })();
        let _ = fs::remove_file(temp);
        result.map_err(|e| format!("cannot save {}: {e}", path.display()))?;
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
        description = "Read wallet connection and pending login progress. Does not install, sign in or trade. During pairing poll every 3–5 seconds and show the official login_url and pairing_code to the user."
    )]
    pub async fn get_bnb_connection(&self) -> Result<Json<Value>, String> {
        let state = self
            .connection
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if matches!(
            state["phase"].as_str(),
            Some("preparing" | "awaiting_wallet" | "failed")
        ) {
            return Ok(Json(state));
        }
        let config = crate::setup::config_path(&self.root);
        Ok(Json(match crate::setup::connection_status(&config).await {
            Ok(address) => {
                serde_json::json!({"phase":"connected","connected":true,"wallet_address":address})
            }
            Err(_) => {
                serde_json::json!({"phase":"disconnected","connected":false,"message":"Ask the user to connect the wallet with connect_bnb_wallet. This does not authorize trading."})
            }
        }))
    }

    #[tool(
        description = "On explicit user request to connect/reconnect their wallet, prepare managed dependencies and start official Binance Agentic Wallet pairing. Returns immediately; poll get_bnb_connection for login URL/code and completion. No private keys, trade, or trading mandate. Does not open a browser automatically."
    )]
    pub async fn connect_bnb_wallet(&self) -> Result<Json<Value>, String> {
        let mut state = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(
            state["phase"].as_str(),
            Some("preparing" | "awaiting_wallet")
        ) {
            return Ok(Json(state.clone()));
        }
        *state = serde_json::json!({"phase":"preparing", "message":"正在准备依赖；请调用 get_bnb_connection 查看登录链接与进度。"});
        let response = state.clone();
        let progress = self.connection.clone();
        let root = self.root.clone();
        let config = crate::setup::config_path(&root);
        tokio::spawn(async move {
            let result =
                crate::setup::setup_for_root(&root, &config, false, true, Some(&progress)).await;
            *progress.lock().unwrap_or_else(|e| e.into_inner()) = match result {
                Ok(()) => serde_json::json!({"phase":"ready"}),
                Err(e) => {
                    serde_json::json!({"phase":"failed", "message":format!("{e:#}"), "retry_tool":"connect_bnb_wallet"})
                }
            };
        });
        Ok(Json(response))
    }

    #[tool(
        name = "get_bnb_strategy_authorization",
        description = "Inspect one operator-created automatic trading authorization, or list them when authorization_id is omitted. Shows frozen strategy, exact order intent, cumulative budget, expiry, cooldown and blockers. Cannot create or increase authorization.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    fn get_bnb_strategy_authorization(
        &self,
        Parameters(a): Parameters<AuthorizationQuery>,
    ) -> Result<Json<Value>, String> {
        let c = agentic_config(&self.root)?;
        match a.authorization_id {
            Some(id) => crate::autonomy::status(&c, &id),
            None => crate::autonomy::list(&c),
        }
        .map(Json)
        .map_err(|e| e.to_string())
    }
    #[tool(
        name = "execute_bnb_authorized_strategy",
        description = "REAL AUTOMATIC TRADING under an existing operator-created strategy authorization. May submit ONE native order WITHOUT terminal CONFIRM. Cannot override strategy, amount, wallet or limits. Requires a stable request_id per evaluation; retries return the same record and never replay. Starts background work: query get_bnb_authorized_execution and keep MCP running. New evaluations need new IDs only after previous results are terminal and safe. Not a scheduler.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    fn execute_bnb_authorized_strategy(
        &self,
        Parameters(a): Parameters<AuthorizedExecutionArgs>,
    ) -> Result<Json<Value>, String> {
        crate::autonomy::start(
            agentic_config(&self.root)?,
            a.authorization_id,
            a.request_id,
        )
        .map(Json)
        .map_err(|e| e.to_string())
    }
    #[tool(
        name = "get_bnb_authorized_execution",
        description = "Read durable automatic execution status and native settlement evidence. Unknown/interrupted outcomes must be inspected, never replayed with another request ID.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    fn get_bnb_authorized_execution(
        &self,
        Parameters(a): Parameters<AuthorizedExecutionArgs>,
    ) -> Result<Json<Value>, String> {
        crate::autonomy::execution(
            &agentic_config(&self.root)?,
            &a.authorization_id,
            &a.request_id,
        )
        .map(Json)
        .map_err(|e| e.to_string())
    }
    #[tool(
        name = "refresh_bnb_authorized_execution",
        description = "Read-only order and receipt refresh for an existing automatic execution with an order ID. Never resubmits or refunds budget. Does not automatically clear a halt.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn refresh_bnb_authorized_execution(
        &self,
        Parameters(a): Parameters<AuthorizedExecutionArgs>,
    ) -> Result<Json<Value>, String> {
        crate::autonomy::refresh(
            agentic_config(&self.root)?,
            a.authorization_id,
            a.request_id,
        )
        .await
        .map(Json)
        .map_err(|e| e.to_string())
    }
    #[tool(
        name = "revoke_bnb_strategy_authorization",
        description = "Revoke a strategy authorization to block future orders. Cannot undo an already dispatched transaction, create authorization or increase limits.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    fn revoke_bnb_strategy_authorization(
        &self,
        Parameters(a): Parameters<AuthorizationId>,
    ) -> Result<Json<Value>, String> {
        crate::autonomy::revoke(&agentic_config(&self.root)?, &a.authorization_id)
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(
        name = "save_bnb_flow",
        description = "Compile and atomically save USER-AUTHORED Flow YAML under the workspace root. No execution. For overwrites supply overwrite=true and the current sha256 from read_bnb_flow. Returns source hash and restricted strategy diagnostics.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    fn save_bnb_flow(
        &self,
        Parameters(a): Parameters<SaveFlowArguments>,
    ) -> Result<Json<Value>, String> {
        if a.overwrite {
            let hash = a
                .expected_sha256
                .as_deref()
                .ok_or("overwriting requires expected_sha256 from read_bnb_flow")?;
            self.read_flow(&a.path, Some(hash))?;
        }
        let (_, summary, yaml) = Self::compile_source(&a.yaml)?;
        let saved_path = self.save(&a.path, &yaml, a.overwrite)?;
        Ok(Json(
            serde_json::json!({"saved_path":saved_path,"sha256":source_hash(&yaml),"summary":summary,
            "strategy_validation_error":crate::strategy::check(&yaml).err().map(|e|e.to_string())}),
        ))
    }

    #[tool(
        name = "read_bnb_flow",
        description = "Read a workspace .http.yml strategy, including full YAML, input summary and sha256 for review or conditional overwrite. Never executes.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    fn read_bnb_flow(
        &self,
        Parameters(a): Parameters<ReadFlowArguments>,
    ) -> Result<Json<Value>, String> {
        let source = self.read_flow(&a.path, None)?;
        let (_, summary, _) = Self::compile_source(&source)?;
        Ok(Json(
            serde_json::json!({"path":a.path,"yaml":source,"sha256":source_hash(&source),"summary":summary}),
        ))
    }

    #[tool(
        name = "run_bnb_strategy",
        description = "Read-only evaluation of saved USER-AUTHORED Flow YAML with inputs. Runs bounded conditions/loops, native quotes and allowlisted Web3 GETs; returns outputs, steps and candidate trade decision. Never queues, signs or submits. Live data, not a historical backtest. Maximum 30s/32 requests. Use stock_strategy for minimum receive or stock_spread_strategy for signed RWA token/reference price deviation.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn run_bnb_strategy(
        &self,
        Parameters(a): Parameters<RunStrategyArguments>,
    ) -> Result<Json<Value>, String> {
        let source = self.read_flow(&a.path, a.expected_sha256.as_deref())?;
        let snapshot =
            crate::strategy::Snapshot::new(&source, a.inputs).map_err(|e| e.to_string())?;
        let report = crate::strategy::run(&agentic_config(&self.root)?, &snapshot)
            .await
            .map_err(|e| e.to_string())?;
        Ok(Json(
            serde_json::json!({"sha256":source_hash(&source),"report":report}),
        ))
    }

    #[tool(
        name = "request_bnb_strategy_execution",
        description = "Evaluate a reviewed saved strategy afresh and QUEUE at most one triggered intent for operator confirmation. Requires source sha256 and stable request_id; retries must reuse ID and inputs. Freezes YAML/inputs; operator rechecks conditions after confirmation before submitting. Not a background scheduler; never signs or submits. Existing execution status/cancel/refresh tools accept returned intent_id.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn request_bnb_strategy_execution(
        &self,
        Parameters(a): Parameters<QueueStrategyArguments>,
    ) -> Result<Json<Value>, String> {
        let source = self.read_flow(&a.path, Some(&a.expected_sha256))?;
        let snapshot =
            crate::strategy::Snapshot::new(&source, a.inputs).map_err(|e| e.to_string())?;
        agentic_inbox(&self.root)?
            .enqueue_strategy(a.request_id, snapshot)
            .await
            .map(Json)
            .map_err(|e| e.to_string())
    }

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
        agentic_inbox(&self.root)?
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
        agentic_inbox(&self.root)?
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
        agentic_inbox(&self.root)?
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
        agentic_inbox(&self.root)?
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
        let c = agentic_config(&self.root)?;
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
        let report =
            crate::agentic::inspect_order(agentic_config(&self.root)?, args.intent, args.order_id)
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
                TemplateInfo::new(FlowTemplate::StockStrategy, "Editable native quote -> exact minimum receive threshold -> trade decision. Run with run_bnb_strategy; queue with request_bnb_strategy_execution. No Web3 key needed."),
                TemplateInfo::new(FlowTemplate::StockSpreadStrategy, "RWA token price vs API reference price -> exact signed basis-point threshold -> candidate intent. Validates token identity and token-price age. Requires Web3 credentials. The reference is token-derived per-share data, not an independent equity quote. Read-only preview via run_bnb_strategy."),
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
                "User strategy profile: 64 KiB YAML, 16 KiB inputs, 30 seconds, 32 requests, at most one decision. Local POST https://flow-bnb.invalid/strategy/{quote,compare,rwa-spread,decision}; only official Web3 RWA platforms/search/price and wallet balances GETs. File bodies, custom auth, arbitrary network destinations and direct signing/submission are blocked. compare accepts nonnegative decimal strings and eq/gt/gte/lt/lte. decision accepts {triggered:boolean,intent:AgenticIntent}. Native quote accepts AgenticIntent and returns data.toCoinAmount. Conditional steps and bounded loops use standard Flow YAML. Runtime checks every rendered URL.".to_owned(),
                "rwa-spread accepts {prices: RWA price data array, stock_token, intent, operator: eq/gt/gte/lt/lte, threshold_bps: signed integer, max_age_seconds: 1..86400}. Exactly one BSC row must match the monitored token and a token in the intent. Positive bps is above reference, negative below. Uses exact arithmetic; displayed bps/percent are truncated to six decimal places. Rejects missing, zero, malformed, stale or future-dated prices. referencePrice is token-derived per-share data; its independent timestamp is unavailable. This is an API-field deviation, not a claim of equity-market arbitrage.".to_owned(),
                "MCP tools never accept Binance API credentials or wallet private keys; receipt reports omit the provider RPC URL.".to_owned(),
                "Manual requests queue intents for operator review. Automatic execution needs an operator-created frozen strategy mandate with exact intent, cumulative budget, order count, cooldown and expiry. Generated flows themselves never sign or broadcast.".to_owned(),
                "Legacy Web3 execution policy requires BSC, size/slippage/impact limits, successful simulation, and explicit operator confirmation.".to_owned(),
                "Native Agentic Wallet uses separate token-quantity/slippage limits and account/balance/audit gates; it does not inherit legacy USD, price-impact or simulation guarantees. Native execution tools durably queue, query, cancel unclaimed requests and refresh existing orders; Manual execution uses agentic-operator confirmation; execute_bnb_authorized_strategy may submit within a pre-authorized mandate without a per-order prompt. Unknown outcomes or discrepancies halt automatic execution.".to_owned(),
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
            canonical_yaml: canonical_yaml.clone(),
            saved_path,
            sha256: source_hash(&canonical_yaml),
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
            strategy_validation_error: crate::strategy::check(&canonical_yaml)
                .err()
                .map(|e| e.to_string()),
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
                "Onboarding: call get_bnb_connection to check connection. On user request use connect_bnb_wallet, then poll get_bnb_connection every 3–5 seconds. Show the official login_url and pairing_code; the user confirms in Binance App. Installation/login never authorize a trade. Automatic mode: inspect get_bnb_strategy_authorization first. When the user requests execution under an existing mandate, call execute_bnb_authorized_strategy with authorization_id and stable request_id, then poll get_bnb_authorized_execution. This can trade without CONFIRM. MCP cannot create, edit or expand mandates. Use revoke_bnb_strategy_authorization to stop future submissions. Never replace request IDs to recover unknown execution. Manual mode remains available. For user-authored strategies: generate stock_strategy for native quote thresholds or stock_spread_strategy for RWA token/reference deviation (the reference is token-derived per-share data), edit YAML, validate_bnb_flow, save_bnb_flow, read_bnb_flow, then run_bnb_strategy for a read-only preview. On user request call request_bnb_strategy_execution with the reviewed source hash and stable request_id. This queues only a triggered decision; operator rechecks frozen strategy after terminal confirmation. Query/cancel/refresh with existing agentic execution tools. Never treat a dry run as approval or replace an existing ID to retry. Start with list_bnb_capabilities. Generate and validate a template, evaluate the trade policy, then build an execution plan. Never ask for private keys or API secrets. For native Agentic Wallet use prepare_agentic_trade and request_agentic_execution with a stable request_id, then get_agentic_execution to retrieve the operator result. Never generate a replacement ID to retry a submitted or unknown order. Manual requests require operator terminal confirmation; automatic submission is allowed only under an operator-created mandate. Wallet signing remains in Agentic Wallet.",
            )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FlowTemplate {
    StockStrategy,
    StockSpreadStrategy,
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
    pub sha256: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ValidatedFlow {
    pub valid: bool,
    pub strategy_validation_error: Option<String>,
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
            FlowTemplate::StockStrategy,
            FlowTemplate::StockSpreadStrategy,
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
fn agentic_inbox(root: &Path) -> Result<crate::agentic_handoff::Inbox, String> {
    crate::agentic_handoff::Inbox::open(agentic_config(root)?).map_err(|e| e.to_string())
}
fn agentic_config(root: &Path) -> Result<crate::agentic::Config, String> {
    let p = crate::setup::config_path(root);
    crate::agentic::Config::read(&p)
        .map_err(|e| format!("{e}; run flow-bnb setup in {}", root.display()))
}

fn source_hash(source: &str) -> String {
    format!("{:x}", Sha256::digest(source.as_bytes()))
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveFlowArguments {
    pub path: String,
    pub yaml: String,
    #[serde(default)]
    pub overwrite: bool,
    pub expected_sha256: Option<String>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadFlowArguments {
    pub path: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunStrategyArguments {
    pub path: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, Value>,
    pub expected_sha256: Option<String>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueueStrategyArguments {
    pub path: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, Value>,
    pub expected_sha256: String,
    pub request_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationQuery {
    pub authorization_id: Option<String>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationId {
    pub authorization_id: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedExecutionArgs {
    pub authorization_id: String,
    pub request_id: String,
}
