use std::collections::BTreeMap;

use rmcp::schemars;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    EXCERPT_CHARS, MATCH_AT, MAX_QUESTIONS, MAX_RESULTS, bounded, number, read_excerpts,
    something_judged,
};
use crate::model::{Client, Question, Questions};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LintParams {
    #[schemars(
        description = "Rules in plain words, for example: no function mutates its arguments"
    )]
    pub rules: Vec<String>,
    #[schemars(description = "Files to check against every rule")]
    pub files: Vec<String>,
}

pub async fn lint(client: &Client, params: LintParams) -> Result<Value, String> {
    bounded("rules", params.rules.len(), MAX_QUESTIONS)?;
    let (excerpts, mut failed) =
        read_excerpts(&params.files, EXCERPT_CHARS, "pass fewer files").await?;
    let questions: Questions = params
        .rules
        .iter()
        .enumerate()
        .map(|(index, rule)| {
            (
                format!("rule_{index}"),
                Question::yesno(&format!("This excerpt breaks this rule: {rule}.")),
            )
        })
        .collect();
    let results = client
        .ask_each(
            excerpts.iter().map(|excerpt| excerpt.state()).collect(),
            &questions,
        )
        .await;
    something_judged(&results)?;
    let mut violations = Vec::new();
    let mut unjudged: BTreeMap<&str, Vec<(usize, usize)>> = BTreeMap::new();
    let mut previous_failure = None;
    for (position, (excerpt, result)) in excerpts.iter().zip(results).enumerate() {
        match result {
            Ok(answers) => {
                violations.extend(params.rules.iter().enumerate().filter_map(|(index, rule)| {
                    let yes = answers.get(&format!("rule_{index}"))?.yes()?;
                    (yes >= MATCH_AT).then(|| {
                        json!({
                            "file": excerpt.file,
                            "lines": excerpt.lines(),
                            "rule": rule,
                            "likelihood": yes
                        })
                    })
                }))
            }
            Err(error) => {
                failed
                    .entry(excerpt.file.clone())
                    .or_insert_with(|| format!("{error:#}"));
                let spans = unjudged.entry(&excerpt.file).or_default();
                match spans.last_mut() {
                    Some(span) if previous_failure.is_some_and(|last| last + 1 == position) => {
                        span.1 = excerpt.last_line;
                    }
                    _ => spans.push((excerpt.first_line, excerpt.last_line)),
                }
                previous_failure = Some(position);
            }
        }
    }
    for (file, spans) in unjudged {
        let lines: Vec<String> = spans
            .iter()
            .map(|(first, last)| format!("{first}-{last}"))
            .collect();
        if let Some(reason) = failed.get_mut(file) {
            *reason = format!("lines {}: {reason}", lines.join(", "));
        }
    }
    violations.sort_by(|a, b| number(b, "likelihood").total_cmp(&number(a, "likelihood")));
    let total = violations.len();
    violations.truncate(MAX_RESULTS);
    Ok(json!({ "total": total, "violations": violations, "failed": failed }))
}
