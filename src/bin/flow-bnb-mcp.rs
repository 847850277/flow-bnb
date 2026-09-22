use std::{env, path::PathBuf, process::ExitCode};

use flow_bnb::FlowBnbMcpServer;
use rmcp::{transport::stdio, ServiceExt};

const USAGE: &str = "Usage: flow-bnb-mcp [--root DIRECTORY]";

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

async fn run() -> Result<(), String> {
    let root = parse_arguments(env::args().skip(1))?;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();
    let service = FlowBnbMcpServer::new(root)?
        .serve(stdio())
        .await
        .map_err(|error| format!("cannot start MCP stdio server: {error}"))?;
    service
        .waiting()
        .await
        .map_err(|error| format!("MCP server stopped with an error: {error}"))?;
    Ok(())
}

fn parse_arguments(arguments: impl IntoIterator<Item = String>) -> Result<PathBuf, String> {
    let mut arguments = arguments.into_iter();
    let mut root = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--root" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--root requires a directory".to_owned())?;
                root = Some(PathBuf::from(value));
            }
            "-h" | "--help" => return Err(USAGE.to_owned()),
            unknown => return Err(format!("unknown argument `{unknown}`\n{USAGE}")),
        }
    }
    root.map_or_else(
        || env::current_dir().map_err(|error| format!("cannot read current directory: {error}")),
        Ok,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_root() {
        assert_eq!(
            parse_arguments(["--root".to_owned(), "/tmp/flow-bnb".to_owned()]).unwrap(),
            PathBuf::from("/tmp/flow-bnb")
        );
    }
}
