use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{StreamExt, stream};
use reqwest::StatusCode;
use serde_json::{Value, json};

use super::protocol::{Answers, Questions, answers_from_wire, questions_to_wire};
use crate::clip;

pub const CONCURRENCY: usize = 8;
const DEFAULT_BASE_URL: &str = "http://127.0.0.1:11435";
const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const ERROR_BODY_CHARS: usize = 300;

#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    endpoint: String,
    api_key: Option<String>,
    model: String,
}

pub fn setting(name: &str) -> Option<String> {
    std::env::var(format!("CLAUDE_PLUGIN_OPTION_{name}"))
        .or_else(|_| std::env::var(format!("IU2U_{name}")))
        .ok()
        .filter(|value| !value.is_empty())
}

impl Client {
    pub fn new(
        base_url: &str,
        api_key: Option<String>,
        model: String,
        timeout: Duration,
    ) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT.min(timeout))
                .timeout(timeout)
                .build()?,
            endpoint: format!("{}/v1/systemone", base_url.trim_end_matches('/')),
            api_key: api_key.filter(|key| !key.is_empty()),
            model,
        })
    }

    pub fn from_env() -> Result<Self> {
        let timeout_ms = match setting("TIMEOUT_MS") {
            Some(value) => value
                .parse()
                .context("IU2U_TIMEOUT_MS must be milliseconds")?,
            None => DEFAULT_TIMEOUT_MS,
        };
        Self::new(
            &setting("BASE_URL").unwrap_or_else(|| DEFAULT_BASE_URL.into()),
            setting("API_KEY"),
            setting("MODEL")
                .context("set IU2U_MODEL to the model name on your decision model server")?,
            Duration::from_millis(timeout_ms),
        )
    }

    pub async fn ask(&self, state: &str, questions: &Questions) -> Result<Answers> {
        let body = json!({
            "model": self.model,
            "state": state,
            "questions": questions_to_wire(questions)
        });
        let mut request = self.http.post(&self.endpoint).json(&body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("decision model unreachable at {}", self.endpoint))?;
        let rejected = response.error_for_status_ref().err();
        let text = response.text().await?;
        let body = || clip(&text, ERROR_BODY_CHARS);
        if let Some(error) = rejected {
            return Err(anyhow::Error::new(error)
                .context(format!("decision model rejected the request: {}", body())));
        }
        let response: Value =
            serde_json::from_str(&text).context("decision model returned invalid JSON")?;
        let answers = answers_from_wire(&response)
            .with_context(|| format!("decision model returned unexpected answers: {}", body()))?;
        anyhow::ensure!(
            questions.keys().all(|id| answers.contains_key(id)),
            "decision model left questions unanswered: {}",
            body()
        );
        Ok(answers)
    }

    pub async fn ask_each(
        &self,
        states: Vec<String>,
        questions: &Questions,
    ) -> Vec<Result<Answers>> {
        let (timeouts_in_a_row, unreachable) = (&AtomicUsize::new(0), &AtomicBool::new(false));
        let mut results: Vec<_> = stream::iter(states.into_iter().enumerate())
            .map(|(index, state)| async move {
                let answers = self
                    .ask_in_batch(&state, questions, timeouts_in_a_row, unreachable)
                    .await;
                (index, answers)
            })
            .buffer_unordered(CONCURRENCY)
            .collect()
            .await;
        results.sort_unstable_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, answers)| answers).collect()
    }

    async fn ask_in_batch(
        &self,
        state: &str,
        questions: &Questions,
        timeouts_in_a_row: &AtomicUsize,
        unreachable: &AtomicBool,
    ) -> Result<Answers> {
        anyhow::ensure!(
            !unreachable.load(Ordering::Relaxed),
            "skipped: an earlier request to the decision model at {} failed",
            self.endpoint
        );
        let answers = self.ask(state, questions).await;
        let transport = answers
            .as_ref()
            .err()
            .and_then(|error| error.downcast_ref::<reqwest::Error>());
        match transport {
            Some(error)
                if error.is_connect()
                    || error.status().is_some_and(|status| {
                        [
                            StatusCode::UNAUTHORIZED,
                            StatusCode::FORBIDDEN,
                            StatusCode::NOT_FOUND,
                        ]
                        .contains(&status)
                    }) =>
            {
                unreachable.store(true, Ordering::Relaxed);
            }
            Some(error) if error.is_timeout() => {
                if timeouts_in_a_row.fetch_add(1, Ordering::Relaxed) + 1 >= CONCURRENCY {
                    unreachable.store(true, Ordering::Relaxed);
                }
            }
            _ => timeouts_in_a_row.store(0, Ordering::Relaxed),
        }
        answers
    }
}
