use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub path: String,
    pub kind: WatchEventKind,
}

#[derive(Debug, Clone)]
pub enum WatchEventKind {
    Modified,
    Created,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    Degraded,
    Lost,
}

#[derive(Debug, Error)]
pub enum WatchError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("connection failed: {0}")]
    Connection(String),

    #[error("unsupported protocol: {0}")]
    UnsupportedProtocol(String),

    #[error("invalid url: {0}")]
    InvalidUrl(String),

    #[error("notify error: {0}")]
    Notify(String),

    #[error("ssh error: {0}")]
    Ssh(String),

    #[error("ftp error: {0}")]
    Ftp(String),

    #[error("http error: {0}")]
    Http(String),
}

#[derive(Debug, Clone)]
pub struct WatchOptions {
    pub debounce: Duration,
    pub poll_interval: Duration,
    pub loss_timeout: Duration,
    pub password: Option<String>,
    pub key_path: Option<PathBuf>,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            debounce: Duration::from_secs(2),
            poll_interval: Duration::from_secs(5),
            loss_timeout: Duration::from_secs(30),
            password: None,
            key_path: None,
        }
    }
}

pub trait PathWatcher {
    fn poll(&mut self) -> Result<Vec<WatchEvent>, WatchError>;
    fn read(&mut self, path: &str) -> Result<Vec<u8>, WatchError>;
    fn has_pending(&self) -> bool;
    fn connection_state(&self) -> ConnectionState;
}
