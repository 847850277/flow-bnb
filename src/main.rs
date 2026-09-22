use std::{collections::HashSet, env, fs, path::PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use flow_bnb::BinanceWeb3Transport;
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
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
    match Cli::parse().command {
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
