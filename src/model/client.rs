use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{StreamExt, stream};
use reqwest::StatusCode;
use serde_json::{Value, json};

use super::adapters::{Adapter, Refused};
use super::protocol::{Answers, Questions, answers_from_wire, questions_to_wire};
use crate::clip;

pub const CONCURRENCY: usize = 8;
const DEFAULT_BASE_URL: &str = "http://127.0.0.1:11435";
const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const ERROR_BODY_CHARS: usize = 300;
const SCREENSHOT: &str = "iu2u-screenshot";
const IMAGES_HEADER: &str = "TypeSafe-Danger-Zone-Images";

#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    endpoint: String,
    api_key: Option<String>,
    model: String,
    timeout: Duration,
    adapter: Option<Adapter>,
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
        Self::build(None, base_url, api_key, model, timeout)
    }

    fn build(
        adapter: Option<Adapter>,
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
            endpoint: match &adapter {
                None => format!("{}/v1/systemone", base_url.trim_end_matches('/')),
                Some(adapter) => adapter.program().into(),
            },
            api_key: api_key.filter(|key| !key.is_empty()),
            model,
            timeout,
            adapter,
        })
    }

    pub fn from_env() -> Result<Self> {
        let timeout_ms = match setting("TIMEOUT_MS") {
            Some(value) => value
                .parse()
                .context("IU2U_TIMEOUT_MS must be milliseconds")?,
            None => DEFAULT_TIMEOUT_MS,
        };
        let model = setting("MODEL").context(
            "set IU2U_MODEL to the model name on your decision model server, or to apple-intelligence or claude",
        )?;
        Self::build(
            Adapter::from_model(&model, setting("EFFORT"))?,
            &setting("BASE_URL").unwrap_or_else(|| DEFAULT_BASE_URL.into()),
            setting("API_KEY"),
            model,
            Duration::from_millis(timeout_ms),
        )
    }

    pub async fn ask(&self, state: &str, questions: &Questions) -> Result<Answers> {
        self.ask_seeing(state, questions, None).await
    }

    pub async fn ask_seeing(
        &self,
        state: &str,
        questions: &Questions,
        screenshot: Option<&str>,
    ) -> Result<Answers> {
        if let Some(adapter) = &self.adapter {
            anyhow::ensure!(
                screenshot.is_none(),
                "adapters take no screenshots: unset IU2U_VISION"
            );
            return adapter.ask(self.timeout, state, questions).await;
        }
        let mut body = json!({
            "model": self.model,
            "state": state,
            "questions": questions_to_wire(questions)
        });
        let mut request = self.http.post(&self.endpoint);
        if let Some(screenshot) = screenshot {
            body["state"] = format!("{state}\n\nScreenshot of the part of the screen in view, while the text above also lists what lies out of view: {SCREENSHOT}").into();
            body["assets"] = json!({ SCREENSHOT: { "type": "image", "format": "png", "data_base64": screenshot } });
            request = request.header(IMAGES_HEADER, "true");
        }
        let mut request = request.json(&body);
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
        let (failures_in_a_row, unreachable) = (&AtomicUsize::new(0), &AtomicBool::new(false));
        let mut results: Vec<_> = stream::iter(states.into_iter().enumerate())
            .map(|(index, state)| async move {
                let answers = self
                    .ask_in_batch(&state, questions, failures_in_a_row, unreachable)
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
        failures_in_a_row: &AtomicUsize,
        unreachable: &AtomicBool,
    ) -> Result<Answers> {
        anyhow::ensure!(
            !unreachable.load(Ordering::Relaxed),
            "skipped: an earlier request to the decision model at {} failed",
            self.endpoint
        );
        let answers = self.ask(state, questions).await;
        match answers.as_ref().err() {
            Some(error) if cannot_reach(error) => unreachable.store(true, Ordering::Relaxed),
            Some(error) if !rejects_only_this_item(error) => {
                if failures_in_a_row.fetch_add(1, Ordering::Relaxed) + 1 >= CONCURRENCY {
                    unreachable.store(true, Ordering::Relaxed);
                }
            }
            _ => failures_in_a_row.store(0, Ordering::Relaxed),
        }
        answers
    }
}

fn cannot_reach(error: &anyhow::Error) -> bool {
    let refused = [
        StatusCode::UNAUTHORIZED,
        StatusCode::FORBIDDEN,
        StatusCode::NOT_FOUND,
    ];
    error.downcast_ref::<reqwest::Error>().is_some_and(|error| {
        error.is_connect()
            || error
                .status()
                .is_some_and(|status| refused.contains(&status))
    }) || error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

fn rejects_only_this_item(error: &anyhow::Error) -> bool {
    let systemic = [StatusCode::REQUEST_TIMEOUT, StatusCode::TOO_MANY_REQUESTS];
    error.is::<Refused>()
        || error
            .downcast_ref::<reqwest::Error>()
            .and_then(reqwest::Error::status)
            .is_some_and(|status| status.is_client_error() && !systemic.contains(&status))
}

pub(crate) fn timed_out(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<reqwest::Error>()
        .is_some_and(reqwest::Error::is_timeout)
        || error.is::<tokio::time::error::Elapsed>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Question;

    #[test]
    fn a_missing_program_stops_a_batch() {
        let missing = Err::<(), _>(std::io::Error::from(std::io::ErrorKind::NotFound))
            .context("cannot run claude")
            .unwrap_err();
        assert!(cannot_reach(&missing));
    }

    #[test]
    fn an_item_the_model_refuses_does_not_stop_a_batch() {
        let refused = anyhow::Error::msg(Refused("apfel refused this item".into()));
        assert!(rejects_only_this_item(&refused) && !cannot_reach(&refused));
    }

    #[tokio::test]
    async fn adapters_refuse_screenshots() {
        let client = Client::build(
            Some(Adapter::AppleIntelligence),
            DEFAULT_BASE_URL,
            None,
            "apple-intelligence".into(),
            Duration::from_secs(1),
        )
        .unwrap();
        let questions = Questions::from([("rain".into(), Question::yesno("It rains"))]);
        let error = client
            .ask_seeing("A page", &questions, Some("iVBORw0KGgo="))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unset IU2U_VISION"), "{error:#}");
    }
}
