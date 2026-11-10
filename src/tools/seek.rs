use rmcp::schemars;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{EXCERPT_CHARS, MATCH_AT, MAX_RESULTS, read_excerpts, something_judged};
use crate::model::{Client, Question, Questions};

const EXCERPT_CHARS_RANGE: std::ops::RangeInclusive<usize> = 200..=8_000;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SeekParams {
    #[schemars(description = "What you are looking for, in plain words")]
    pub query: String,
    #[schemars(description = "Files to search, for example the output of a glob")]
    pub files: Vec<String>,
    #[schemars(
        description = "Most non-whitespace characters per excerpt, from 200 to 8000; defaults to 2000. Excerpts follow the structure of the text, such as functions, blocks and paragraphs"
    )]
    pub max_excerpt_chars: Option<usize>,
}

pub async fn seek(client: &Client, params: SeekParams) -> Result<Value, String> {
    let budget = params.max_excerpt_chars.unwrap_or(EXCERPT_CHARS);
    if !EXCERPT_CHARS_RANGE.contains(&budget) {
        return Err(format!(
            "max_excerpt_chars must be from {} to {}, got {budget}",
            EXCERPT_CHARS_RANGE.start(),
            EXCERPT_CHARS_RANGE.end()
        ));
    }
    let (excerpts, skipped) = read_excerpts(
        &params.files,
        budget,
        "pass fewer files or a larger max_excerpt_chars",
    )
    .await?;
    let question = Questions::from([(
        "relevant".into(),
        Question::yesno(&format!(
            "This excerpt is directly relevant to: {}.",
            params.query
        )),
    )]);
    let results = client
        .ask_each(
            excerpts.iter().map(|excerpt| excerpt.state()).collect(),
            &question,
        )
        .await;
    something_judged(&results)?;
    let mut matches: Vec<(f64, _)> = results
        .iter()
        .zip(&excerpts)
        .filter_map(|(result, excerpt)| Some((result.as_ref().ok()?["relevant"].yes()?, excerpt)))
        .filter(|(yes, _)| *yes >= MATCH_AT)
        .collect();
    matches.sort_by(|a, b| b.0.total_cmp(&a.0));
    Ok(json!({
        "excerpts": excerpts.len(),
        "failed": results.iter().filter(|result| result.is_err()).count(),
        "skipped": skipped,
        "total": matches.len(),
        "matches": matches.iter().take(MAX_RESULTS).map(|(yes, excerpt)| json!({
            "file": excerpt.file,
            "lines": excerpt.lines(),
            "relevance": yes,
        })).collect::<Vec<_>>(),
    }))
}
