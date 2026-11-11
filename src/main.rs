use std::io::{ErrorKind, Read, Write};
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::process::ExitCode;

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;
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
    #[cfg(unix)]
    #[command(
        about = "Run scripts that drive the screen toward goals and check what it shows; each file is a script, or stdin is"
    )]
    Act {
        scripts: Vec<PathBuf>,
        #[arg(
            long,
            requires = "scripts",
            help = "Never ask the decision model: replay the lock files and fail where the screen changed"
        )]
        locked: bool,
        #[arg(long, help = "Show the browser window instead of running it headless")]
        headful: bool,
    },
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
        #[cfg(unix)]
        Command::Act {
            scripts,
            locked,
            headful,
        } => return act(&scripts, locked, headful).await,
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

#[cfg(unix)]
async fn act(scripts: &[PathBuf], locked: bool, headful: bool) -> Result<ExitCode> {
    let paths: Vec<Option<&Path>> = if scripts.is_empty() {
        vec![None]
    } else {
        scripts.iter().map(|path| Some(path.as_path())).collect()
    };
    let mut exit_code = 0;
    for path in paths {
        let lock = path.map(|path| {
            let mut lock = path.as_os_str().to_owned();
            lock.push(".lock");
            PathBuf::from(lock)
        });
        let name = path.map_or("-".into(), |path| path.display().to_string());
        let outcome = async {
            let text = match path {
                Some(path) => std::fs::read_to_string(path)
                    .with_context(|| format!("cannot read {}", path.display()))?,
                None => read_stdin()?,
            };
            commands::act(Client::from_env(), &text, lock.as_deref(), locked, headful).await
        };
        let mut report = match outcome.await {
            Ok(outcome) => {
                exit_code = exit_code.max(outcome.code);
                outcome.report
            }
            Err(error) => {
                exit_code = 2;
                serde_json::json!({ "status": "error", "error": format!("{error:#}") })
            }
        };
        report["script"] = name.into();
        match writeln!(std::io::stdout(), "{report}") {
            Err(error) if error.kind() == ErrorKind::BrokenPipe => break,
            written => written?,
        }
    }
    Ok(ExitCode::from(exit_code))
}

fn read_stdin() -> Result<String> {
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text)?;
    Ok(text)
}
