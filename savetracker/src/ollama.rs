use std::io::{BufRead, BufReader};

use serde::{Deserialize, Serialize};

use crate::analyze::{self, AnalyzeError, Analyzer};
use crate::diff::FileDiff;

#[derive(Debug, Serialize)]
struct GenerateRequest {
    model: String,
    prompt: String,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct GenerateResponse {
    response: String,
    #[serde(default)]
    done: bool,
}

pub struct OllamaAnalyzer {
    client: reqwest::blocking::Client,
    base_url: String,
    model: String,
}

impl OllamaAnalyzer {
    pub fn new(base_url: String, model: String) -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
            base_url,
            model,
        }
    }

    pub fn analyze_streaming<F>(
        &self,
        diff: &FileDiff,
        user_notes: Option<&str>,
        mut on_token: F,
    ) -> Result<(), AnalyzeError>
    where
        F: FnMut(&str),
    {
        let url = format!("{}/api/generate", self.base_url);
        let request = GenerateRequest {
            model: self.model.clone(),
            prompt: analyze::build_prompt(diff, user_notes),
            stream: true,
        };

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()?
            .error_for_status()?;

        let reader = BufReader::new(response);
        for line in reader.lines() {
            let line = line.map_err(|e| AnalyzeError::Backend(e.to_string()))?;
            if let Ok(resp) = serde_json::from_str::<GenerateResponse>(&line) {
                on_token(&resp.response);
                if resp.done {
                    break;
                }
            }
        }

        Ok(())
    }
}

impl Analyzer for OllamaAnalyzer {
    fn analyze(
        &self,
        diff: &FileDiff,
        user_notes: Option<&str>,
    ) -> Result<String, AnalyzeError> {
        let url = format!("{}/api/generate", self.base_url);
        let request = GenerateRequest {
            model: self.model.clone(),
            prompt: analyze::build_prompt(diff, user_notes),
            stream: false,
        };

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()?
            .error_for_status()?;

        let body: GenerateResponse = response.json()?;
        Ok(body.response)
    }
}
