use alloc::string::String;
use alloc::vec::Vec;

use chrono::{DateTime, Utc};

#[derive(Debug)]
pub enum StorageError {
    Io(String),
    InvalidVersion(String),
    NotFound(String),
    Backend(String),
}

impl core::fmt::Display for StorageError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::InvalidVersion(v) => write!(f, "invalid version identifier: {v}"),
            Self::NotFound(v) => write!(f, "version not found: {v}"),
            Self::Backend(e) => write!(f, "backend error: {e}"),
        }
    }
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
    fn save(&self, file_path: &str, data: &[u8]) -> Result<Snapshot, StorageError>;

    fn latest(&self, file_path: &str) -> Result<Option<Snapshot>, StorageError>;

    fn list(&self, file_path: &str) -> Result<Vec<VersionInfo>, StorageError>;

    fn load(&self, file_path: &str, version: &str) -> Result<Snapshot, StorageError>;

    fn set_description(
        &self,
        file_path: &str,
        version: &str,
        description: &str,
    ) -> Result<(), StorageError>;

    fn tracked_files(&self) -> Result<Vec<String>, StorageError>;

    fn save_batch(&self, files: &[(&str, &[u8])]) -> Result<Vec<Snapshot>, StorageError> {
        files
            .iter()
            .map(|(path, data)| self.save(path, data))
            .collect()
    }

    fn reviewed_by(&self, _file_path: &str, _version: &str) -> Result<Vec<String>, StorageError> {
        Ok(Vec::new())
    }

    fn mark_reviewed(
        &self,
        _file_path: &str,
        _version: &str,
        _identity: &str,
    ) -> Result<(), StorageError> {
        Ok(())
    }
}
