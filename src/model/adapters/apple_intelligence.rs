use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::Semaphore;

use super::{Refused, SYSTEM, printed, run};

pub(super) const PROGRAM: &str = "apfel";
const REFUSES_THIS_ITEM: [i32; 2] = [3, 4];

static SCHEMAS: AtomicUsize = AtomicUsize::new(0);
static RUNS_AT_ONCE: Semaphore = Semaphore::const_new(2);

struct Removed(PathBuf);

impl Drop for Removed {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

pub(super) async fn reply(timeout: Duration, prompt: &str, schema: &Value) -> Result<Value> {
    let _turn = RUNS_AT_ONCE.acquire().await?;
    let schema_file = Removed(std::env::temp_dir().join(format!(
        "iu2u-schema-{}-{}.json",
        std::process::id(),
        SCHEMAS.fetch_add(1, Ordering::Relaxed)
    )));
    std::fs::remove_file(&schema_file.0).ok();
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&schema_file.0)
        .and_then(|mut file| file.write_all(schema.to_string().as_bytes()))
        .context("cannot write the schema for apfel")?;
    let mut command = Command::new(PROGRAM);
    command
        .args([
            "--quiet",
            "--system",
            SYSTEM,
            "--temperature",
            "0",
            "--schema",
        ])
        .arg(&schema_file.0);
    for (name, _) in
        std::env::vars_os().filter(|(name, _)| name.to_string_lossy().starts_with("APFEL_"))
    {
        command.env_remove(name);
    }
    let output = run(command, prompt, timeout).await?;
    match output.status.code() {
        Some(0) => {}
        Some(code) if REFUSES_THIS_ITEM.contains(&code) => {
            return Err(anyhow::Error::msg(Refused(format!(
                "apfel refused this item: {}",
                printed(&output.stderr)
            ))));
        }
        _ => bail!("apfel failed: {}", printed(&output.stderr)),
    }
    serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "apfel returned answers that are not JSON: {}",
            printed(&output.stdout)
        )
    })
}
