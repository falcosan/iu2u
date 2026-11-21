use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{StreamExt, stream};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::sync::OnceCell;

use super::adapters::Adapter;
#[cfg(unix)]
use super::adapters::decision;
use super::protocol::{Answers, Questions, answers_from_wire, questions_to_wire};
use crate::clip;
use crate::config::{self, Config};

pub const CONCURRENCY: usize = 8;
pub const TRIES: u32 = 3;
const RETRY_AFTER: Duration = Duration::from_secs(1);
const CONNECT_WITHIN: Duration = Duration::from_secs(2);
const SYSTEMONE: &str = "/v1/systemone";
pub(super) const ERROR_BODY_CHARS: usize = 300;
const SCREENSHOT: &str = "iu2u-screenshot";
const IMAGES_HEADER: &str = "TypeSafe-Danger-Zone-Images";
const NO_SCREENSHOTS: &str =
    "this model takes no screenshots: set vision to false, and unset IU2U_VISION";

#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    endpoint: String,
    api_key: Option<String>,
    model: String,
    timeout: Duration,
    adapter: Option<Adapter>,
    context: OnceCell<Option<usize>>,
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
        let base = base_url.trim_end_matches('/');
        Ok(Self {
            http: reqwest::Client::builder()
                .connect_timeout(CONNECT_WITHIN.min(timeout))
                .build()?,
            base: base.into(),
            endpoint: match &adapter {
                None => format!("{base}{SYSTEMONE}"),
                Some(_) => Adapter::PROGRAM.into(),
            },
            api_key: api_key.filter(|key| !key.is_empty()),
            model,
            timeout,
            adapter,
            context: OnceCell::new(),
            #[cfg(unix)]
            local: None,
        })
    }

    #[cfg(unix)]
    fn local(server: decision::Server, model: String, timeout: Duration) -> Self {
        Self {
            http: server.http().clone(),
            base: decision::LOCAL_BASE.into(),
            endpoint: server.socket().display().to_string(),
            api_key: None,
            model,
            timeout,
            adapter: None,
            context: OnceCell::new(),
            local: Some(server),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    pub fn from_env() -> Result<Self> {
        let config = Config::load()?;
        let timeout_ms: u64 = config
            .get("timeout_ms")?
            .and_then(|ms| ms.parse().ok())
            .context("timeout_ms has no value")?;
        let model = config.get("model")?.context(if cfg!(unix) {
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
            Adapter::from_model(&model, &config.get("effort")?.unwrap_or_default())?,
            &config.get("base_url")?.unwrap_or_default(),
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

    #[cfg(unix)]
    pub(crate) fn timeout(&self) -> Duration {
        self.timeout
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
        let mut tried = 1;
        loop {
            match self.ask_once(state, questions, screenshot).await {
                Err(error) if tried < TRIES && transient(&error) => {
                    tokio::time::sleep(RETRY_AFTER * tried).await;
                    tried += 1;
                }
                answers => return answers,
            }
        }
    }

    async fn ask_once(
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
        let response = self
            .send(|| {
                let request = self.http.post(self.url(SYSTEMONE));
                match screenshot {
                    Some(_) => request.header(IMAGES_HEADER, "true"),
                    None => request,
                }
                .json(&body)
            })
            .await
            .with_context(|| format!("decision model unreachable at {}", self.endpoint))?;
        let rejected = response.error_for_status_ref().err();
        let text = response.text().await?;
        let body = || clip(&text, ERROR_BODY_CHARS);
        if let Some(error) = rejected {
            let error = anyhow::Error::new(error);
            return Err(match text.trim().is_empty() {
                true => error.context("decision model rejected the request"),
                false => error.context(format!("decision model rejected the request: {}", body())),
            });
        }
        let response: Value =
            serde_json::from_str(&text).context("decision model returned invalid JSON")?;
        let answers =
            answers_from_wire(&response, questions).with_context(|| unexpected_answers(&text))?;
        anyhow::ensure!(
            questions.keys().all(|id| answers.contains_key(id)),
            "decision model left questions unanswered: {}",
            body()
        );
        Ok(answers)
    }

    pub async fn context(&self) -> Option<usize> {
        if self.adapter.is_some() {
            return Some(Adapter::CONTEXT);
        }
        self.context
            .get_or_try_init(async || {
                let Some(served) = self
                    .reported("/props", "/default_generation_settings/n_ctx")
                    .await?
                else {
                    return anyhow::Ok(None);
                };
                let trained = self
                    .reported("/v1/models", "/data/0/meta/n_ctx_train")
                    .await?;
                anyhow::Ok(Some(trained.map_or(served, |trained| served.min(trained))))
            })
            .await
            .ok()
            .copied()
            .flatten()
    }

    async fn reported(&self, path: &str, pointer: &str) -> Result<Option<usize>> {
        let response = self.send(|| self.http.get(self.url(path))).await?;
        anyhow::ensure!(
            !response.status().is_server_error(),
            "the decision model server is not ready"
        );
        let reply: Option<Value> = match response.status().is_success() {
            true => response.json().await.ok(),
            false => None,
        };
        Ok(reply
            .as_ref()
            .and_then(|reply| reply.pointer(pointer)?.as_u64())
            .and_then(|tokens| usize::try_from(tokens).ok())
            .filter(|tokens| *tokens > 0))
    }

    pub async fn tokens(&self, text: &str) -> Result<usize> {
        anyhow::ensure!(
            self.adapter.is_none(),
            "the {} adapter counts no tokens",
            Adapter::PROGRAM
        );
        let reply: Value = self
            .send(|| {
                self.http
                    .post(self.url("/tokenize"))
                    .json(&json!({ "content": text }))
            })
            .await?
            .error_for_status()?
            .json()
            .await?;
        reply["tokens"]
            .as_array()
            .map(Vec::len)
            .context("the decision model server returned no tokens")
    }

    async fn send(
        &self,
        request: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        let send = || {
            let request = request().timeout(self.timeout);
            match &self.api_key {
                Some(key) => request.bearer_auth(key),
                None => request,
            }
            .send()
        };
        #[cfg(unix)]
        if let Some(server) = &self.local {
            return server.send(send).await;
        }
        Ok(send().await?)
    }

    pub async fn ask_each(
        &self,
        states: Vec<String>,
        questions: &Questions,
    ) -> Vec<Result<Answers>> {
        self.batch(states, questions, None).await
    }

    pub async fn ask_each_within(
        &self,
        states: Vec<String>,
        questions: &Questions,
        within: Duration,
    ) -> Vec<Result<Answers>> {
        self.batch(
            states,
            questions,
            Some(tokio::time::Instant::now() + within),
        )
        .await
    }

    async fn batch(
        &self,
        states: Vec<String>,
        questions: &Questions,
        deadline: Option<tokio::time::Instant>,
    ) -> Vec<Result<Answers>> {
        let (failures_in_a_row, unreachable) = (&AtomicUsize::new(0), &AtomicBool::new(false));
        let mut results: Vec<_> = stream::iter(states.into_iter().enumerate())
            .map(|(index, state)| async move {
                let answers = self
                    .ask_in_batch(&state, questions, failures_in_a_row, unreachable, deadline)
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
        deadline: Option<tokio::time::Instant>,
    ) -> Result<Answers> {
        anyhow::ensure!(
            !unreachable.load(Ordering::Relaxed),
            "skipped: an earlier request to the decision model at {} failed",
            self.endpoint
        );
        let answers = match deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, self.ask(state, questions))
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("skipped: the batch ran out of time"))),
            None => self.ask(state, questions).await,
        };
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

pub(super) fn unexpected_answers(body: &str) -> String {
    format!(
        "decision model returned unexpected answers: {}",
        clip(body, ERROR_BODY_CHARS)
    )
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

fn transient(error: &anyhow::Error) -> bool {
    let busy = error
        .downcast_ref::<reqwest::Error>()
        .and_then(reqwest::Error::status)
        .is_some_and(|status| status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error());
    !cannot_reach(error) && (busy || timed_out(error))
}

fn timed_out(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<reqwest::Error>()
        .is_some_and(reqwest::Error::is_timeout)
        || error.is::<tokio::time::error::Elapsed>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Question;

    #[tokio::test]
    async fn a_model_that_answers_too_late_is_retried_whatever_runs_it() {
        let late = tokio::time::timeout(Duration::ZERO, std::future::pending::<()>())
            .await
            .context("claude did not answer")
            .unwrap_err();
        assert!(transient(&late));
    }

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
        let adapter = adapter.unwrap();
        assert_eq!(adapter.context().await, Some(Adapter::CONTEXT));
        assert!(adapter.tokens("a page").await.is_err());
        assert_eq!(refused(adapter).await, NO_SCREENSHOTS);
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
