use thiserror::Error;

use crate::diff::FileDiff;

#[derive(Debug, Error)]
pub enum AnalyzeError {
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),

    #[error("backend error: {0}")]
    Backend(String),

    #[error("missing api key for {0}")]
    MissingApiKey(String),
}

pub trait Analyzer: Send {
    fn analyze(
        &self,
        diff: &FileDiff,
        user_notes: Option<&str>,
    ) -> Result<String, AnalyzeError>;
}

pub fn build_prompt(diff: &FileDiff, user_notes: Option<&str>) -> String {
    let base = format!(
        "You are analyzing changes to a game save file.\n\
         File format: {format}\n\
         Change summary: {summary}\n\n\
         Diff:\n{detail}\n\n",
        format = diff.format,
        summary = diff.summary,
        detail = diff.detail,
    );

    let style = "\
        Respond in markdown format.\n\n\
        First, describe what happened in natural language. Focus on meaningful \
        gameplay events: progression, missions, loot, areas visited, boss kills. \
        Skip trivial stat changes like playtime, timestamps, or fog-of-war updates. \
        Write directly — never say \"The player\" or \"It appears\".\n\n\
        If nothing meaningful changed, just say \"Minor save update\".\n\n\
        After the summary, list all specific field changes under a \"## Changes\" heading \
        using a bullet list.";

    match user_notes {
        Some(notes) => format!(
            "{base}\
             Notes: {notes}\n\n\
             Correlate these notes with the diff. {style}"
        ),
        None => format!("{base}{style}"),
    }
}
