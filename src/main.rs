use std::io::{ErrorKind, Write};
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::process::ExitCode;

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;
use clap::{Parser, Subcommand};
use futures_util::{StreamExt, stream};
use iu2u::commands::{self, EXIT_ERROR, EXIT_FAILED};
use iu2u::{config, hooks::Power, mcp::Server, model::Client};
use rmcp::{ServiceExt, transport::stdio};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
#[cfg(unix)]
use tokio::signal::unix::{Signal, SignalKind, signal};

#[cfg(unix)]
const INTERRUPTED: u8 = 130;
#[cfg(unix)]
const TERMINATED: u8 = 143;
#[cfg(unix)]
const HUNG_UP: u8 = 129;

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
    #[command(hide = true)]
    Serve { sha256: String, identity: String },
    #[cfg(unix)]
    #[command(
        about = "Choose the decision model and the hooks, install what the choice needs, and test it"
    )]
    Init {
        #[arg(
            long,
            help = "Ask nothing: keep the configured values, take the defaults, install and test"
        )]
        yes: bool,
    },
    #[cfg(unix)]
    #[command(
        about = "Show the configured model and, for a local one, what is installed and running"
    )]
    Status,
    #[cfg(unix)]
    #[command(about = "Stop every local decision model server of this user")]
    Stop,
    #[command(about = "Show every setting with its value and where it comes from")]
    Config {
        #[command(subcommand)]
        action: Option<Setting>,
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
            help = "Never ask the decision model: replay the lock files and fail where the screen does not match them"
        )]
        locked: bool,
        #[arg(long, help = "Show the browser window instead of running it headless")]
        headful: bool,
        #[arg(
            long,
            help = "Print one JSON report per script instead of the readable results"
        )]
        json: bool,
        #[arg(
            long,
            value_name = "SCRIPT",
            help = "Run this script once before the others; the services it starts and the values it sets last until the last script ends"
        )]
        before: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum Setting {
    #[command(about = "Print the value of a setting")]
    Get { key: String },
    #[command(about = "Write a setting to the config file")]
    Set {
        key: String,
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    #[command(about = "Remove a setting from the config file")]
    Unset { key: String },
    #[command(about = "Print the path of the config file")]
    Path,
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
            Server::from_config()
                .serve(stdio())
                .await?
                .waiting()
                .await?;
        }
        Command::Hook { power } => {
            let text = read_stdin().await?;
            if power.enabled() {
                let input = serde_json::from_str(&text).unwrap_or_default();
                print!("{}", power.run(&input, Client::from_env()).await?);
            }
        }
        Command::Filter { statement, at } => {
            return Ok(match filter(&statement, at).await {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::from(EXIT_FAILED),
                Err(error) => failed(&error),
            });
        }
        #[cfg(unix)]
        Command::Serve { sha256, identity } => iu2u::model::serve(&sha256, &identity).await?,
        #[cfg(unix)]
        Command::Status => print!("{}", iu2u::model::status().await?),
        #[cfg(unix)]
        Command::Stop => match iu2u::model::stop_all().await? {
            0 => {}
            1 => println!("Stopped 1 local server."),
            stopped => println!("Stopped {stopped} local servers."),
        },
        #[cfg(unix)]
        Command::Init { yes } => {
            return Ok(init(yes).await.unwrap_or_else(|error| failed(&error)));
        }
        Command::Config { action } => {
            return Ok(settings(action).unwrap_or_else(|error| failed(&error)));
        }
        #[cfg(unix)]
        Command::Act {
            scripts,
            locked,
            headful,
            json,
            before,
        } => {
            return Ok(act(&scripts, before.as_deref(), locked, headful, json)
                .await
                .unwrap_or_else(|error| failed(&error)));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn failed(error: &anyhow::Error) -> ExitCode {
    writeln!(std::io::stderr(), "Error: {error:#}").ok();
    ExitCode::from(EXIT_ERROR)
}

#[cfg(unix)]
async fn init(yes: bool) -> Result<ExitCode> {
    use std::io::IsTerminal;
    anyhow::ensure!(
        yes || std::io::stdin().is_terminal(),
        "iu2u init asks its questions on a terminal; run iu2u init --yes to keep the configured values and take the defaults"
    );
    commands::init(&mut std::io::stdin().lock(), yes).await?;
    Ok(ExitCode::SUCCESS)
}

fn settings(action: Option<Setting>) -> Result<ExitCode> {
    match action {
        None => print!("{}", config::list()?),
        Some(Setting::Get { key }) => match config::get(&key)? {
            Some(value) => println!("{value}"),
            None => return Ok(ExitCode::from(EXIT_FAILED)),
        },
        Some(Setting::Set { key, value }) => config::set(&key, &value)?,
        Some(Setting::Unset { key }) => config::unset(&key)?,
        Some(Setting::Path) => println!("{}", config::location()?.display()),
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
async fn act(
    scripts: &[PathBuf],
    before: Option<&Path>,
    locked: bool,
    headful: bool,
    json: bool,
) -> Result<ExitCode> {
    let paths: Vec<Option<&Path>> = if scripts.is_empty() {
        vec![None]
    } else {
        scripts.iter().map(|path| Some(path.as_path())).collect()
    };
    let mut reporter = commands::Reporter::new(json);
    let mut suite = commands::Suite::new(locked, headful)?;
    let mut exit_code = 0;
    let runs = before
        .map(|before| (Some(before), true))
        .into_iter()
        .chain(paths.into_iter().map(|path| (path, false)));
    let (mut interrupt, mut terminate, mut hangup) = (
        caught(SignalKind::interrupt())?,
        caught(SignalKind::terminate())?,
        caught(SignalKind::hangup())?,
    );
    let (mut running, mut done) = (None, Vec::new());
    let scripts = async {
        for (path, first) in runs {
            let lock = path.map(|path| {
                let mut lock = path.as_os_str().to_owned();
                lock.push(".lock");
                PathBuf::from(lock)
            });
            let name = path.map_or("-".into(), |path| path.display().to_string());
            reporter.start(&name);
            running = Some(name.clone());
            done.clear();
            let outcome = async {
                let text = match path {
                    Some(path) => std::fs::read_to_string(path)
                        .with_context(|| format!("cannot read {}", path.display()))?,
                    None => read_stdin().await?,
                };
                let base = path
                    .and_then(Path::parent)
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                let outcome = commands::act(
                    Client::from_env(),
                    &text,
                    base,
                    lock.as_deref(),
                    &mut suite,
                    first,
                    |step| {
                        done.push(step.clone());
                        reporter.step(step);
                    },
                )
                .await?;
                anyhow::Ok((text, outcome))
            };
            let (text, mut report) = match outcome.await {
                Ok((text, outcome)) => {
                    exit_code = exit_code.max(outcome.code);
                    (Some(text), outcome.report)
                }
                Err(error) => {
                    exit_code = EXIT_ERROR;
                    let report =
                        serde_json::json!({ "status": "error", "error": format!("{error:#}") });
                    (None, report)
                }
            };
            report["script"] = name.into();
            reporter.finish(&report, text.as_deref());
            running = None;
            if first && exit_code != 0 {
                exit_code = EXIT_ERROR;
                break;
            }
            if reporter.broken() {
                break;
            }
        }
    };
    let stopped = tokio::select! {
        () = scripts => None,
        () = received(&mut interrupt) => Some(INTERRUPTED),
        () = received(&mut terminate) => Some(TERMINATED),
        () = received(&mut hangup) => Some(HUNG_UP),
    };
    if stopped.is_some()
        && let Some(name) = running
    {
        let report = serde_json::json!({ "status": "error", "script": name, "error": "stopped by a signal", "steps": done });
        reporter.finish(&report, None);
    }
    suite.stop().await;
    reporter.summary();
    reporter.close()?;
    Ok(ExitCode::from(stopped.unwrap_or(exit_code)))
}

#[cfg(unix)]
fn caught(kind: SignalKind) -> Result<Option<Signal>> {
    let mut old = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
    let ignored = unsafe {
        libc::sigaction(kind.as_raw_value(), std::ptr::null(), old.as_mut_ptr()) == 0
            && old.assume_init().sa_sigaction == libc::SIG_IGN
    };
    Ok(if ignored { None } else { Some(signal(kind)?) })
}

#[cfg(unix)]
async fn received(signal: &mut Option<Signal>) {
    match signal {
        Some(signal) => {
            signal.recv().await;
        }
        None => std::future::pending().await,
    }
}

async fn read_stdin() -> Result<String> {
    let mut text = String::new();
    tokio::io::stdin().read_to_string(&mut text).await?;
    Ok(text)
}
