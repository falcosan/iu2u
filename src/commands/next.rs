use std::collections::BTreeMap;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::model::{Answer, Client, Question, Questions};

#[derive(Debug, Deserialize)]
pub struct NextRequest {
    pub goal: String,
    pub screen: String,
    pub actions: BTreeMap<String, String>,
}

pub async fn next(client: &Client, request: NextRequest) -> Result<Value> {
    anyhow::ensure!(request.actions.len() >= 2, "give at least two actions");
    let goal = &request.goal;
    let questions = Questions::from([
        (
            "action".into(),
            Question::Pick {
                text: format!("Which action moves closest to this goal: {goal}."),
                options: request.actions,
            },
        ),
        (
            "done".into(),
            Question::yesno(&format!(
                "The screen shows that this goal has been reached: {goal}."
            )),
        ),
        (
            "stuck".into(),
            Question::yesno(&format!(
                "The screen shows that progress toward this goal is blocked, for example by an error, a dead end or a loop: {goal}."
            )),
        ),
    ]);
    let answers = client.ask(&request.screen, &questions).await?;
    let Answer::Pick {
        pick, confidence, ..
    } = &answers["action"]
    else {
        anyhow::bail!("decision model did not pick an action");
    };
    Ok(json!({
        "action": pick,
        "confidence": confidence,
        "done": answers["done"].yes(),
        "stuck": answers["stuck"].yes(),
    }))
}
