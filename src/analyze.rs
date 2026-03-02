use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::diff::FileDiff;

#[derive(Debug, Error)]
pub enum AnalyzeError {
    #[error("ollama request failed: {0}")]
    Request(#[from] reqwest::Error),

    #[error("ollama returned an error: {0}")]
    OllamaError(String),
}

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

fn build_prompt(diff: &FileDiff, user_notes: Option<&str>) -> String {
    let base = format!(
        "You are analyzing changes to a game save file.\n\
         File format: {format}\n\
         Change summary: {summary}\n\n\
         Diff:\n{detail}\n\n",
        format = diff.format,
        summary = diff.summary,
        detail = diff.detail,
    );

    match user_notes {
        Some(notes) => format!(
            "{base}\
             The player provided these notes about what they did:\n\
             {notes}\n\n\
             Correlate the player's description with the changes in the diff. \
             Identify which parts of the save file correspond to what the player described. \
             Note any changes not covered by the player's notes. \
             Keep your answer concise."
        ),
        None => format!(
            "{base}\
             Based on this diff, what game state changes likely occurred? \
             Be specific about what the player did or what happened in the game. \
             Keep your answer concise."
        ),
    }
}

pub async fn analyze(
    diff: &FileDiff,
    ollama_url: &str,
    model: &str,
    user_notes: Option<&str>,
) -> Result<String, AnalyzeError> {
    let client = Client::new();
    let url = format!("{ollama_url}/api/generate");

    let request = GenerateRequest {
        model: model.to_string(),
        prompt: build_prompt(diff, user_notes),
        stream: false,
    };

    let response = client
        .post(&url)
        .json(&request)
        .send()
        .await?
        .error_for_status()?;

    let body: GenerateResponse = response.json().await?;
    Ok(body.response)
}

pub async fn analyze_streaming<F>(
    diff: &FileDiff,
    ollama_url: &str,
    model: &str,
    user_notes: Option<&str>,
    mut on_token: F,
) -> Result<(), AnalyzeError>
where
    F: FnMut(&str),
{
    let client = Client::new();
    let url = format!("{ollama_url}/api/generate");

    let request = GenerateRequest {
        model: model.to_string(),
        prompt: build_prompt(diff, user_notes),
        stream: true,
    };

    let response = client
        .post(&url)
        .json(&request)
        .send()
        .await?
        .error_for_status()?;

    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if let Ok(text) = std::str::from_utf8(&chunk) {
            for line in text.lines() {
                if let Ok(resp) = serde_json::from_str::<GenerateResponse>(line) {
                    on_token(&resp.response);
                    if resp.done {
                        return Ok(());
                    }
                }
            }
        }
    }

    Ok(())
}
