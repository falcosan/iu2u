use anyhow::{Context, Result};
use serde_json::Value;

use super::{context, data_path, permission};
use crate::model::{Client, Question, Questions};

const DONE_AT: f64 = 0.7;
const UNVERIFIED_BELOW: f64 = 0.3;
const SEND_BACK: &str = "iu2u proof: you said the work is done without reporting a check that proves it. Run the tests or commands that show it works and report their results, or say plainly what could not be verified and why.";

pub(super) async fn run(input: &Value, client: &Client) -> Result<String> {
    if input["tool_name"] == "SubagentHandback" {
        return handback(input, client).await;
    }
    let judged_at_handback = input["agent_id"].as_str().is_some_and(|agent| {
        data_path("proof", agent).is_ok_and(|marker| std::fs::remove_file(marker).is_ok())
    });
    if input["stop_hook_active"].as_bool() == Some(true) || judged_at_handback {
        return Ok(String::new());
    }
    let message = input["last_assistant_message"]
        .as_str()
        .context("no last message")?;
    if !done_without_proof(message, client).await? {
        return Ok(String::new());
    }
    Ok(context(&input["hook_event_name"], SEND_BACK))
}

async fn handback(input: &Value, client: &Client) -> Result<String> {
    let report = input
        .pointer("/tool_input/message")
        .and_then(Value::as_str)
        .context("no report")?;
    let marker = data_path("proof", input["agent_id"].as_str().context("no agent_id")?)?;
    if marker.exists() {
        return Ok(String::new());
    }
    std::fs::File::create(&marker)?;
    if !done_without_proof(report, client).await? {
        return Ok(String::new());
    }
    Ok(permission("deny", SEND_BACK))
}

async fn done_without_proof(message: &str, client: &Client) -> Result<bool> {
    let questions = Questions::from([
        (
            "finished".into(),
            Question::yesno(
                "The message announces that requested work on code, files or systems has been completed or fixed.",
            ),
        ),
        (
            "verified".into(),
            Question::yesno(
                "The message quotes concrete results of checks the assistant ran, such as test counts, exit codes, build or command output, or says plainly what could not be verified and why.",
            ),
        ),
    ]);
    let answers = client.ask(message, &questions).await?;
    let finished = answers["finished"].yes().unwrap_or(0.0);
    let verified = answers["verified"].yes().unwrap_or(1.0);
    Ok(finished >= DONE_AT && verified < UNVERIFIED_BELOW)
}
