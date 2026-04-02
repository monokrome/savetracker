use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::patch;
use crate::storage::{Snapshot, Storage, StorageError, VersionInfo};

fn io_err(e: &std::io::Error) -> StorageError {
    StorageError::Io(e.to_string())
}

const DEFAULT_FULL_INTERVAL: Duration = Duration::from_secs(30 * 60);

pub struct CopyStore {
    base_dir: PathBuf,
    max_snapshots: usize,
    full_interval: Duration,
}

impl CopyStore {
    pub fn new(base_dir: PathBuf, max_snapshots: usize) -> Self {
        Self {
            base_dir,
            max_snapshots,
            full_interval: DEFAULT_FULL_INTERVAL,
        }
    }

    pub fn with_full_interval(mut self, interval: Duration) -> Self {
        self.full_interval = interval;
        self
    }

    fn file_dir(&self, file_path: &Path) -> PathBuf {
        let name = file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| crate::UNKNOWN.to_string());
        self.base_dir.join(name)
    }

    fn description_path(entry_path: &Path) -> PathBuf {
        entry_path.with_extension("description")
    }

    fn read_description(entry_path: &Path) -> Option<String> {
        fs::read_to_string(Self::description_path(entry_path))
            .ok()
            .filter(|s| !s.is_empty())
    }

    fn sorted_entries(&self, dir: &Path) -> Result<Vec<Entry>, StorageError> {
        let mut entries: Vec<Entry> = Vec::new();

        for entry in fs::read_dir(dir).map_err(|e| io_err(&e))? {
            let entry = entry.map_err(|e| io_err(&e))?;
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str());
            let stem = path.file_stem().and_then(|s| s.to_str());

            let kind = match ext {
                Some("snapshot") => EntryKind::Full,
                Some("patch") => EntryKind::Patch,
                _ => continue,
            };

            if let Ok(ts) = parse_timestamp(stem) {
                entries.push(Entry {
                    timestamp: ts,
                    path,
                    kind,
                });
            }
        }

        entries.sort_by_key(|e| e.timestamp);
        Ok(entries)
    }

    fn latest_full_timestamp(&self, entries: &[Entry]) -> Option<DateTime<Utc>> {
        entries
            .iter()
            .rev()
            .find(|e| e.kind == EntryKind::Full)
            .map(|e| e.timestamp)
    }

    fn needs_full_snapshot(&self, entries: &[Entry]) -> bool {
        let Some(last_full) = self.latest_full_timestamp(entries) else {
            return true;
        };

        let elapsed = Utc::now()
            .signed_duration_since(last_full)
            .to_std()
            .unwrap_or(Duration::ZERO);

        elapsed >= self.full_interval
    }

    fn reconstruct(&self, entries: &[Entry]) -> Result<Option<Vec<u8>>, StorageError> {
        if entries.is_empty() {
            return Ok(None);
        }

        // Find the last full snapshot
        let full_idx = entries.iter().rposition(|e| e.kind == EntryKind::Full);

        let Some(full_idx) = full_idx else {
            return Err(StorageError::Backend("no full snapshot found".into()));
        };

        let mut data = fs::read(&entries[full_idx].path).map_err(|e| io_err(&e))?;

        // Replay patches after the full snapshot
        for entry in &entries[full_idx + 1..] {
            if entry.kind == EntryKind::Patch {
                let patch_data = fs::read(&entry.path).map_err(|e| io_err(&e))?;
                let p = patch::decode(&patch_data).ok_or_else(|| {
                    StorageError::Backend(format!("corrupt patch: {}", entry.path.display()))
                })?;
                data = patch::apply(&data, &p);
            }
        }

        Ok(Some(data))
    }

    fn reconstruct_at(
        &self,
        entries: &[Entry],
        target_idx: usize,
    ) -> Result<Vec<u8>, StorageError> {
        // Find the last full snapshot at or before target_idx
        let full_idx = entries[..=target_idx]
            .iter()
            .rposition(|e| e.kind == EntryKind::Full)
            .ok_or_else(|| StorageError::Backend("no full snapshot before target".into()))?;

        let mut data = fs::read(&entries[full_idx].path).map_err(|e| io_err(&e))?;

        for entry in &entries[full_idx + 1..=target_idx] {
            if entry.kind == EntryKind::Patch {
                let patch_data = fs::read(&entry.path).map_err(|e| io_err(&e))?;
                let p = patch::decode(&patch_data).ok_or_else(|| {
                    StorageError::Backend(format!("corrupt patch: {}", entry.path.display()))
                })?;
                data = patch::apply(&data, &p);
            }
        }

        Ok(data)
    }

    fn version_info_from(entry: &Entry) -> VersionInfo {
        let id = entry.timestamp.format("%Y%m%d_%H%M%S_%3f").to_string();
        let description = Self::read_description(&entry.path);
        VersionInfo {
            id,
            timestamp: entry.timestamp,
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
        for entry in entries.into_iter().take(to_remove) {
            fs::remove_file(&entry.path).map_err(|e| io_err(&e))?;
            let desc_path = Self::description_path(&entry.path);
            if desc_path.exists() {
                fs::remove_file(desc_path).map_err(|e| io_err(&e))?;
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
enum EntryKind {
    Full,
    Patch,
}

#[derive(Debug)]
struct Entry {
    timestamp: DateTime<Utc>,
    path: PathBuf,
    kind: EntryKind,
}

impl Storage for CopyStore {
    fn save(&self, file_path: &str, data: &[u8]) -> Result<Snapshot, StorageError> {
        let dir = self.file_dir(Path::new(file_path));
        fs::create_dir_all(&dir).map_err(|e| io_err(&e))?;

        let entries = self.sorted_entries(&dir)?;
        let timestamp = Utc::now();
        let id = timestamp.format("%Y%m%d_%H%M%S_%3f").to_string();

        let clock_went_back = entries.last().is_some_and(|e| timestamp <= e.timestamp);

        let use_patch = !clock_went_back
            && !self.needs_full_snapshot(&entries)
            && self.reconstruct(&entries)?.is_some();

        if use_patch {
            let prev_data = self.reconstruct(&entries)?.unwrap();
            if patch::should_patch(&prev_data, data) {
                let p = patch::diff(&prev_data, data);
                let encoded = patch::encode(&p);
                let patch_path = dir.join(format!("{id}.patch"));
                fs::File::create(&patch_path)
                    .map_err(|e| io_err(&e))?
                    .write_all(&encoded)
                    .map_err(|e| io_err(&e))?;
            } else {
                // Truncation or identical — store full
                let snapshot_path = dir.join(format!("{id}.snapshot"));
                fs::File::create(&snapshot_path)
                    .map_err(|e| io_err(&e))?
                    .write_all(data)
                    .map_err(|e| io_err(&e))?;
            }
        } else {
            let snapshot_path = dir.join(format!("{id}.snapshot"));
            fs::File::create(&snapshot_path)
                .map_err(|e| io_err(&e))?
                .write_all(data)
                .map_err(|e| io_err(&e))?;
        }

        self.prune(Path::new(file_path))?;

        Ok(Snapshot {
            version: VersionInfo {
                id,
                timestamp,
                description: None,
            },
            data: data.to_vec(),
        })
    }

    fn latest(&self, file_path: &str) -> Result<Option<Snapshot>, StorageError> {
        let dir = self.file_dir(Path::new(file_path));
        if !dir.exists() {
            return Ok(None);
        }

        let entries = self.sorted_entries(&dir)?;
        let Some(last) = entries.last() else {
            return Ok(None);
        };

        let data = self
            .reconstruct(&entries)?
            .ok_or_else(|| StorageError::Backend("reconstruction failed".into()))?;

        Ok(Some(Snapshot {
            version: Self::version_info_from(last),
            data,
        }))
    }

    fn list(&self, file_path: &str) -> Result<Vec<VersionInfo>, StorageError> {
        let dir = self.file_dir(Path::new(file_path));
        if !dir.exists() {
            return Ok(Vec::new());
        }

        self.sorted_entries(&dir)
            .map(|entries| entries.iter().map(Self::version_info_from).collect())
    }

    fn load(&self, file_path: &str, version: &str) -> Result<Snapshot, StorageError> {
        let dir = self.file_dir(Path::new(file_path));
        let entries = self.sorted_entries(&dir)?;

        let idx = entries
            .iter()
            .position(|e| e.timestamp.format("%Y%m%d_%H%M%S_%3f").to_string() == version)
            .ok_or_else(|| StorageError::NotFound(version.to_string()))?;

        let data = self.reconstruct_at(&entries, idx)?;

        Ok(Snapshot {
            version: Self::version_info_from(&entries[idx]),
            data,
        })
    }

    fn set_description(
        &self,
        file_path: &str,
        version: &str,
        description: &str,
    ) -> Result<(), StorageError> {
        let dir = self.file_dir(Path::new(file_path));
        let entries = self.sorted_entries(&dir)?;

        let entry = entries
            .iter()
            .find(|e| e.timestamp.format("%Y%m%d_%H%M%S_%3f").to_string() == version)
            .ok_or_else(|| StorageError::NotFound(version.to_string()))?;

        let desc_path = Self::description_path(&entry.path);
        fs::File::create(&desc_path)
            .map_err(|e| io_err(&e))?
            .write_all(description.as_bytes())
            .map_err(|e| io_err(&e))?;

        Ok(())
    }

    fn tracked_files(&self) -> Result<Vec<String>, StorageError> {
        if !self.base_dir.exists() {
            return Ok(Vec::new());
        }

        let mut files = Vec::new();
        for entry in fs::read_dir(&self.base_dir).map_err(|e| io_err(&e))? {
            let entry = entry.map_err(|e| io_err(&e))?;
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

    fn store_with_dir(dir: &tempfile::TempDir) -> CopyStore {
        CopyStore::new(dir.path().to_path_buf(), 50)
    }

    #[test]
    fn save_and_retrieve_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_dir(&dir);

        let snapshot = store.save("test.json", b"test data").unwrap();
        assert_eq!(snapshot.data, b"test data");

        let latest = store.latest("test.json").unwrap().unwrap();
        assert_eq!(latest.data, b"test data");
    }

    #[test]
    fn second_save_creates_patch() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_dir(&dir);

        store.save("save.dat", b"version one").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.save("save.dat", b"version two").unwrap();

        // Should have 1 .snapshot and 1 .patch
        let file_dir = dir.path().join("save.dat");
        let snapshots: Vec<_> = fs::read_dir(&file_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("snapshot"))
            .collect();
        let patches: Vec<_> = fs::read_dir(&file_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("patch"))
            .collect();

        assert_eq!(snapshots.len(), 1);
        assert_eq!(patches.len(), 1);

        let latest = store.latest("save.dat").unwrap().unwrap();
        assert_eq!(latest.data, b"version two");
    }

    #[test]
    fn load_intermediate_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_dir(&dir);

        let v1 = store.save("save.dat", b"aaa").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let v2 = store.save("save.dat", b"bbb").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.save("save.dat", b"ccc").unwrap();

        let loaded_v1 = store.load("save.dat", &v1.version.id).unwrap();
        assert_eq!(loaded_v1.data, b"aaa");

        let loaded_v2 = store.load("save.dat", &v2.version.id).unwrap();
        assert_eq!(loaded_v2.data, b"bbb");
    }

    #[test]
    fn truncation_forces_full_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_dir(&dir);

        store.save("save.dat", b"long data here").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.save("save.dat", b"short").unwrap();

        // Both should be full snapshots since truncation can't be patched
        let file_dir = dir.path().join("save.dat");
        let snapshots: Vec<_> = fs::read_dir(&file_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("snapshot"))
            .collect();
        assert_eq!(snapshots.len(), 2);

        let latest = store.latest("save.dat").unwrap().unwrap();
        assert_eq!(latest.data, b"short");
    }

    #[test]
    fn forced_full_after_interval() {
        let dir = tempfile::tempdir().unwrap();
        let store = CopyStore::new(dir.path().to_path_buf(), 50).with_full_interval(Duration::ZERO);

        store.save("save.dat", b"aaa").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.save("save.dat", b"bbb").unwrap();

        // Both should be full since interval is zero
        let file_dir = dir.path().join("save.dat");
        let snapshots: Vec<_> = fs::read_dir(&file_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("snapshot"))
            .collect();
        assert_eq!(snapshots.len(), 2);
    }

    #[test]
    fn set_and_read_description() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_dir(&dir);

        let snapshot = store.save("save.dat", b"data").unwrap();
        store
            .set_description("save.dat", &snapshot.version.id, "cleared dungeon")
            .unwrap();

        let latest = store.latest("save.dat").unwrap().unwrap();
        assert_eq!(
            latest.version.description.as_deref(),
            Some("cleared dungeon")
        );
    }

    #[test]
    fn description_on_patch_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_dir(&dir);

        store.save("save.dat", b"aaa").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let v2 = store.save("save.dat", b"bbb").unwrap();

        store
            .set_description("save.dat", &v2.version.id, "patch description")
            .unwrap();

        let versions = store.list("save.dat").unwrap();
        assert_eq!(
            versions[1].description.as_deref(),
            Some("patch description")
        );
    }

    #[test]
    fn list_includes_both_types() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_dir(&dir);

        store.save("save.dat", b"one").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.save("save.dat", b"two").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.save("save.dat", b"three").unwrap();

        let versions = store.list("save.dat").unwrap();
        assert_eq!(versions.len(), 3);
    }

    #[test]
    fn prune_respects_max() {
        let dir = tempfile::tempdir().unwrap();
        let store = CopyStore::new(dir.path().to_path_buf(), 3);

        for i in 0..5 {
            store
                .save("save.dat", format!("data {i}").as_bytes())
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let entries = store.list("save.dat").unwrap();
        assert_eq!(entries.len(), 3);

        let latest = store.latest("save.dat").unwrap().unwrap();
        assert_eq!(latest.data, b"data 4");
    }

    #[test]
    fn latest_returns_none_for_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_dir(&dir);
        assert!(store.latest("nonexistent.sav").unwrap().is_none());
    }

    #[test]
    fn tracked_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_dir(&dir);

        store.save("alpha.sav", b"a").unwrap();
        store.save("beta.sav", b"b").unwrap();

        let tracked = store.tracked_files().unwrap();
        assert_eq!(tracked, vec!["alpha.sav", "beta.sav"]);
    }

    #[test]
    fn load_nonexistent_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_dir(&dir);
        assert!(store.load("save.dat", "00000000_000000_000").is_err());
    }

    #[test]
    fn set_description_nonexistent_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_dir(&dir);
        assert!(store
            .set_description("save.dat", "00000000_000000_000", "test")
            .is_err());
    }

    #[test]
    fn clock_rollback_forces_full_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_dir(&dir);
        let file_dir = dir.path().join("save.dat");

        // Write a normal first save
        store.save("save.dat", b"first").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));

        // Manually create an entry with a far-future timestamp
        fs::create_dir_all(&file_dir).unwrap();
        let future_name = "29991231_235959_999.snapshot";
        fs::write(file_dir.join(future_name), b"future save").unwrap();

        // Next save should detect clock went back and write full, not patch
        store.save("save.dat", b"after rollback").unwrap();

        // All three should be full snapshots (no patches created despite changes)
        let snapshots: Vec<_> = fs::read_dir(&file_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("snapshot"))
            .collect();
        let patches: Vec<_> = fs::read_dir(&file_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("patch"))
            .collect();

        assert_eq!(snapshots.len(), 3);
        assert_eq!(patches.len(), 0);
    }
}
