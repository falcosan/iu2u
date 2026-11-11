use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::{ITEM_CHARS, data_path};
use crate::clip;
use crate::model::{Client, Question, Questions};

const TRIM_AT_CHARS: usize = 12_000;
const BLOCK_LINES: usize = 40;
const MAX_BLOCKS: usize = 200;
const KEEP_AT: f64 = 0.3;
const ALWAYS_KEPT: [&str; 7] = [
    "error",
    "fail",
    "warn",
    "panic",
    "exception",
    "denied",
    "fatal",
];

pub(super) async fn run(input: &Value, client: &Client) -> Result<String> {
    let response = input.get("tool_response").context("no tool_response")?;
    let stdout = response["stdout"].as_str().context("no stdout")?;
    if stdout.len() < TRIM_AT_CHARS || response.get("persistedOutputPath").is_some() {
        return Ok(String::new());
    }
    let saved = data_path("trimmed", input["tool_use_id"].as_str().unwrap_or("output"))?
        .with_extension("txt");
    let command = input
        .pointer("/tool_input/command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let purpose = input
        .pointer("/tool_input/description")
        .and_then(Value::as_str)
        .unwrap_or(command);
    let lines: Vec<&str> = stdout.lines().collect();
    let block_lines = BLOCK_LINES.max(lines.len().div_ceil(MAX_BLOCKS));
    let blocks: Vec<&[&str]> = lines.chunks(block_lines).collect();
    let question = Questions::from([(
        "matters".into(),
        Question::yesno(&format!(
            "Does this part of the output of `{}` contain an error, a failure, a warning, or a result or value needed for this purpose: {}?",
            clip(command, ITEM_CHARS),
            clip(purpose, ITEM_CHARS)
        )),
    )]);
    let last = blocks.len() - 1;
    let undecided: Vec<usize> = (0..blocks.len())
        .filter(|&index| index != 0 && index != last && !always_kept(blocks[index]))
        .collect();
    let answers = client
        .ask_each(
            undecided
                .iter()
                .map(|&index| blocks[index].join("\n"))
                .collect(),
            &question,
        )
        .await;
    let mut dropped = vec![false; blocks.len()];
    for (&index, answer) in undecided.iter().zip(answers) {
        dropped[index] = answer
            .ok()
            .and_then(|answers| answers["matters"].yes())
            .is_some_and(|yes| yes < KEEP_AT);
    }
    if !dropped.contains(&true) {
        return Ok(String::new());
    }
    write_private(&saved, stdout)?;
    let mut trimmed = Vec::new();
    let mut hidden = 0;
    for (block, drop) in blocks.iter().zip(&dropped) {
        if *drop {
            hidden += block.len();
            continue;
        }
        if hidden > 0 {
            trimmed.push(format!(
                "[iu2u trim: {hidden} lines hidden; the full output is in {} for 24 hours]",
                saved.display()
            ));
            hidden = 0;
        }
        trimmed.extend(block.iter().map(|line| (*line).to_string()));
    }
    let mut updated = response.clone();
    updated["stdout"] = json!(trimmed.join("\n"));
    Ok(json!({
        "hookSpecificOutput": { "hookEventName": "PostToolUse", "updatedToolOutput": updated }
    })
    .to_string())
}

fn always_kept(block: &[&str]) -> bool {
    block.iter().any(|line| {
        let line = line.to_lowercase();
        ALWAYS_KEPT.iter().any(|word| line.contains(word))
    })
}

fn write_private(path: &Path, text: &str) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(path)?, text.as_bytes())?;
    Ok(())
}
