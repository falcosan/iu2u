use std::collections::BTreeMap;

use rmcp::{handler::server::wrapper::Parameters, schemars, tool, tool_router};
use serde::{Deserialize, Serialize};

use crate::client::Client;
use crate::gate;

#[derive(Clone)]
pub struct Server {
    client: Client,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum QuestionType {
    Choice,
    Score,
    Noul,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Criteria {
    Options(BTreeMap<String, String>),
    Levels(Vec<String>),
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Question {
    #[serde(rename = "type")]
    #[schemars(description = "choice: pick one option; score: rate on ordered levels; noul: probability that the statement is true")]
    pub kind: QuestionType,
    #[schemars(description = "The question, or for noul the statement to judge")]
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "choice: map of option label to description; score: levels from lowest to highest; noul: omit")]
    pub criteria: Option<Criteria>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DecideParams {
    #[schemars(description = "What to judge: a message, ticket, diff, log or JSON, as text")]
    pub state: String,
    #[schemars(description = "Questions keyed by a short id; all are answered in one pass")]
    pub questions: BTreeMap<String, Question>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GateParams {
    #[schemars(description = "Shell command about to run")]
    pub command: String,
    #[serde(default)]
    #[schemars(description = "The agent's permission mode, from the hook input")]
    pub permission_mode: String,
}

#[tool_router(server_handler)]
impl Server {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    #[tool(
        description = "Ask a decision model typed questions about a state and get calibrated probabilities in one fast pass, without generated text. Use it for classifying, routing, triaging, screening or scoring many items; keep summaries and explanations for yourself."
    )]
    async fn decide(&self, Parameters(params): Parameters<DecideParams>) -> Result<String, String> {
        self.client
            .decide(&params.state, &params.questions)
            .await
            .map(|response| response.to_string())
            .map_err(|error| format!("{error:#}"))
    }

    #[tool(
        description = "Pre-tool-use gate for shell commands, called by the iu2u hook. Returns hook JSON that can only deny or ask, never allow."
    )]
    async fn gate_command(&self, Parameters(params): Parameters<GateParams>) -> String {
        let verdict = if params.command.chars().count() > gate::MAX_COMMAND_CHARS {
            None
        } else {
            self.client
                .decide(&params.command, &gate::questions())
                .await
                .ok()
                .and_then(|response| gate::judge(&response["answers"]))
        };
        gate::hook_output(&verdict.unwrap_or_else(|| gate::unjudged(&params.permission_mode)))
    }
}
