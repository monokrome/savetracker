use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::storage::{Snapshot, Storage, StorageError, VersionInfo};

pub struct CopyStore {
    base_dir: PathBuf,
    max_snapshots: usize,
}

impl CopyStore {
    pub fn new(base_dir: PathBuf, max_snapshots: usize) -> Self {
        Self {
            base_dir,
            max_snapshots,
        }
    }

    fn file_dir(&self, file_path: &Path) -> PathBuf {
        let name = file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        self.base_dir.join(name)
    }

    fn description_path(snapshot_path: &Path) -> PathBuf {
        snapshot_path.with_extension("description")
    }

    fn read_description(snapshot_path: &Path) -> Option<String> {
        let desc_path = Self::description_path(snapshot_path);
        fs::read_to_string(&desc_path)
            .ok()
            .filter(|s| !s.is_empty())
    }

    fn sorted_entries(&self, dir: &Path) -> Result<Vec<(DateTime<Utc>, PathBuf)>, StorageError> {
        let mut entries: Vec<(DateTime<Utc>, PathBuf)> = Vec::new();

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("snapshot") {
                if let Ok(ts) = parse_timestamp(path.file_stem().and_then(|s| s.to_str())) {
                    entries.push((ts, path));
                }
            }
        }

        entries.sort_by_key(|(ts, _)| *ts);
        Ok(entries)
    }

    fn version_info_from(ts: DateTime<Utc>, snapshot_path: &Path) -> VersionInfo {
        let id = ts.format("%Y%m%d_%H%M%S_%3f").to_string();
        let description = Self::read_description(snapshot_path);
        VersionInfo {
            id,
            timestamp: ts,
            description,
        }
    }

    fn prune(&self, file_path: &Path) -> Result<(), StorageError> {
        let dir = self.file_dir(file_path);
        let entries = self.sorted_entries(&dir)?;

        if entries.len() <= self.max_snapshots {
            return Ok(());
        }

        let to_remove = entries.len() - self.max_snapshots;
        for (_, path) in entries.into_iter().take(to_remove) {
            fs::remove_file(&path)?;
            let desc_path = Self::description_path(&path);
            if desc_path.exists() {
                fs::remove_file(desc_path)?;
            }
        }

        Ok(())
    }
}

impl Storage for CopyStore {
    fn save(&self, file_path: &Path, data: &[u8]) -> Result<Snapshot, StorageError> {
        let dir = self.file_dir(file_path);
        fs::create_dir_all(&dir)?;

        let timestamp = Utc::now();
        let id = timestamp.format("%Y%m%d_%H%M%S_%3f").to_string();
        let snapshot_path = dir.join(format!("{id}.snapshot"));

        let mut file = fs::File::create(&snapshot_path)?;
        file.write_all(data)?;

        self.prune(file_path)?;

        Ok(Snapshot {
            version: VersionInfo {
                id,
                timestamp,
                description: None,
            },
            data: data.to_vec(),
        })
    }

    fn latest(&self, file_path: &Path) -> Result<Option<Snapshot>, StorageError> {
        let dir = self.file_dir(file_path);
        if !dir.exists() {
            return Ok(None);
        }

        let entries = self.sorted_entries(&dir)?;
        let Some((ts, path)) = entries.last() else {
            return Ok(None);
        };

        let mut file = fs::File::open(path)?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;

        Ok(Some(Snapshot {
            version: Self::version_info_from(*ts, path),
            data,
        }))
    }

    fn list(&self, file_path: &Path) -> Result<Vec<VersionInfo>, StorageError> {
        let dir = self.file_dir(file_path);
        if !dir.exists() {
            return Ok(Vec::new());
        }

        self.sorted_entries(&dir).map(|entries| {
            entries
                .iter()
                .map(|(ts, path)| Self::version_info_from(*ts, path))
                .collect()
        })
    }

    fn load(&self, file_path: &Path, version: &str) -> Result<Snapshot, StorageError> {
        let dir = self.file_dir(file_path);
        let path = dir.join(format!("{version}.snapshot"));

        if !path.exists() {
            return Err(StorageError::NotFound(version.to_string()));
        }

        let mut file = fs::File::open(&path)?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;

        let timestamp = parse_timestamp(Some(version))?;

        Ok(Snapshot {
            version: Self::version_info_from(timestamp, &path),
            data,
        })
    }

    fn set_description(
        &self,
        file_path: &Path,
        version: &str,
        description: &str,
    ) -> Result<(), StorageError> {
        let dir = self.file_dir(file_path);
        let snapshot_path = dir.join(format!("{version}.snapshot"));

        if !snapshot_path.exists() {
            return Err(StorageError::NotFound(version.to_string()));
        }

        let desc_path = Self::description_path(&snapshot_path);
        let mut file = fs::File::create(&desc_path)?;
        file.write_all(description.as_bytes())?;

        Ok(())
    }

    fn tracked_files(&self) -> Result<Vec<String>, StorageError> {
        if !self.base_dir.exists() {
            return Ok(Vec::new());
        }

        let mut files = Vec::new();
        for entry in fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            if entry.path().is_dir() {
                files.push(entry.file_name().to_string_lossy().to_string());
            }
        }

        files.sort();
        Ok(files)
    }
}

fn parse_timestamp(stem: Option<&str>) -> Result<DateTime<Utc>, StorageError> {
    let stem = stem.ok_or_else(|| StorageError::InvalidVersion("empty".to_string()))?;

    chrono::NaiveDateTime::parse_from_str(stem, "%Y%m%d_%H%M%S_%3f")
        .map(|naive| naive.and_utc())
        .map_err(|_| StorageError::InvalidVersion(stem.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_retrieve_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = CopyStore::new(dir.path().to_path_buf(), 50);
        let file_path = Path::new("test_save.json");
        let data = b"test data content";

        let snapshot = store.save(file_path, data).unwrap();
        assert_eq!(snapshot.data, data);
        assert!(snapshot.version.description.is_none());

        let latest = store.latest(file_path).unwrap().unwrap();
        assert_eq!(latest.data, data);
    }

    #[test]
    fn set_and_read_description() {
        let dir = tempfile::tempdir().unwrap();
        let store = CopyStore::new(dir.path().to_path_buf(), 50);
        let file_path = Path::new("save.dat");

        let snapshot = store.save(file_path, b"data").unwrap();
        assert!(snapshot.version.description.is_none());

        store
            .set_description(file_path, &snapshot.version.id, "cleared the dungeon")
            .unwrap();

        let latest = store.latest(file_path).unwrap().unwrap();
        assert_eq!(
            latest.version.description.as_deref(),
            Some("cleared the dungeon")
        );

        let loaded = store.load(file_path, &snapshot.version.id).unwrap();
        assert_eq!(
            loaded.version.description.as_deref(),
            Some("cleared the dungeon")
        );
    }

    #[test]
    fn list_includes_descriptions() {
        let dir = tempfile::tempdir().unwrap();
        let store = CopyStore::new(dir.path().to_path_buf(), 50);
        let file_path = Path::new("save.dat");

        let v1 = store.save(file_path, b"one").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let _v2 = store.save(file_path, b"two").unwrap();

        store
            .set_description(file_path, &v1.version.id, "first save")
            .unwrap();

        let versions = store.list(file_path).unwrap();
        assert_eq!(versions[0].description.as_deref(), Some("first save"));
        assert!(versions[1].description.is_none());
    }

    #[test]
    fn prune_removes_description_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let store = CopyStore::new(dir.path().to_path_buf(), 2);
        let file_path = Path::new("save.dat");

        let v1 = store.save(file_path, b"one").unwrap();
        store
            .set_description(file_path, &v1.version.id, "will be pruned")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));

        store.save(file_path, b"two").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.save(file_path, b"three").unwrap();

        let versions = store.list(file_path).unwrap();
        assert_eq!(versions.len(), 2);

        let result = store.load(file_path, &v1.version.id);
        assert!(result.is_err());
    }

    #[test]
    fn prune_old_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let store = CopyStore::new(dir.path().to_path_buf(), 3);
        let file_path = Path::new("save.dat");

        for i in 0..5 {
            store
                .save(file_path, format!("data {i}").as_bytes())
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let entries = store.list(file_path).unwrap();
        assert_eq!(entries.len(), 3);

        let latest = store.latest(file_path).unwrap().unwrap();
        assert_eq!(latest.data, b"data 4");
    }

    #[test]
    fn latest_returns_none_for_unknown_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = CopyStore::new(dir.path().to_path_buf(), 50);

        let result = store.latest(Path::new("nonexistent.sav")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_by_version_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = CopyStore::new(dir.path().to_path_buf(), 50);
        let file_path = Path::new("save.dat");

        let snapshot = store.save(file_path, b"version one").unwrap();
        let loaded = store.load(file_path, &snapshot.version.id).unwrap();
        assert_eq!(loaded.data, b"version one");
    }

    #[test]
    fn tracked_files_lists_saved_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = CopyStore::new(dir.path().to_path_buf(), 50);

        store.save(Path::new("alpha.sav"), b"a").unwrap();
        store.save(Path::new("beta.sav"), b"b").unwrap();

        let tracked = store.tracked_files().unwrap();
        assert_eq!(tracked, vec!["alpha.sav", "beta.sav"]);
    }

    #[test]
    fn load_nonexistent_version_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = CopyStore::new(dir.path().to_path_buf(), 50);

        let result = store.load(Path::new("save.dat"), "00000000_000000_000");
        assert!(result.is_err());
    }

    #[test]
    fn set_description_nonexistent_version_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = CopyStore::new(dir.path().to_path_buf(), 50);

        let result = store.set_description(Path::new("save.dat"), "00000000_000000_000", "test");
        assert!(result.is_err());
    }
}
