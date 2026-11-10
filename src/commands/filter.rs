use anyhow::Result;
use futures_util::{Stream, StreamExt};

use crate::model::{CONCURRENCY, Client, Question, Questions};

pub fn filter<'a>(
    client: &'a Client,
    statement: &str,
    at: f64,
    lines: impl Stream<Item = std::io::Result<String>> + 'a,
) -> impl Stream<Item = Result<String>> + 'a {
    let question = Questions::from([("holds".into(), Question::yesno(statement))]);
    lines
        .map(move |line| {
            let question = question.clone();
            async move {
                let line = line?;
                if line.trim().is_empty() {
                    return Ok(None);
                }
                let holds = client.ask(&line, &question).await?["holds"].yes();
                anyhow::Ok(holds.is_some_and(|yes| yes >= at).then_some(line))
            }
        })
        .buffered(CONCURRENCY)
        .filter_map(|kept| async move { kept.transpose() })
}
