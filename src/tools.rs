mod excerpts;
mod lint;
mod rank;
mod seek;
mod weigh;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::io::AsyncReadExt;

use crate::model::Answers;
use excerpts::{Excerpt, excerpts};

pub use lint::{LintParams, lint};
pub use rank::{RankParams, rank};
pub use seek::{SeekParams, seek};
pub use weigh::{WeighParams, weigh};

const MAX_ITEMS: usize = 500;
const MAX_QUESTIONS: usize = 50;
const MAX_FILE_BYTES: u64 = 1 << 20;
const MATCH_AT: f64 = 0.5;
const EXCERPT_CHARS: usize = 2000;
const MAX_EXCERPTS: usize = 2000;
const MAX_RESULTS: usize = 20;

fn bounded(what: &str, count: usize, max: usize) -> Result<(), String> {
    if count == 0 {
        Err(format!("{what} must not be empty"))
    } else if count > max {
        Err(format!("at most {max} {what} per call, got {count}"))
    } else {
        Ok(())
    }
}

fn something_judged(results: &[anyhow::Result<Answers>]) -> Result<(), String> {
    match results {
        [Err(error), ..] if results.iter().all(Result::is_err) => Err(format!("{error:#}")),
        _ => Ok(()),
    }
}

fn number(entry: &Value, key: &str) -> f64 {
    entry[key].as_f64().unwrap_or(f64::NEG_INFINITY)
}

async fn read_text(file: &str) -> Result<String, String> {
    let cannot_read = |reason: &dyn std::fmt::Display| format!("cannot read {file}: {reason}");
    let path = std::env::var_os("CLAUDE_PROJECT_DIR").map_or_else(
        || PathBuf::from(file),
        |project| Path::new(&project).join(file),
    );
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|error| cannot_read(&error))?;
    if !metadata.is_file() {
        return Err(cannot_read(&"not a regular file"));
    }
    let mut bytes = Vec::new();
    tokio::fs::File::open(&path)
        .await
        .map_err(|error| cannot_read(&error))?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| cannot_read(&error))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(cannot_read(&format!("larger than {MAX_FILE_BYTES} bytes")));
    }
    String::from_utf8(bytes).map_err(|error| cannot_read(&error))
}

async fn read_excerpts(
    files: &[String],
    budget: usize,
    too_many: &str,
) -> Result<(Vec<Excerpt>, BTreeMap<String, String>), String> {
    bounded("files", files.len(), MAX_ITEMS)?;
    let mut found = Vec::new();
    let mut unreadable = BTreeMap::new();
    for file in files {
        match read_text(file).await {
            Ok(text) => found.extend(excerpts(file, &text, budget)),
            Err(error) => {
                unreadable.insert(file.clone(), error);
            }
        }
        if found.len() > MAX_EXCERPTS {
            return Err(format!(
                "at most {MAX_EXCERPTS} excerpts per call; {too_many}"
            ));
        }
    }
    if unreadable.len() == files.len() {
        return Err(format!("nothing readable: {unreadable:?}"));
    }
    Ok((found, unreadable))
}
