mod compass;
mod guard;
mod proof;
mod prune;
mod trim;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::ValueEnum;
use serde_json::{Value, json};

use crate::model::{Client, setting};

const ITEM_CHARS: usize = 300;
const KEPT_FOR: Duration = Duration::from_secs(24 * 60 * 60);
const JUDGE_WITHIN: Duration = Duration::from_secs(12);

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Power {
    Guard,
    Trim,
    Proof,
    Compass,
    Prune,
}

impl Power {
    pub fn enabled(self) -> bool {
        let name = format!("{self:?}").to_uppercase();
        match setting(&name).as_deref() {
            Some("true") => true,
            Some("false") => false,
            _ => matches!(self, Self::Guard | Self::Proof),
        }
    }

    pub async fn run(self, input: &Value, client: Result<Client>) -> Result<String> {
        let output = match (self, client) {
            (Self::Guard, client) => return Ok(guard::run(input, client).await),
            (_, Err(error)) => return Err(error),
            (Self::Trim, Ok(client)) => trim::run(input, &client).await,
            (Self::Proof, Ok(client)) => within_hook_timeout(proof::run(input, &client)).await,
            (Self::Compass, Ok(client)) => within_hook_timeout(compass::run(input, &client)).await,
            (Self::Prune, Ok(client)) => prune::run(input, &client).await,
        };
        Ok(output.unwrap_or_else(|error| {
            eprintln!("iu2u {self:?}: {error:#}");
            String::new()
        }))
    }
}

fn data_path(folder: &str, id: &str) -> Result<PathBuf> {
    let dir = Path::new(&std::env::var_os("CLAUDE_PLUGIN_DATA").context("no CLAUDE_PLUGIN_DATA")?)
        .join(folder);
    std::fs::create_dir_all(&dir)?;
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let expired = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified.elapsed().unwrap_or_default() > KEPT_FOR);
        if expired {
            std::fs::remove_file(entry.path()).ok();
        }
    }
    let name: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    Ok(dir.join(name))
}

fn permission(decision: &str, reason: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": decision,
            "permissionDecisionReason": reason
        }
    })
    .to_string()
}

async fn within_hook_timeout<T>(judge: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(JUDGE_WITHIN, judge).await?
}

fn context(event: &Value, text: &str) -> String {
    json!({ "hookSpecificOutput": { "hookEventName": event, "additionalContext": text } })
        .to_string()
}
