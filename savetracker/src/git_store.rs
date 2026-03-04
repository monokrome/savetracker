use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use git2::{Oid, Repository, Signature};

use crate::storage::{Snapshot, Storage, StorageError, VersionInfo};

const NOTES_REF: &str = "refs/notes/savetracker";

pub struct GitStore {
    repo: Repository,
}

impl GitStore {
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        let repo = Repository::open_bare(path).map_err(git_err)?;
        Ok(Self { repo })
    }

    pub fn init(path: &Path) -> Result<Self, StorageError> {
        let repo = Repository::init_bare(path).map_err(git_err)?;
        Ok(Self { repo })
    }

    pub fn open_or_init(path: &Path) -> Result<Self, StorageError> {
        if path.join("HEAD").exists() {
            Self::open(path)
        } else {
            Self::init(path)
        }
    }

    fn signature(&self) -> Result<Signature<'_>, StorageError> {
        Signature::now("savetracker", "savetracker@localhost").map_err(git_err)
    }

    fn head_commit(&self) -> Result<Option<git2::Commit<'_>>, StorageError> {
        match self.repo.head() {
            Ok(reference) => {
                let commit = reference
                    .peel_to_commit()
                    .map_err(git_err)?;
                Ok(Some(commit))
            }
            Err(e) if e.code() == git2::ErrorCode::UnbornBranch => Ok(None),
            Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(e) => Err(git_err(e)),
        }
    }

    fn file_name(file_path: &Path) -> String {
        file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    fn commits_for_file(
        &self,
        file_name: &str,
    ) -> Result<Vec<(Oid, git2::Commit<'_>)>, StorageError> {
        let Some(head) = self.head_commit()? else {
            return Ok(Vec::new());
        };

        let mut revwalk = self.repo.revwalk().map_err(git_err)?;
        revwalk.push(head.id()).map_err(git_err)?;
        revwalk
            .set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE)
            .map_err(git_err)?;

        let mut results = Vec::new();

        for oid_result in revwalk {
            let oid = oid_result.map_err(git_err)?;
            let commit = self.repo.find_commit(oid).map_err(git_err)?;
            let tree = commit.tree().map_err(git_err)?;

            if tree.get_name(file_name).is_some() {
                results.push((oid, commit));
            }
        }

        Ok(results)
    }

    fn version_info_for(&self, commit: &git2::Commit<'_>) -> VersionInfo {
        let id = commit.id().to_string();
        let timestamp = git_time_to_utc(commit.time());
        let description = self.read_note(commit.id());

        VersionInfo {
            id,
            timestamp,
            description,
        }
    }

    fn read_note(&self, oid: Oid) -> Option<String> {
        self.repo
            .find_note(Some(NOTES_REF), oid)
            .ok()
            .and_then(|note| note.message().map(|s| s.to_string()))
            .filter(|s| !s.is_empty())
    }

    fn read_blob(&self, commit: &git2::Commit<'_>, file_name: &str) -> Result<Vec<u8>, StorageError> {
        let tree = commit.tree().map_err(git_err)?;
        let entry = tree
            .get_name(file_name)
            .ok_or_else(|| StorageError::NotFound(file_name.to_string()))?;

        let blob = self
            .repo
            .find_blob(entry.id())
            .map_err(git_err)?;

        Ok(blob.content().to_vec())
    }
}

impl Storage for GitStore {
    fn save(&self, file_path: &Path, data: &[u8]) -> Result<Snapshot, StorageError> {
        let file_name = Self::file_name(file_path);
        let sig = self.signature()?;

        let blob_oid = self.repo.blob(data).map_err(git_err)?;

        let mut tree_builder = self.repo.treebuilder(None).map_err(git_err)?;
        tree_builder
            .insert(&file_name, blob_oid, 0o100644)
            .map_err(git_err)?;
        let tree_oid = tree_builder.write().map_err(git_err)?;
        let tree = self.repo.find_tree(tree_oid).map_err(git_err)?;

        let parent = self.head_commit()?;
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();

        let message = format!("save {file_name}");
        let commit_oid = self
            .repo
            .commit(Some("HEAD"), &sig, &sig, &message, &tree, &parents)
            .map_err(git_err)?;

        let commit = self.repo.find_commit(commit_oid).map_err(git_err)?;
        let timestamp = git_time_to_utc(commit.time());

        Ok(Snapshot {
            version: VersionInfo {
                id: commit_oid.to_string(),
                timestamp,
                description: None,
            },
            data: data.to_vec(),
        })
    }

    fn latest(&self, file_path: &Path) -> Result<Option<Snapshot>, StorageError> {
        let file_name = Self::file_name(file_path);
        let commits = self.commits_for_file(&file_name)?;

        let Some((_, commit)) = commits.last() else {
            return Ok(None);
        };

        let data = self.read_blob(commit, &file_name)?;
        let info = self.version_info_for(commit);

        Ok(Some(Snapshot {
            version: info,
            data,
        }))
    }

    fn list(&self, file_path: &Path) -> Result<Vec<VersionInfo>, StorageError> {
        let file_name = Self::file_name(file_path);
        let commits = self.commits_for_file(&file_name)?;

        Ok(commits
            .iter()
            .map(|(_, commit)| self.version_info_for(commit))
            .collect())
    }

    fn load(&self, file_path: &Path, version: &str) -> Result<Snapshot, StorageError> {
        let file_name = Self::file_name(file_path);
        let oid =
            Oid::from_str(version).map_err(|_| StorageError::InvalidVersion(version.to_string()))?;

        let commit = self
            .repo
            .find_commit(oid)
            .map_err(|_| StorageError::NotFound(version.to_string()))?;

        let data = self.read_blob(&commit, &file_name)?;
        let info = self.version_info_for(&commit);

        Ok(Snapshot {
            version: info,
            data,
        })
    }

    fn set_description(
        &self,
        _file_path: &Path,
        version: &str,
        description: &str,
    ) -> Result<(), StorageError> {
        let oid =
            Oid::from_str(version).map_err(|_| StorageError::InvalidVersion(version.to_string()))?;

        self.repo
            .find_commit(oid)
            .map_err(|_| StorageError::NotFound(version.to_string()))?;

        let sig = self.signature()?;
        self.repo
            .note(&sig, &sig, Some(NOTES_REF), oid, description, true)
            .map_err(git_err)?;

        Ok(())
    }

    fn save_batch(&self, files: &[(&Path, &[u8])]) -> Result<Vec<Snapshot>, StorageError> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        if files.len() == 1 {
            return self.save(files[0].0, files[0].1).map(|s| vec![s]);
        }

        let sig = self.signature()?;
        let mut tree_builder = self.repo.treebuilder(None).map_err(git_err)?;
        let mut file_data: Vec<(String, Vec<u8>)> = Vec::with_capacity(files.len());

        for (path, data) in files {
            let name = Self::file_name(path);
            let blob_oid = self.repo.blob(data).map_err(git_err)?;
            tree_builder
                .insert(&name, blob_oid, 0o100644)
                .map_err(git_err)?;
            file_data.push((name, data.to_vec()));
        }

        let tree_oid = tree_builder.write().map_err(git_err)?;
        let tree = self.repo.find_tree(tree_oid).map_err(git_err)?;

        let parent = self.head_commit()?;
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();

        let names: Vec<&str> = file_data.iter().map(|(n, _)| n.as_str()).collect();
        let message = format!("save {}", names.join(", "));

        let commit_oid = self
            .repo
            .commit(Some("HEAD"), &sig, &sig, &message, &tree, &parents)
            .map_err(git_err)?;

        let commit = self.repo.find_commit(commit_oid).map_err(git_err)?;
        let timestamp = git_time_to_utc(commit.time());
        let id = commit_oid.to_string();

        Ok(file_data
            .into_iter()
            .map(|(_, data)| Snapshot {
                version: VersionInfo {
                    id: id.clone(),
                    timestamp,
                    description: None,
                },
                data,
            })
            .collect())
    }

    fn tracked_files(&self) -> Result<Vec<String>, StorageError> {
        let Some(head) = self.head_commit()? else {
            return Ok(Vec::new());
        };

        let mut revwalk = self.repo.revwalk().map_err(git_err)?;
        revwalk.push(head.id()).map_err(git_err)?;

        let mut files = std::collections::BTreeSet::new();

        for oid_result in revwalk {
            let oid = oid_result.map_err(git_err)?;
            let commit = self.repo.find_commit(oid).map_err(git_err)?;
            let tree = commit.tree().map_err(git_err)?;

            for entry in tree.iter() {
                if let Some(name) = entry.name() {
                    files.insert(name.to_string());
                }
            }
        }

        Ok(files.into_iter().collect())
    }
}

#[allow(clippy::needless_pass_by_value)]
fn git_err(e: git2::Error) -> StorageError {
    StorageError::Backend(e.message().to_string())
}

fn git_time_to_utc(time: git2::Time) -> DateTime<Utc> {
    Utc.timestamp_opt(time.seconds(), 0)
        .single()
        .unwrap_or_else(Utc::now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, GitStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = GitStore::init(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn save_and_latest_roundtrip() {
        let (_dir, store) = temp_store();
        let path = Path::new("test_save.dat");
        let data = b"hello world";

        let snapshot = store.save(path, data).unwrap();
        assert_eq!(snapshot.data, data);
        assert!(snapshot.version.description.is_none());
        assert_eq!(snapshot.version.id.len(), 40);

        let latest = store.latest(path).unwrap().unwrap();
        assert_eq!(latest.data, data);
        assert_eq!(latest.version.id, snapshot.version.id);
    }

    #[test]
    fn save_and_load_by_sha() {
        let (_dir, store) = temp_store();
        let path = Path::new("save.dat");

        let snapshot = store.save(path, b"version one").unwrap();
        let loaded = store.load(path, &snapshot.version.id).unwrap();
        assert_eq!(loaded.data, b"version one");
        assert_eq!(loaded.version.id, snapshot.version.id);
    }

    #[test]
    fn list_returns_chronological_versions() {
        let (_dir, store) = temp_store();
        let path = Path::new("save.dat");

        let v1 = store.save(path, b"one").unwrap();
        let v2 = store.save(path, b"two").unwrap();
        let v3 = store.save(path, b"three").unwrap();

        let versions = store.list(path).unwrap();
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0].id, v1.version.id);
        assert_eq!(versions[1].id, v2.version.id);
        assert_eq!(versions[2].id, v3.version.id);
    }

    #[test]
    fn set_and_read_description() {
        let (_dir, store) = temp_store();
        let path = Path::new("save.dat");

        let snapshot = store.save(path, b"data").unwrap();
        assert!(snapshot.version.description.is_none());

        store
            .set_description(path, &snapshot.version.id, "cleared the dungeon")
            .unwrap();

        let latest = store.latest(path).unwrap().unwrap();
        assert_eq!(
            latest.version.description.as_deref(),
            Some("cleared the dungeon")
        );

        let loaded = store.load(path, &snapshot.version.id).unwrap();
        assert_eq!(
            loaded.version.description.as_deref(),
            Some("cleared the dungeon")
        );
    }

    #[test]
    fn description_in_list() {
        let (_dir, store) = temp_store();
        let path = Path::new("save.dat");

        let v1 = store.save(path, b"one").unwrap();
        let _v2 = store.save(path, b"two").unwrap();

        store
            .set_description(path, &v1.version.id, "first save")
            .unwrap();

        let versions = store.list(path).unwrap();
        assert_eq!(versions[0].description.as_deref(), Some("first save"));
        assert!(versions[1].description.is_none());
    }

    #[test]
    fn tracked_files_multiple() {
        let (_dir, store) = temp_store();

        store.save(Path::new("alpha.sav"), b"a").unwrap();
        store.save(Path::new("beta.sav"), b"b").unwrap();

        let tracked = store.tracked_files().unwrap();
        assert_eq!(tracked, vec!["alpha.sav", "beta.sav"]);
    }

    #[test]
    fn interleaved_saves() {
        let (_dir, store) = temp_store();
        let path_a = Path::new("a.sav");
        let path_b = Path::new("b.sav");

        store.save(path_a, b"a1").unwrap();
        store.save(path_b, b"b1").unwrap();
        store.save(path_a, b"a2").unwrap();

        let a_versions = store.list(path_a).unwrap();
        assert_eq!(a_versions.len(), 2);

        let b_versions = store.list(path_b).unwrap();
        assert_eq!(b_versions.len(), 1);

        let latest_a = store.latest(path_a).unwrap().unwrap();
        assert_eq!(latest_a.data, b"a2");
    }

    #[test]
    fn latest_returns_none_for_unknown() {
        let (_dir, store) = temp_store();
        let result = store.latest(Path::new("nonexistent.sav")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_nonexistent_version_returns_error() {
        let (_dir, store) = temp_store();
        let fake_sha = "a".repeat(40);
        let result = store.load(Path::new("save.dat"), &fake_sha);
        assert!(result.is_err());
    }

    #[test]
    fn set_description_nonexistent_version_returns_error() {
        let (_dir, store) = temp_store();
        let fake_sha = "b".repeat(40);
        let result = store.set_description(Path::new("save.dat"), &fake_sha, "test");
        assert!(result.is_err());
    }

    #[test]
    fn open_or_init_creates_new() {
        let dir = tempfile::tempdir().unwrap();
        let store = GitStore::open_or_init(dir.path()).unwrap();
        store.save(Path::new("test.dat"), b"data").unwrap();

        let latest = store.latest(Path::new("test.dat")).unwrap();
        assert!(latest.is_some());
    }

    #[test]
    fn open_or_init_reopens_existing() {
        let dir = tempfile::tempdir().unwrap();

        {
            let store = GitStore::init(dir.path()).unwrap();
            store.save(Path::new("test.dat"), b"persisted").unwrap();
        }

        let store = GitStore::open_or_init(dir.path()).unwrap();
        let latest = store.latest(Path::new("test.dat")).unwrap().unwrap();
        assert_eq!(latest.data, b"persisted");
    }

    #[test]
    fn empty_repo_operations() {
        let (_dir, store) = temp_store();

        assert!(store.latest(Path::new("any.dat")).unwrap().is_none());
        assert!(store.list(Path::new("any.dat")).unwrap().is_empty());
        assert!(store.tracked_files().unwrap().is_empty());
    }

    #[test]
    fn invalid_version_format() {
        let (_dir, store) = temp_store();
        let result = store.load(Path::new("save.dat"), "not-a-sha");
        assert!(matches!(result, Err(StorageError::InvalidVersion(_))));
    }

    #[test]
    fn save_batch_creates_single_commit() {
        let (_dir, store) = temp_store();
        let path_a = Path::new("a.sav");
        let path_b = Path::new("b.sav");

        let files: Vec<(&Path, &[u8])> = vec![(path_a, b"alpha"), (path_b, b"beta")];
        let snapshots = store.save_batch(&files).unwrap();

        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].data, b"alpha");
        assert_eq!(snapshots[1].data, b"beta");

        // Both snapshots share the same commit SHA
        assert_eq!(snapshots[0].version.id, snapshots[1].version.id);

        // Both files are loadable from that commit
        let loaded_a = store.load(path_a, &snapshots[0].version.id).unwrap();
        assert_eq!(loaded_a.data, b"alpha");

        let loaded_b = store.load(path_b, &snapshots[0].version.id).unwrap();
        assert_eq!(loaded_b.data, b"beta");
    }

    #[test]
    fn save_batch_empty() {
        let (_dir, store) = temp_store();
        let files: Vec<(&Path, &[u8])> = vec![];
        let snapshots = store.save_batch(&files).unwrap();
        assert!(snapshots.is_empty());
    }

    #[test]
    fn save_batch_single_delegates_to_save() {
        let (_dir, store) = temp_store();
        let path = Path::new("only.sav");

        let files: Vec<(&Path, &[u8])> = vec![(path, b"solo")];
        let snapshots = store.save_batch(&files).unwrap();

        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].data, b"solo");

        let latest = store.latest(path).unwrap().unwrap();
        assert_eq!(latest.data, b"solo");
    }

    #[test]
    fn save_batch_then_list_per_file() {
        let (_dir, store) = temp_store();
        let path_a = Path::new("a.sav");
        let path_b = Path::new("b.sav");

        // First individual save
        store.save(path_a, b"a1").unwrap();

        // Then a batch save
        let files: Vec<(&Path, &[u8])> = vec![(path_a, b"a2"), (path_b, b"b1")];
        store.save_batch(&files).unwrap();

        // a.sav should have 2 versions, b.sav should have 1
        let a_versions = store.list(path_a).unwrap();
        assert_eq!(a_versions.len(), 2);

        let b_versions = store.list(path_b).unwrap();
        assert_eq!(b_versions.len(), 1);

        let latest_a = store.latest(path_a).unwrap().unwrap();
        assert_eq!(latest_a.data, b"a2");
    }

    #[test]
    fn description_overwrite() {
        let (_dir, store) = temp_store();
        let path = Path::new("save.dat");
        let snapshot = store.save(path, b"data").unwrap();

        store
            .set_description(path, &snapshot.version.id, "first note")
            .unwrap();
        store
            .set_description(path, &snapshot.version.id, "updated note")
            .unwrap();

        let loaded = store.load(path, &snapshot.version.id).unwrap();
        assert_eq!(
            loaded.version.description.as_deref(),
            Some("updated note")
        );
    }
}
