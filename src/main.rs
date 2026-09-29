use std::{collections::HashSet, env, fs, path::PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use flow_bnb::{
    receipt::{watch_transaction, WatchOptions},
    BinanceWeb3Transport,
};
use futures::StreamExt;
use postman_flow::{
    compile_flow, execute_flow, parse_flow_yaml, CompileEnvironment, FlowEvent, FlowInputs,
    FlowSessionEnvironment,
};
use postman_http::request::RequestOptions;

#[derive(Parser)]
#[command(
    version,
    about = "Run auditable BNB Chain workflows powered by postman-flow"
)]
struct Cli {
    /// Show HTTP method/URL, headers, bodies, status and timing on stderr.
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Authorize a frozen strategy once; no per-order CONFIRM within this mandate.
    StrategyAuthorize {
        file: PathBuf,
        #[arg(
            long,
            default_value = ".flow-bnb/agentic.json",
            env = "FLOW_BNB_AGENTIC_CONFIG"
        )]
        config: PathBuf,
        #[arg(long)]
        id: String,
        #[arg(long = "input", value_name = "NAME=VALUE")]
        inputs: Vec<String>,
        #[arg(long)]
        max_orders: u32,
        #[arg(long)]
        max_total_sell_amount: String,
        #[arg(long)]
        valid_for_minutes: u32,
        #[arg(long, default_value_t = 60)]
        cooldown_seconds: u32,
    },
    /// Run one pre-authorized strategy evaluation without a terminal prompt.
    StrategyAuto {
        #[arg(
            long,
            default_value = ".flow-bnb/agentic.json",
            env = "FLOW_BNB_AGENTIC_CONFIG"
        )]
        config: PathBuf,
        #[arg(long)]
        authorization_id: String,
        #[arg(long)]
        request_id: String,
    },
    /// Inspect one authorization, or list all when --id is omitted.
    StrategyAuthorization {
        #[arg(
            long,
            default_value = ".flow-bnb/agentic.json",
            env = "FLOW_BNB_AGENTIC_CONFIG"
        )]
        config: PathBuf,
        #[arg(long)]
        id: Option<String>,
    },
    /// Revoke future automatic orders; already dispatched orders are unaffected.
    StrategyRevoke {
        #[arg(
            long,
            default_value = ".flow-bnb/agentic.json",
            env = "FLOW_BNB_AGENTIC_CONFIG"
        )]
        config: PathBuf,
        #[arg(long)]
        id: String,
    },
    /// Evaluate a user strategy without trading; optionally queue a triggered intent for an operator.
    StrategyRun {
        file: PathBuf,
        #[arg(
            long,
            default_value = ".flow-bnb/agentic.json",
            env = "FLOW_BNB_AGENTIC_CONFIG"
        )]
        config: PathBuf,
        #[arg(long = "input", value_name = "NAME=VALUE")]
        inputs: Vec<String>,
        /// Stable retry key. Queues for confirmation, never directly submits a trade.
        #[arg(long)]
        enqueue: Option<String>,
    },
    /// Install managed wallet dependencies, pair the wallet and generate MCP configuration.
    Setup {
        #[arg(
            long,
            default_value = ".flow-bnb/agentic.json",
            env = "FLOW_BNB_AGENTIC_CONFIG"
        )]
        config: PathBuf,
        /// Prepare dependencies without initiating a new wallet login.
        #[arg(long)]
        no_login: bool,
        /// Display the login link without opening a browser.
        #[arg(long)]
        no_open: bool,
    },
    /// Check wallet installation, login, account binding and outstanding submission locks.
    Doctor {
        #[arg(
            long,
            default_value = ".flow-bnb/agentic.json",
            env = "FLOW_BNB_AGENTIC_CONFIG"
        )]
        config: PathBuf,
    },
    /// Start the MCP stdio service from the same executable.
    Mcp {
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Operate queued native trades; every order requires fresh terminal confirmation.
    AgenticOperator {
        #[arg(
            long,
            default_value = ".flow-bnb/agentic.json",
            env = "FLOW_BNB_AGENTIC_CONFIG"
        )]
        config: PathBuf,
        #[arg(long, required_unless_present = "watch", conflicts_with = "watch")]
        intent_id: Option<String>,
        /// Keep this terminal open to review incoming MCP intents, oldest first.
        #[arg(long)]
        watch: bool,
    },
    /// Read a queued native trade's status, optionally refreshing its existing order.
    AgenticExecution {
        #[arg(
            long,
            default_value = ".flow-bnb/agentic.json",
            env = "FLOW_BNB_AGENTIC_CONFIG"
        )]
        config: PathBuf,
        #[arg(long)]
        intent_id: String,
        #[arg(long)]
        refresh: bool,
    },
    /// Prepare a native Agentic Wallet order through Flow; --execute requests terminal confirmation.
    AgenticTrade {
        #[arg(
            long,
            default_value = ".flow-bnb/agentic.json",
            env = "FLOW_BNB_AGENTIC_CONFIG"
        )]
        config: PathBuf,
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        report: PathBuf,
        #[arg(long)]
        execute: bool,
    },
    /// Inspect a previously submitted native order, including receipt-based settlement.
    AgenticInspect {
        #[arg(
            long,
            default_value = ".flow-bnb/agentic.json",
            env = "FLOW_BNB_AGENTIC_CONFIG"
        )]
        config: PathBuf,
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        order_id: String,
        #[arg(long)]
        report: PathBuf,
    },
    /// Resume read-only order and settlement tracking; never submits another order.
    AgenticTrack {
        #[arg(
            long,
            default_value = ".flow-bnb/agentic.json",
            env = "FLOW_BNB_AGENTIC_CONFIG"
        )]
        config: PathBuf,
        #[arg(long)]
        report: PathBuf,
    },
    /// Inspect and execute one queued intent after fresh preparation and terminal confirmation.
    ApproveTrade {
        #[arg(long, env = "FLOW_BNB_HANDOFF_DIR")]
        handoff_dir: PathBuf,
        #[arg(long)]
        intent_id: String,
        #[arg(long)]
        policy: PathBuf,
        /// Configuration for the bundled local development-node wallet adapter.
        #[arg(long, required_unless_present = "signer", conflicts_with_all = ["signer", "rpc_url"])]
        wallet_config: Option<PathBuf>,
        /// Trusted external wallet adapter implementing the signer protocol.
        #[arg(
            long,
            required_unless_present = "wallet_config",
            requires = "rpc_url",
            conflicts_with = "demo"
        )]
        signer: Option<PathBuf>,
        /// Read-only settlement RPC for an external wallet adapter.
        #[arg(long, requires = "signer")]
        rpc_url: Option<String>,
        /// Use deterministic API fixtures; wallet endpoint must still be a loopback dev node.
        #[arg(long)]
        demo: bool,
    },
    /// Review a queued intent and its durable status without executing it.
    ReviewTrade {
        #[arg(long, env = "FLOW_BNB_HANDOFF_DIR")]
        handoff_dir: PathBuf,
        #[arg(long)]
        intent_id: String,
    },
    /// Read-only wallet, quote, policy, approval and simulation diagnosis.
    PrepareTrade {
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        policy: PathBuf,
        #[arg(long)]
        report: PathBuf,
    },
    /// Prepare anew, confirm on the terminal, then hand off one transaction to a trusted wallet.
    ExecuteTrade {
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        policy: PathBuf,
        #[arg(long)]
        report: PathBuf,
        #[arg(long)]
        signer: PathBuf,
        #[arg(long, env = "FLOW_BNB_RPC_URL")]
        rpc_url: String,
    },
    /// Track an already-broadcast transaction through read-only JSON-RPC, without Binance credentials.
    WatchTransaction {
        #[command(flatten)]
        options: WatchOptions,
        /// Also persist the redacted JSON result to this file (created only if absent).
        #[arg(long)]
        report: Option<PathBuf>,
    },
    /// Parse and compile a workflow without sending network requests.
    Check { file: PathBuf },
    /// Execute a workflow against the signed Binance Web3 API.
    Run {
        file: PathBuf,
        /// Bind a flow input. JSON values retain their type; other values are strings.
        #[arg(long = "input", value_name = "NAME=VALUE")]
        inputs: Vec<String>,
        /// Bind a flow input from an environment variable without exposing it in argv.
        #[arg(long = "env", value_name = "NAME=ENV_VAR")]
        env_inputs: Vec<String>,
        #[arg(long, default_value_t = 15_000)]
        timeout_ms: u64,
        #[arg(long, default_value = "BINANCE_WEB3_API_KEY")]
        api_key_env: String,
        #[arg(long, default_value = "BINANCE_WEB3_SECRET_KEY")]
        secret_key_env: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.verbose {
        // Restrict debug output to our layers; wire-level dependency logs may contain credentials.
        tracing_subscriber::fmt()
            .with_env_filter("warn,postman_flow=debug,flow_bnb=debug")
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .try_init()
            .map_err(|_| anyhow::anyhow!("could not initialize verbose logging"))?;
    }
    match cli.command {
        Command::StrategyAuthorize {
            file,
            config,
            id,
            inputs,
            max_orders,
            max_total_sell_amount,
            valid_for_minutes,
            cooldown_seconds,
        } => {
            let c = flow_bnb::agentic::Config::read(&config)?;
            let snapshot = load_strategy(&file, inputs)?;
            let limits = flow_bnb::autonomy::Limits {
                max_orders,
                max_total_sell_amount,
                valid_for_minutes,
                cooldown_seconds,
            };
            let result = flow_bnb::autonomy::authorize(c, id, snapshot, limits).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            eprintln!("策略授权已创建。授权范围内无需逐笔 CONFIRM；修改策略或额度须重新授权。授权不会启动定时任务。");
            Ok(())
        }
        Command::StrategyAuto {
            config,
            authorization_id,
            request_id,
        } => {
            let c = flow_bnb::agentic::Config::read(&config)?;
            let result = flow_bnb::autonomy::execute(c, authorization_id, request_id).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            anyhow::ensure!(
                matches!(
                    result["state"].as_str(),
                    Some("completed" | "not_triggered")
                ),
                "automatic execution needs attention; inspect existing record, do not replay"
            );
            Ok(())
        }
        Command::StrategyAuthorization { config, id } => {
            let c = flow_bnb::agentic::Config::read(&config)?;
            let result = match id {
                Some(id) => flow_bnb::autonomy::status(&c, &id)?,
                None => flow_bnb::autonomy::list(&c)?,
            };
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        Command::StrategyRevoke { config, id } => {
            let c = flow_bnb::agentic::Config::read(&config)?;
            println!("{}", flow_bnb::autonomy::revoke(&c, &id)?);
            Ok(())
        }
        Command::StrategyRun {
            file,
            config,
            inputs,
            enqueue,
        } => {
            let c = flow_bnb::agentic::Config::read(&config)?;
            let snapshot = load_strategy(&file, inputs)?;
            let result = match enqueue {
                Some(id) => {
                    flow_bnb::agentic_handoff::Inbox::open(c)?
                        .enqueue_strategy(id, snapshot)
                        .await?
                }
                None => serde_json::to_value(flow_bnb::strategy::run(&c, &snapshot).await?)?,
            };
            println!("{}", serde_json::to_string_pretty(&result)?);
            anyhow::ensure!(
                result["success"] != false && result["state"] != "strategy_failed",
                "strategy evaluation failed; no intent queued"
            );
            Ok(())
        }
        Command::Setup {
            config,
            no_login,
            no_open,
        } => flow_bnb::setup::setup(&config, no_login, no_open).await,
        Command::Doctor { config } => flow_bnb::setup::doctor(&config).await,
        Command::Mcp { root } => {
            use rmcp::ServiceExt;
            let server = flow_bnb::FlowBnbMcpServer::new(root).map_err(anyhow::Error::msg)?;
            let service = server.serve(rmcp::transport::stdio()).await?;
            service.waiting().await?;
            Ok(())
        }
        Command::AgenticOperator {
            config,
            intent_id,
            watch,
        } => {
            let inbox =
                flow_bnb::agentic_handoff::Inbox::open(flow_bnb::agentic::Config::read(&config)?)?;
            if let Some(id) = intent_id {
                let status = inbox.approve(&id).await?;
                println!("{}", serde_json::to_string_pretty(&status)?);
                anyhow::ensure!(
                    operator_finished(&status),
                    "operator stopped; inspect this intent, do not resubmit"
                );
            } else if watch {
                eprintln!("Waiting for queued native trades. Each order requires CONFIRM in this terminal. Ctrl-C stops the operator; queued intents remain durable.");
                loop {
                    if let Some(id) = inbox.pending()?.first() {
                        let status = inbox.approve(id).await?;
                        println!("{}", serde_json::to_string_pretty(&status)?);
                        anyhow::ensure!(
                            operator_finished(&status),
                            "operator stopped; inspect this intent, do not resubmit"
                        );
                        anyhow::ensure!(status["state"] != "settled_with_discrepancy", "settled with discrepancy; operator paused for review, no automatic retry");
                    } else {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
            }
            Ok(())
        }
        Command::AgenticExecution {
            config,
            intent_id,
            refresh,
        } => {
            let inbox =
                flow_bnb::agentic_handoff::Inbox::open(flow_bnb::agentic::Config::read(&config)?)?;
            let status = if refresh {
                inbox.refresh(&intent_id).await?
            } else {
                inbox.status(&intent_id)?
            };
            println!("{}", serde_json::to_string_pretty(&status)?);
            Ok(())
        }
        Command::AgenticTrade {
            config,
            request,
            report,
            execute,
        } => {
            let c = flow_bnb::agentic::Config::read(&config)?;
            let i = serde_json::from_slice(&fs::read(request)?)?;
            let r = flow_bnb::agentic::run(c, i, &report, execute).await?;
            print_agentic_report(&r)?;
            anyhow::ensure!(
                r.has_settlement() || matches!(r.state.as_str(), "ready" | "cancelled"),
                "Agentic Wallet workflow stopped; inspect report"
            );
            Ok(())
        }
        Command::AgenticInspect {
            config,
            request,
            order_id,
            report,
        } => {
            let c = flow_bnb::agentic::Config::read(&config)?;
            let i = serde_json::from_slice(&fs::read(request)?)?;
            let r = flow_bnb::agentic::inspect(c, i, order_id, &report).await?;
            print_agentic_report(&r)?;
            anyhow::ensure!(
                r.has_settlement(),
                "existing order not verified; inspect report"
            );
            Ok(())
        }
        Command::AgenticTrack { config, report } => {
            let r = flow_bnb::agentic::track(flow_bnb::agentic::Config::read(&config)?, &report)
                .await?;
            print_agentic_report(&r)?;
            anyhow::ensure!(
                r.has_settlement(),
                "order not verified; inspect report; do not resubmit"
            );
            Ok(())
        }
        Command::ReviewTrade {
            handoff_dir,
            intent_id,
        } => {
            let inbox = flow_bnb::handoff::Inbox::open(handoff_dir)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"intent":inbox.intent(&intent_id)?,"status":inbox.status(&intent_id)?})
                )?
            );
            Ok(())
        }
        Command::ApproveTrade {
            handoff_dir,
            intent_id,
            policy,
            wallet_config,
            signer,
            rpc_url,
            demo,
        } => {
            let external_wallet = signer.is_some();
            let (executable, rpc_url, args) = if let Some(signer) = signer {
                anyhow::ensure!(
                    signer.is_absolute() && signer.is_file(),
                    "signer must be an absolute executable file path"
                );
                let rpc_url = rpc_url.context("external signer requires --rpc-url")?;
                let url = url::Url::parse(&rpc_url).context("invalid settlement RPC URL")?;
                anyhow::ensure!(
                    matches!(url.scheme(), "http" | "https"),
                    "settlement RPC must be HTTP(S)"
                );
                (signer, rpc_url, vec![])
            } else {
                let wallet_config =
                    wallet_config.context("development wallet requires --wallet-config")?;
                let config: flow_bnb::wallet::DevWalletConfig =
                    serde_json::from_slice(&fs::read(&wallet_config)?)?;
                config.validate()?;
                let executable = std::env::current_exe()?.with_file_name("flow-bnb-wallet-rpc");
                anyhow::ensure!(
                    executable.is_file(),
                    "build flow-bnb-wallet-rpc before approval"
                );
                let args = vec![
                    "--config".into(),
                    wallet_config.canonicalize()?.to_string_lossy().into_owned(),
                ];
                (executable, config.rpc_url, args)
            };
            // Check operator terminal and trusted adapter BEFORE permanently claiming the intent.
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .context("approval requires an operator terminal")?;
            let inbox = flow_bnb::handoff::Inbox::open(handoff_dir)?;
            let mut claim = inbox.claim(&intent_id)?;
            let request = claim.intent.request.clone();
            let audit_path = claim.audit_path.clone();
            let result = trade_request_command(
                request,
                policy,
                audit_path.clone(),
                Some((executable, rpc_url, args)),
                demo,
                Some(&mut claim),
            )
            .await;
            if result.is_err() {
                // The durable journal is written BEFORE wallet handoff. An unreadable
                // audit after a disk/process failure must never imply no submission.
                let phase = inbox
                    .status(&intent_id)
                    .map(|s| s.state)
                    .unwrap_or_default();
                let state = if matches!(phase.as_str(), "preparing" | "awaiting_confirmation") {
                    "not_submitted"
                } else {
                    "needs_attention_outcome_may_be_unknown"
                };
                claim.record(state,"Execution did not complete; inspect the audit. A claimed intent cannot be replayed.")?;
            } else {
                let state = if demo {
                    "completed_simulation"
                } else if external_wallet {
                    "completed_wallet"
                } else {
                    "completed_dev_node"
                };
                claim.record(state, "Wallet submission and settlement checks completed; inspect the audit for the action kind and chain evidence.")?;
            }
            result
        }
        Command::PrepareTrade {
            request,
            policy,
            report,
        } => trade_command(request, policy, report, None).await,
        Command::ExecuteTrade {
            request,
            policy,
            report,
            signer,
            rpc_url,
        } => trade_command(request, policy, report, Some((signer, rpc_url))).await,
        Command::WatchTransaction { options, report } => {
            let result = watch_transaction(options).await?;
            let json = serde_json::to_string_pretty(&result)?;
            println!("{json}");
            if let Some(path) = report {
                use std::io::Write;
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .context("cannot create receipt report (existing files are not overwritten)")?;
                writeln!(file, "{json}")?;
                file.sync_all()?;
            }
            if !result.success {
                bail!("transaction tracking ended with {:?}", result.outcome);
            }
            Ok(())
        }
        Command::Check { file } => {
            let plan = load_plan(&file)?;
            println!("OK: {} ({} steps)", plan.name(), plan.step_count());
            Ok(())
        }
        Command::Run {
            file,
            inputs,
            env_inputs,
            timeout_ms,
            api_key_env,
            secret_key_env,
        } => {
            let plan = load_plan(&file)?;
            let inputs = parse_inputs(inputs, env_inputs)?;
            let api_key = required_env(&api_key_env)?;
            let secret_key = required_env(&secret_key_env)?;
            let transport = BinanceWeb3Transport::new(api_key, secret_key)?;
            let session =
                FlowSessionEnvironment::new(inputs).with_request_options(RequestOptions {
                    timeout_ms: Some(timeout_ms),
                    ..RequestOptions::default()
                });
            let events = execute_flow(plan, transport, session)?;
            let mut events = std::pin::pin!(events);
            let mut succeeded = false;

            while let Some(event) = events.next().await {
                let event = event?;
                println!("{event:?}");
                if let FlowEvent::FlowFinished { success, .. } = event {
                    succeeded = success;
                }
            }

            if !succeeded {
                bail!("flow failed; inspect the events above");
            }
            Ok(())
        }
    }
}

fn operator_finished(status: &serde_json::Value) -> bool {
    matches!(
        status["state"].as_str(),
        Some("completed" | "settled_with_discrepancy" | "cancelled")
    )
}

fn print_agentic_report(r: &flow_bnb::agentic::Report) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(r)?);
    if r.state == "settled_with_discrepancy" {
        eprintln!("Order settled with an amount discrepancy; review settlement. Do not resubmit. Any existing submission lock is retained.");
    }
    Ok(())
}

fn load_plan(path: &PathBuf) -> Result<postman_flow::FlowPlan> {
    let source =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let document =
        parse_flow_yaml(&source).map_err(|error| anyhow::anyhow!("{}: {error}", path.display()))?;
    compile_flow(
        &document.flow,
        &document.apis,
        &CompileEnvironment::default(),
    )
    .map_err(|errors| {
        anyhow::anyhow!(
            "{}",
            errors
                .iter()
                .map(|error| format!("{}: {error}", path.display()))
                .collect::<Vec<_>>()
                .join("\n")
        )
    })
}

fn parse_inputs(values: Vec<String>, env_values: Vec<String>) -> Result<FlowInputs> {
    let mut inputs = FlowInputs::new();
    let mut names = HashSet::new();

    for value in values {
        let (name, raw) = split_binding(&value, "--input")?;
        insert_input(&mut inputs, &mut names, name, parse_json_or_string(raw))?;
    }
    for value in env_values {
        let (name, env_name) = split_binding(&value, "--env")?;
        let raw = required_env(env_name)?;
        insert_input(&mut inputs, &mut names, name, parse_json_or_string(&raw))?;
    }

    Ok(inputs)
}

fn split_binding<'a>(binding: &'a str, option: &str) -> Result<(&'a str, &'a str)> {
    let (name, value) = binding
        .split_once('=')
        .with_context(|| format!("{option} requires NAME=VALUE"))?;
    if name.is_empty() || value.is_empty() {
        bail!("{option} requires nonempty NAME=VALUE");
    }
    Ok((name, value))
}

fn insert_input(
    inputs: &mut FlowInputs,
    names: &mut HashSet<String>,
    name: &str,
    value: serde_json::Value,
) -> Result<()> {
    if !names.insert(name.to_owned()) {
        bail!("input '{name}' was provided more than once");
    }
    inputs.insert(name, value);
    Ok(())
}

fn parse_json_or_string(value: &str) -> serde_json::Value {
    serde_json::from_str(value).unwrap_or_else(|_| serde_json::Value::String(value.to_owned()))
}

fn required_env(name: &str) -> Result<String> {
    env::var(name).with_context(|| format!("required environment variable {name} is not set"))
}

async fn trade_command(
    request_path: PathBuf,
    policy_path: PathBuf,
    report_path: PathBuf,
    execution: Option<(PathBuf, String)>,
) -> Result<()> {
    let request = serde_json::from_slice(&fs::read(request_path)?)?;
    trade_request_command(
        request,
        policy_path,
        report_path,
        execution.map(|(path, url)| (path, url, vec![])),
        false,
        None,
    )
    .await
}

type WalletExecution = (PathBuf, String, Vec<String>);
async fn trade_request_command(
    request: flow_bnb::trade::TradeRequest,
    policy_path: PathBuf,
    report_path: PathBuf,
    execution: Option<WalletExecution>,
    demo: bool,
    mut claim: Option<&mut flow_bnb::handoff::Claim>,
) -> Result<()> {
    use flow_bnb::trade::{
        invoke_signer_with_args, prepare_with_transport, verify_settlement, ExecutionPolicy,
    };
    use std::io::{BufRead, Seek, SeekFrom, Write};
    let policy: ExecutionPolicy = serde_json::from_slice(&fs::read(policy_path)?)?;
    // Reserve the audit destination before network I/O or wallet handoff.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&report_path)
        .context("report already exists or cannot be created")?;
    let api = if demo {
        println!("SIMULATION ONLY: fixture market data and local development wallet; no mainnet evidence.");
        TradeTransport::Demo(flow_bnb::demo::DemoApi::default())
    } else {
        let api = BinanceWeb3Transport::new(
            required_env("BINANCE_WEB3_API_KEY")?,
            required_env("BINANCE_WEB3_SECRET_KEY")?,
        )?;
        let rpc_url = execution
            .as_ref()
            .map(|(_, url, _)| url.clone())
            .or_else(|| env::var("FLOW_BNB_RPC_URL").ok());
        TradeTransport::Live(flow_bnb::trade::BalanceFallback {
            api,
            rpc: postman_request::RequestClient::try_new("flow-bnb-balance")?,
            rpc_url,
        })
    };
    let prepared = prepare_with_transport(request, policy, api.clone()).await;
    let report = prepared.report().clone();
    let mut audit = serde_json::json!({"mode":if demo {"simulation"} else {"live_api"},"preparation":report,"execution":"not_requested"});
    fn save(file: &mut fs::File, audit: &serde_json::Value) -> Result<()> {
        file.seek(SeekFrom::Start(0))?;
        serde_json::to_writer_pretty(&mut *file, audit)?;
        let length = file.stream_position()?;
        file.set_len(length)?;
        file.sync_all()?;
        Ok(())
    }
    save(&mut file, &audit)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if let Some((signer, rpc_url, signer_args)) = execution {
        if !prepared.ready() {
            bail!("preparation blocked; no signer was invoked");
        }
        if let Some(claim) = claim.as_deref_mut() {
            claim.record("awaiting_confirmation","Fresh preparation completed; operator must confirm the exact action before it expires.")?;
        }
        // No --yes flag or MCP boolean: confirmation must come from the operator's terminal.
        let mut terminal = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .context("execution requires an interactive operator terminal")?;
        writeln!(terminal,"Review the transaction above. Type its complete confirmation_id to submit this ONE transaction:")?;
        terminal.flush()?;
        let mut confirmation = String::new();
        std::io::BufReader::new(terminal).read_line(&mut confirmation)?;
        let signer_request = prepared.authorize(confirmation.trim())?;
        audit["execution"] = serde_json::json!("handoff_started_outcome_unknown_until_verified");
        save(&mut file, &audit)?;
        if let Some(claim) = claim.as_deref_mut() {
            claim.record(
                "handoff_started_outcome_unknown",
                "Operator confirmed; wallet submission may occur. Do not replay this intent.",
            )?;
        }
        let response = match invoke_signer_with_args(&signer, &signer_args, &signer_request).await {
            Ok(response) => response,
            Err(error) => {
                audit["execution"] = serde_json::json!("handoff_failed_or_unknown");
                audit["error"] = serde_json::json!(error.to_string());
                save(&mut file, &audit)?;
                return Err(error);
            }
        };
        audit["tx_hash"] = serde_json::json!(response.tx_hash);
        save(&mut file, &audit)?;
        if let Some(claim) = claim {
            claim.record(
                "submitted_verifying",
                "Wallet returned a transaction hash; checking payload and settlement.",
            )?;
        }
        let rpc = postman_request::RequestClient::try_new("flow-bnb-settlement")?;
        let settlement =
            verify_settlement(&report, &signer_request, &response, &rpc_url, api, rpc).await;
        audit["execution"] = serde_json::json!(settlement.state);
        audit["settlement"] = serde_json::to_value(&settlement)?;
        save(&mut file, &audit)?;
        println!("{}", serde_json::to_string_pretty(&settlement)?);
        if !settlement.errors.is_empty() {
            bail!("settlement needs attention; inspect report before retrying");
        }
    } else if !report.blockers.is_empty() {
        bail!("preparation blocked; see report");
    }
    Ok(())
}

#[derive(Clone)]
enum TradeTransport {
    Live(flow_bnb::trade::BalanceFallback<BinanceWeb3Transport, postman_request::RequestClient>),
    Demo(flow_bnb::demo::DemoApi),
}
impl postman_http::HttpTransport for TradeTransport {
    async fn execute(
        &self,
        request: postman_http::request::Request,
        options: RequestOptions,
    ) -> Result<postman_http::HttpResponse, postman_http::HttpError> {
        match self {
            Self::Live(t) => t.execute(request, options).await,
            Self::Demo(t) => t.execute(request, options).await,
        }
    }
}

fn load_strategy(
    file: &std::path::Path,
    inputs: Vec<String>,
) -> Result<flow_bnb::strategy::Snapshot> {
    anyhow::ensure!(
        fs::metadata(file)?.len() <= 65_536,
        "strategy exceeds 64 KiB"
    );
    let source = fs::read_to_string(file)?;
    let mut bindings = std::collections::BTreeMap::new();
    for value in inputs {
        let (name, raw) = split_binding(&value, "--input")?;
        anyhow::ensure!(
            bindings
                .insert(name.to_owned(), parse_json_or_string(raw))
                .is_none(),
            "duplicate strategy input"
        );
    }
    flow_bnb::strategy::Snapshot::new(&source, bindings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_requires_exactly_one_wallet_and_external_rpc() {
        let base = [
            "flow-bnb",
            "approve-trade",
            "--handoff-dir",
            "/tmp/inbox",
            "--intent-id",
            "test",
            "--policy",
            "policy.json",
        ];
        for extra in [
            vec!["--wallet-config", "dev.json"],
            vec!["--wallet-config", "dev.json", "--demo"],
            vec![
                "--signer",
                "/tmp/external-signer",
                "--rpc-url",
                "https://rpc.example",
            ],
        ] {
            assert!(Cli::try_parse_from(base.iter().copied().chain(extra)).is_ok());
        }
        for extra in [
            vec![],
            vec!["--signer", "/tmp/external-signer"],
            vec!["--rpc-url", "https://rpc.example"],
            vec![
                "--wallet-config",
                "dev.json",
                "--signer",
                "/tmp/external-signer",
                "--rpc-url",
                "https://rpc.example",
            ],
            vec![
                "--signer",
                "/tmp/external-signer",
                "--rpc-url",
                "https://rpc.example",
                "--demo",
            ],
        ] {
            assert!(Cli::try_parse_from(base.iter().copied().chain(extra)).is_err());
        }
    }

    #[test]
    fn verbose_is_accepted_before_and_after_subcommands() {
        for args in [
            vec!["flow-bnb", "-v", "run", "flows/rwa_discovery.http.yml"],
            vec![
                "flow-bnb",
                "run",
                "flows/rwa_discovery.http.yml",
                "--verbose",
            ],
            vec!["flow-bnb", "check", "flows/rwa_discovery.http.yml", "-v"],
        ] {
            assert!(Cli::try_parse_from(args).unwrap().verbose);
        }
        assert!(
            !Cli::try_parse_from(["flow-bnb", "check", "flows/rwa_discovery.http.yml"])
                .unwrap()
                .verbose
        );
    }
}
