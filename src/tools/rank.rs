use rmcp::schemars;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{MAX_ITEMS, bounded, number, something_judged};
use crate::model::{Answer, Client, Question, Questions};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RankParams {
    #[schemars(description = "Texts to order")]
    pub items: Vec<String>,
    #[schemars(description = "What to rank by, for example: how severe is this review finding")]
    pub criterion: String,
    #[schemars(
        description = "Ordered levels from lowest to highest; defaults to low, medium, high"
    )]
    pub levels: Option<Vec<String>>,
}

pub async fn rank(client: &Client, params: RankParams) -> Result<Value, String> {
    bounded("items", params.items.len(), MAX_ITEMS)?;
    let levels = params
        .levels
        .unwrap_or_else(|| vec!["low".into(), "medium".into(), "high".into()]);
    let question = Questions::from([(
        "rank".into(),
        Question::Scale {
            text: params.criterion,
            levels,
        },
    )]);
    let results = client.ask_each(params.items, &question).await;
    something_judged(&results)?;
    let mut ranked: Vec<Value> = results
        .into_iter()
        .enumerate()
        .map(
            |(item, result)| match result.map(|answers| answers["rank"].clone()) {
                Ok(Answer::Scale { level, .. }) => json!({ "item": item, "level": level }),
                Ok(_) => json!({ "item": item, "error": "unexpected answer" }),
                Err(error) => json!({ "item": item, "error": format!("{error:#}") }),
            },
        )
        .collect();
    ranked.sort_by(|a, b| number(b, "level").total_cmp(&number(a, "level")));
    Ok(Value::from(ranked))
}
