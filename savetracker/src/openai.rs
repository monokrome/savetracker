use serde::{Deserialize, Serialize};

use crate::analyze::{self, AnalyzeError, Analyzer};
use crate::diff::FileDiff;

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
}

#[derive(Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: String,
}

pub struct OpenAiAnalyzer {
    client: reqwest::blocking::Client,
    url: String,
    api_key: String,
    model: String,
}

impl OpenAiAnalyzer {
    pub fn new(url: String, api_key: String, model: String) -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
            url,
            api_key,
            model,
        }
    }
}

impl OpenAiAnalyzer {
    fn complete(&self, prompt: String) -> Result<String, AnalyzeError> {
        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![Message {
                role: "user".to_string(),
                content: prompt,
            }],
        };

        let response = self
            .client
            .post(&self.url)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()?
            .error_for_status()?;

        let body: ChatResponse = response.json()?;
        body.choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| AnalyzeError::Backend("empty response".to_string()))
    }
}

impl Analyzer for OpenAiAnalyzer {
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
