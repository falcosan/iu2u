use std::collections::VecDeque;
use std::io::{BufRead, BufReader};

use anyhow::{Context, Result};
use serde_json::Value;

use super::ITEM_CHARS;
use crate::clip;
use crate::model::{Client, Question, Questions};

const MAX_STEPS: usize = 150;
const RECENT_REQUESTS: usize = 3;
const KEEP_AT: f64 = 0.5;
const DROP_BELOW: f64 = 0.2;
const MAX_LIST_CHARS: usize = 4_500;

pub(super) async fn run(input: &Value, client: &Client) -> Result<String> {
    let path = input["transcript_path"].as_str().context("no transcript")?;
    let mut requests = VecDeque::with_capacity(RECENT_REQUESTS);
    let mut steps = VecDeque::with_capacity(MAX_STEPS);
    for line in BufReader::new(std::fs::File::open(path)?).lines() {
        let Ok(entry) = serde_json::from_str::<Value>(&line?) else {
            continue;
        };
        if entry["subtype"] == "compact_boundary" {
            steps.clear();
        } else if let Some(step) = transcript_step(&entry) {
            if let Some(request) = step.strip_prefix("user: ") {
                push_bounded(&mut requests, request.to_string(), RECENT_REQUESTS);
            }
            push_bounded(&mut steps, step, MAX_STEPS);
        }
    }
    if let Some(typed) = input["custom_instructions"]
        .as_str()
        .filter(|text| !text.trim().is_empty())
    {
        push_bounded(&mut requests, clip(typed, ITEM_CHARS), RECENT_REQUESTS);
    }
    anyhow::ensure!(!requests.is_empty(), "no user request");
    let requests = Vec::from(requests).join(" | ");
    let steps = Vec::from(steps);
    let question = Questions::from([(
        "needed".into(),
        Question::yesno(&format!(
            "This step is still needed to continue the latest requests: {requests}."
        )),
    )]);
    let answers = client.ask_each(steps.clone(), &question).await;
    let mut keep = Vec::new();
    let mut drop = Vec::new();
    for (step, answer) in steps.iter().zip(answers).rev() {
        match answer.ok().and_then(|answers| answers["needed"].yes()) {
            Some(yes) if yes >= KEEP_AT => keep.push(format!("- {step}")),
            Some(yes) if yes < DROP_BELOW => drop.push(format!("- {step}")),
            _ => {}
        }
    }
    if keep.is_empty() && drop.is_empty() {
        return Ok(String::new());
    }
    Ok(format!(
        "Keep exact details (paths, commands, values, decisions and open problems) from these steps, newest first:\n{}\n\nReduce these steps to one line each or drop them, newest first:\n{}",
        clip(&keep.join("\n"), MAX_LIST_CHARS),
        clip(&drop.join("\n"), MAX_LIST_CHARS)
    ))
}

fn push_bounded<T>(queue: &mut VecDeque<T>, item: T, max: usize) {
    if queue.len() == max {
        queue.pop_front();
    }
    queue.push_back(item);
}

fn transcript_step(entry: &Value) -> Option<String> {
    if entry["isSidechain"].as_bool() == Some(true) {
        return None;
    }
    let content = &entry["message"]["content"];
    match entry["type"].as_str()? {
        "user" if entry["origin"]["kind"] == "human" => {
            let text = match content.as_array() {
                Some(blocks) => blocks
                    .iter()
                    .filter_map(|block| block["text"].as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                None => content.as_str()?.to_string(),
            };
            (!text.is_empty()).then(|| format!("user: {}", clip(&text, ITEM_CHARS)))
        }
        "assistant" => {
            let steps: Vec<String> = content
                .as_array()?
                .iter()
                .filter_map(|block| match block["type"].as_str()? {
                    "text" => Some(format!("assistant: {}", block["text"].as_str()?)),
                    "tool_use" => Some(format!("{}: {}", block["name"].as_str()?, block["input"])),
                    _ => None,
                })
                .collect();
            (!steps.is_empty()).then(|| clip(&steps.join(" | "), ITEM_CHARS))
        }
        _ => None,
    }
}
