mod client;
mod gate;
mod server;

use std::time::Duration;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use rmcp::{ServiceExt, transport::stdio};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Serve the powers to an agent over MCP on stdio")]
    Mcp(Model),
}

#[derive(Args)]
struct Model {
    #[arg(long, env = "IU2U_BASE_URL", default_value = "http://127.0.0.1:11435")]
    base_url: String,
    #[arg(long, env = "IU2U_MODEL")]
    model: Option<String>,
    #[arg(long, env = "IU2U_API_KEY", hide_env_values = true)]
    api_key: Option<String>,
    #[arg(long, env = "IU2U_TIMEOUT_MS", default_value_t = 10_000)]
    timeout_ms: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let Command::Mcp(model) = Cli::parse().command;
    let client = client::Client::new(
        &model.base_url,
        model.api_key,
        model.model,
        Duration::from_millis(model.timeout_ms),
    )?;
    server::Server::new(client).serve(stdio()).await?.waiting().await?;
    Ok(())
}
