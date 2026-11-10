use rmcp::schemars;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{MAX_ITEMS, MAX_QUESTIONS, bounded, something_judged};
use crate::model::{Client, Questions};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WeighParams {
    #[schemars(
        description = "Texts to judge, one per item: messages, tickets, rows, diffs, log lines"
    )]
    pub items: Vec<String>,
    #[schemars(
        description = "Questions keyed by a short id, asked about every item in one pass per item"
    )]
    pub questions: Questions,
}

pub async fn weigh(client: &Client, params: WeighParams) -> Result<Value, String> {
    bounded("items", params.items.len(), MAX_ITEMS)?;
    bounded("questions", params.questions.len(), MAX_QUESTIONS)?;
    let results = client.ask_each(params.items, &params.questions).await;
    something_judged(&results)?;
    Ok(Value::from_iter(results.into_iter().enumerate().map(
        |(item, result)| match result {
            Ok(answers) => json!({ "item": item, "answers": answers }),
            Err(error) => json!({ "item": item, "error": format!("{error:#}") }),
        },
    )))
}
