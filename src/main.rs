use std::io::{ErrorKind, Read, Write};
use std::pin::pin;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use futures_util::{StreamExt, stream};
use iu2u::{commands, hooks::Power, mcp::Server, model::Client};
use rmcp::{ServiceExt, transport::stdio};
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Serve the tools to an agent over MCP on stdio")]
    Mcp,
    #[command(about = "Run a hook power on the hook JSON read from stdin")]
    Hook { power: Power },
    #[command(about = "Print the stdin lines for which the statement is likely true")]
    Filter {
        statement: String,
        #[arg(long, default_value_t = 0.5, value_parser = probability)]
        at: f64,
    },
    #[command(about = "Pick the next UI action from {goal, screen, actions} JSON on stdin")]
    Next,
}

fn probability(text: &str) -> Result<f64, String> {
    let value: f64 = text.parse().map_err(|error| format!("{error}"))?;
    if (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err("must be between 0 and 1".into())
    }
}

fn main() -> Result<ExitCode> {
    let command = match Cli::try_parse() {
        Ok(cli) => cli.command,
        Err(error)
            if error.use_stderr()
                && std::env::args_os().nth(1).is_some_and(|arg| arg == "hook") =>
        {
            error.print()?;
            return Ok(ExitCode::FAILURE);
        }
        Err(error) => error.exit(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let exit_code = runtime.block_on(run(command));
    runtime.shutdown_background();
    exit_code
}

async fn run(command: Command) -> Result<ExitCode> {
    match command {
        Command::Mcp => {
            Server::new(Client::from_env()?)
                .serve(stdio())
                .await?
                .waiting()
                .await?;
        }
        Command::Hook { power } => {
            let text = read_stdin()?;
            if power.enabled() {
                let input = serde_json::from_str(&text).unwrap_or_default();
                print!("{}", power.run(&input, Client::from_env()).await?);
            }
        }
        Command::Filter { statement, at } => {
            return Ok(match filter(&statement, at).await {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::from(1),
                Err(error) => {
                    eprintln!("Error: {error:#}");
                    ExitCode::from(2)
                }
            });
        }
        Command::Next => {
            let request =
                serde_json::from_str(&read_stdin()?).context("expected {goal, screen, actions}")?;
            println!("{}", commands::next(&Client::from_env()?, request).await?);
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn filter(statement: &str, at: f64) -> Result<bool> {
    let client = Client::from_env()?;
    let lines = stream::unfold(
        BufReader::new(tokio::io::stdin()).split(b'\n'),
        |mut segments| async move {
            let segment = segments.next_segment().await.transpose()?;
            let line = segment.map(|bytes| {
                String::from_utf8_lossy(bytes.strip_suffix(b"\r").unwrap_or(&bytes)).into_owned()
            });
            Some((line, segments))
        },
    );
    let mut kept = pin!(commands::filter(&client, statement, at, lines));
    let mut matched = false;
    while let Some(line) = kept.next().await {
        match writeln!(std::io::stdout(), "{}", line?) {
            Err(error) if error.kind() == ErrorKind::BrokenPipe => return Ok(true),
            written => written?,
        }
        matched = true;
    }
    Ok(matched)
}

fn read_stdin() -> Result<String> {
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text)?;
    Ok(text)
}
