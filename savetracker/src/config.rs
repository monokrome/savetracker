use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub watch_url: String,
    pub snapshot_dir: PathBuf,
    pub ollama_url: String,
    pub model: String,
    pub debounce: Duration,
    pub max_snapshots: usize,
}

impl Config {
    pub fn new(watch_url: String) -> Self {
        Self {
            watch_url,
            snapshot_dir: PathBuf::from(".savetracker").join("snapshots"),
            ollama_url: "http://localhost:11434".to_string(),
            model: "mistral".to_string(),
            debounce: Duration::from_secs(2),
            max_snapshots: 50,
        }
    }

    pub fn with_snapshot_dir(mut self, dir: PathBuf) -> Self {
        self.snapshot_dir = dir;
        self
    }

    pub fn with_ollama_url(mut self, url: String) -> Self {
        self.ollama_url = url;
        self
    }

    pub fn with_model(mut self, model: String) -> Self {
        self.model = model;
        self
    }

    pub fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    pub fn with_max_snapshots(mut self, max: usize) -> Self {
        self.max_snapshots = max;
        self
    }
}
