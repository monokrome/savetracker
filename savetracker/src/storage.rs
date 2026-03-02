use std::path::Path;

use chrono::{DateTime, Utc};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid version identifier: {0}")]
    InvalidVersion(String),

    #[error("version not found: {0}")]
    NotFound(String),
}

#[derive(Debug, Clone)]
pub struct VersionInfo {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub version: VersionInfo,
    pub data: Vec<u8>,
}

pub trait Storage: Send {
    fn save(&self, file_path: &Path, data: &[u8]) -> Result<Snapshot, StorageError>;

    fn latest(&self, file_path: &Path) -> Result<Option<Snapshot>, StorageError>;

    fn list(&self, file_path: &Path) -> Result<Vec<VersionInfo>, StorageError>;

    fn load(&self, file_path: &Path, version: &str) -> Result<Snapshot, StorageError>;

    fn set_description(
        &self,
        file_path: &Path,
        version: &str,
        description: &str,
    ) -> Result<(), StorageError>;

    fn tracked_files(&self) -> Result<Vec<String>, StorageError>;
}
