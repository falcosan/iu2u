use rmcp::{ServerHandler, handler::server::wrapper::Parameters, tool, tool_handler, tool_router};

use crate::model::Client;
use crate::tools::{self, LintParams, RankParams, SeekParams, WeighParams};

#[derive(Clone)]
pub struct Server {
    client: Client,
}

#[tool_handler(
    name = "iu2u",
    instructions = "Fast typed judgments from a decision model. Use these tools when the same question applies to many items, files or excerpts; keep explanations, plans and single high-stakes calls for yourself, and confirm findings by reading the source before acting."
)]
impl ServerHandler for Server {}

#[tool_router]
impl Server {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    #[tool(
        description = "Ask a decision model the same typed questions about many items and get calibrated probabilities in milliseconds per item, without generated text. Use it to classify, route, triage, screen or filter many items; keep summaries, explanations and single high-stakes calls for yourself."
    )]
    async fn weigh(&self, Parameters(params): Parameters<WeighParams>) -> Result<String, String> {
        tools::weigh(&self.client, params)
            .await
            .map(|result| result.to_string())
    }

    #[tool(
        description = "Order items by a criterion with a decision model, highest first. Use it to sort review findings by severity, search results by relevance, tasks by urgency or rows by quality."
    )]
    async fn rank(&self, Parameters(params): Parameters<RankParams>) -> Result<String, String> {
        tools::rank(&self.client, params)
            .await
            .map(|result| result.to_string())
    }

    #[tool(
        description = "Search files by meaning with a decision model: every excerpt is judged against the query and the best matches come back with file and line range. Use it when grep cannot express what you want, for example: where are retries handled, or which code validates user input."
    )]
    async fn seek(&self, Parameters(params): Parameters<SeekParams>) -> Result<String, String> {
        tools::seek(&self.client, params)
            .await
            .map(|result| result.to_string())
    }

    #[tool(
        description = "Check files against rules written in plain words with a decision model, for example: errors are never swallowed, or user-facing text avoids jargon. Long files are judged in excerpts that follow their structure. Returns the most likely violations with file and line range, most likely first, and how many there are in total; read those lines to confirm before acting."
    )]
    async fn lint(&self, Parameters(params): Parameters<LintParams>) -> Result<String, String> {
        tools::lint(&self.client, params)
            .await
            .map(|result| result.to_string())
    }
}
