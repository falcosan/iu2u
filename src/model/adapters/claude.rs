use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::process::Command;

use super::{ADAPTER_SESSION, SYSTEM, printed, run};

pub(super) const PROGRAM: &str = "claude";
pub(crate) const MODEL: &str = "haiku";

pub(super) async fn reply(
    model: &str,
    effort: &str,
    timeout: Duration,
    prompt: &str,
    schema: &Value,
) -> Result<Value> {
    let mut command = Command::new(PROGRAM);
    command
        .args([
            "--print",
            "--model",
            model,
            "--effort",
            effort,
            "--system-prompt",
            SYSTEM,
            "--json-schema",
            &schema.to_string(),
            "--output-format",
            "json",
            "--tools",
            "",
            "--setting-sources",
            "",
            "--strict-mcp-config",
            "--no-session-persistence",
        ])
        .env_remove("CLAUDE_CODE_EFFORT_LEVEL")
        .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
        .env(ADAPTER_SESSION, "1");
    let output = run(command, prompt, timeout).await?;
    let result: Value = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("claude failed: {}", printed(&output.stderr)))?;
    if result["is_error"] != false {
        bail!(
            "claude failed: {}",
            printed(result["result"].as_str().unwrap_or_default().as_bytes())
        );
    }
    result
        .get("structured_output")
        .cloned()
        .context("claude returned no answers")
}
