use serde::{Deserialize, Serialize};

use crate::analyze::{self, AnalyzeError, Analyzer};
use crate::diff::FileDiff;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";

#[derive(Serialize)]
struct MessagesRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<Message>,
}

#[derive(Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    text: String,
}

pub struct ClaudeAnalyzer {
    client: reqwest::blocking::Client,
    api_key: String,
    model: String,
}

impl ClaudeAnalyzer {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
            api_key,
            model,
        }
    }
}

impl ClaudeAnalyzer {
    fn complete(&self, prompt: String) -> Result<String, AnalyzeError> {
        let request = MessagesRequest {
            model: self.model.clone(),
            max_tokens: 4096,
            messages: vec![Message {
                role: "user".to_string(),
                content: prompt,
            }],
        };

        let response = self
            .client
            .post(API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&request)
            .send()?
            .error_for_status()?;

        let body: MessagesResponse = response.json()?;
        body.content
            .into_iter()
            .next()
            .map(|b| b.text)
            .ok_or_else(|| AnalyzeError::Backend("empty response from claude".to_string()))
    }
}

impl Analyzer for ClaudeAnalyzer {
    fn analyze(
        &self,
        diff: &FileDiff,
        user_notes: Option<&str>,
    ) -> Result<String, AnalyzeError> {
        self.complete(analyze::build_prompt(diff, user_notes))
    }

    fn review(
        &self,
        diff: &FileDiff,
        existing_description: &str,
    ) -> Result<String, AnalyzeError> {
        self.complete(analyze::build_review_prompt(diff, existing_description))
    }
}
