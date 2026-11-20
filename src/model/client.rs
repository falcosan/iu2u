use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{StreamExt, stream};
use reqwest::StatusCode;
use serde_json::{Value, json};

use super::adapters::Adapter;
#[cfg(unix)]
use super::adapters::decision;
use super::protocol::{Answers, Questions, answers_from_wire, questions_to_wire};
use crate::clip;
use crate::config::{self, Config};

pub const CONCURRENCY: usize = 8;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const ERROR_BODY_CHARS: usize = 300;
const SCREENSHOT: &str = "iu2u-screenshot";
const IMAGES_HEADER: &str = "TypeSafe-Danger-Zone-Images";
const NO_SCREENSHOTS: &str =
    "this model takes no screenshots: set vision to false, and unset IU2U_VISION";

#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    endpoint: String,
    api_key: Option<String>,
    model: String,
    timeout: Duration,
    adapter: Option<Adapter>,
    #[cfg(unix)]
    local: Option<decision::Server>,
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
                .build()?,
            endpoint: match &adapter {
                None => format!("{}/v1/systemone", base_url.trim_end_matches('/')),
                Some(_) => Adapter::PROGRAM.into(),
            },
            api_key: api_key.filter(|key| !key.is_empty()),
            model,
            timeout,
            adapter,
            #[cfg(unix)]
            local: None,
        })
    }

    #[cfg(unix)]
    fn local(server: decision::Server, model: String, timeout: Duration) -> Self {
        Self {
            http: server.http().clone(),
            endpoint: server.socket().display().to_string(),
            api_key: None,
            model,
            timeout,
            adapter: None,
            local: Some(server),
        }
    }

    fn url(&self) -> &str {
        #[cfg(unix)]
        if self.local.is_some() {
            return decision::LOCAL_URL;
        }
        &self.endpoint
    }

    pub fn from_env() -> Result<Self> {
        let config = Config::load()?;
        let timeout_ms: u64 = config
            .get("timeout_ms")
            .unwrap_or_default()
            .parse()
            .ok()
            .filter(|ms| *ms > 0)
            .context("IU2U_TIMEOUT_MS must be a positive number of milliseconds")?;
        let model = config.get("model").context(if cfg!(unix) {
            "no decision model is set: run iu2u init, or set model with iu2u config set model"
        } else {
            "no decision model is set: set model with iu2u config set model"
        })?;
        #[cfg(unix)]
        if let Some(server) = decision::from_model(&model)? {
            return Ok(Self::local(
                server,
                model,
                Duration::from_millis(timeout_ms),
            ));
        }
        #[cfg(not(unix))]
        anyhow::ensure!(
            super::adapters::Reference::parse(&model)?.is_none(),
            super::adapters::UNSUPPORTED
        );
        Self::build(
            Adapter::from_model(&model, &config.get("effort").unwrap_or_default())?,
            &config.get("base_url").unwrap_or_default(),
            config::api_key().map(|(key, _)| key),
            model,
            Duration::from_millis(timeout_ms),
        )
    }

    #[cfg(unix)]
    pub(crate) fn warm(&self) {
        if let Some(server) = &self.local
            && matches!(server.supervised(), Ok(false))
        {
            server.spawn().ok();
        }
    }

    #[cfg(unix)]
    pub(crate) fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
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
            anyhow::ensure!(screenshot.is_none(), NO_SCREENSHOTS);
            return adapter.ask(self.timeout, state, questions).await;
        }
        #[cfg(unix)]
        anyhow::ensure!(screenshot.is_none() || self.local.is_none(), NO_SCREENSHOTS);
        let mut body = json!({
            "model": self.model,
            "state": state,
            "questions": questions_to_wire(questions)
        });
        if let Some(screenshot) = screenshot {
            body["state"] = format!("{state}\n\nScreenshot of the part of the screen in view, while the text above also lists what lies out of view: {SCREENSHOT}").into();
            body["assets"] = json!({ SCREENSHOT: { "type": "image", "format": "png", "data_base64": screenshot } });
        }
        let send = || {
            let mut request = self.http.post(self.url()).timeout(self.timeout);
            if screenshot.is_some() {
                request = request.header(IMAGES_HEADER, "true");
            }
            let mut request = request.json(&body);
            if let Some(key) = &self.api_key {
                request = request.bearer_auth(key);
            }
            request.send()
        };
        #[cfg(unix)]
        let response = match &self.local {
            None => send().await.map_err(anyhow::Error::from),
            Some(server) => server.send(send).await,
        };
        #[cfg(not(unix))]
        let response = send().await.map_err(anyhow::Error::from);
        let response =
            response.with_context(|| format!("decision model unreachable at {}", self.endpoint))?;
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
        StatusCode::NOT_IMPLEMENTED,
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
    error
        .downcast_ref::<reqwest::Error>()
        .and_then(reqwest::Error::status)
        .is_some_and(|status| status.is_client_error() && !systemic.contains(&status))
}

#[cfg(unix)]
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

    #[tokio::test]
    async fn adapters_and_the_local_model_refuse_screenshots() {
        let refused = async |client: Client| {
            let questions = Questions::from([("rain".into(), Question::yesno("It rains"))]);
            let error = client
                .ask_seeing("A page", &questions, Some("iVBORw0KGgo="))
                .await
                .unwrap_err();
            format!("{error:#}")
        };
        let adapter = Client::build(
            Adapter::from_model("claude", "medium").unwrap(),
            "http://127.0.0.1:9",
            None,
            "claude".into(),
            Duration::from_secs(1),
        );
        assert_eq!(refused(adapter.unwrap()).await, NO_SCREENSHOTS);
        #[cfg(local_model)]
        let local = Client::local(
            decision::Server::new(std::path::Path::new("/tmp/iu2u-unused"), &"a".repeat(64))
                .unwrap(),
            "decision".into(),
            Duration::from_secs(1),
        );
        #[cfg(local_model)]
        assert_eq!(refused(local).await, NO_SCREENSHOTS);
    }
}
