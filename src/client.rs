use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    endpoint: String,
    api_key: Option<String>,
    model: Option<String>,
}

impl Client {
    pub fn new(
        base_url: &str,
        api_key: Option<String>,
        model: Option<String>,
        timeout: Duration,
    ) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder().timeout(timeout).build()?,
            endpoint: format!("{}/v1/systemone", base_url.trim_end_matches('/')),
            api_key: api_key.filter(|key| !key.is_empty()),
            model: model.filter(|model| !model.is_empty()),
        })
    }

    pub async fn decide(&self, state: &str, questions: &impl Serialize) -> Result<Value> {
        let mut body = json!({ "state": state, "questions": questions });
        if let Some(model) = &self.model {
            body["model"] = json!(model);
        }
        let mut request = self.http.post(&self.endpoint).json(&body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("decision model unreachable at {}", self.endpoint))?;
        let status = response.status();
        let text = response.text().await?;
        anyhow::ensure!(status.is_success(), "decision model returned {status}: {text}");
        serde_json::from_str(&text).context("decision model returned invalid JSON")
    }
}
