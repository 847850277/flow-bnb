use anyhow::{Context, Result};
use clap::Parser;
use std::{io::Read, path::PathBuf};
#[derive(Parser)]
#[command(about = "Local development-node wallet adapter; never reads private keys")]
struct Args {
    #[arg(long)]
    config: PathBuf,
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config: flow_bnb::wallet::DevWalletConfig =
        serde_json::from_slice(&std::fs::read(args.config)?)?;
    config.validate()?;
    let mut input = Vec::new();
    std::io::stdin().take(262145).read_to_end(&mut input)?;
    anyhow::ensure!(input.len() <= 262144, "signer request too large");
    let request = serde_json::from_slice(&input).context("invalid signer request")?;
    let rpc = postman_request::RequestClient::try_new("flow-bnb-dev-wallet")?;
    let response = flow_bnb::wallet::send_with_transport(&config, &request, rpc).await?;
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}
