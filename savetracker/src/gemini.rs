use serde::{Deserialize, Serialize};

use crate::analyze::{self, AnalyzeError, Analyzer};
use crate::diff::FileDiff;

const API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";

#[derive(Serialize)]
struct GenerateRequest {
    contents: Vec<Content>,
}

#[derive(Serialize)]
struct Content {
    parts: Vec<Part>,
}

#[derive(Serialize)]
struct Part {
    text: String,
}

#[derive(Deserialize)]
struct GenerateResponse {
    candidates: Vec<Candidate>,
}

#[derive(Deserialize)]
struct Candidate {
    content: CandidateContent,
}

#[derive(Deserialize)]
struct CandidateContent {
    parts: Vec<ResponsePart>,
}

#[derive(Deserialize)]
struct ResponsePart {
    text: String,
}

pub struct GeminiAnalyzer {
    client: reqwest::blocking::Client,
    api_key: String,
    model: String,
}

impl GeminiAnalyzer {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: analyze::http_client(),
            api_key,
            model,
        }
    }
}

impl GeminiAnalyzer {
    fn complete(&self, prompt: String) -> Result<String, AnalyzeError> {
        let url = format!(
            "{API_BASE}/{}:generateContent?key={}",
            self.model, self.api_key
        );

        let request = GenerateRequest {
            contents: vec![Content {
                parts: vec![Part { text: prompt }],
            }],
        };

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()?
            .error_for_status()?;

        let body: GenerateResponse = response.json()?;
        body.candidates
            .into_iter()
            .next()
            .and_then(|c| c.content.parts.into_iter().next())
            .map(|p| p.text)
            .ok_or_else(|| AnalyzeError::Backend("empty response from gemini".to_string()))
    }
}

impl Analyzer for GeminiAnalyzer {
    fn identity(&self) -> String {
        format!("gemini:{}", self.model)
    }

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
