use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

use watch_path::{PathWatcher, WatchEvent};

const BATCH_WINDOW: Duration = Duration::from_millis(500);

pub struct FileChange {
    pub path: String,
    pub data: Vec<u8>,
}

pub fn drain_and_batch(
    watcher: &mut dyn PathWatcher,
    initial_events: Vec<WatchEvent>,
    watch_url: &str,
) -> Result<Vec<Vec<FileChange>>, watch_path::WatchError> {
    let mut all_events = initial_events;

    std::thread::sleep(BATCH_WINDOW);
    let more = watcher.poll()?;
    all_events.extend(more);

    // Deduplicate by path (keep last occurrence)
    let mut seen = std::collections::HashSet::new();
    let mut unique = Vec::new();
    for event in all_events.into_iter().rev() {
        if seen.insert(event.path.clone()) {
            unique.push(event);
        }
    }
    unique.reverse();

    // Read data and resolve mtimes
    let mut changes: Vec<(Option<i64>, FileChange)> = Vec::with_capacity(unique.len());

    for event in unique {
        let data = watcher.read(&event.path)?;
        let mtime_key = resolve_mtime(watch_url, &event.path);
        changes.push((mtime_key, FileChange { path: event.path, data }));
    }

    // Group by mtime (truncated to seconds). None-mtime entries go into one group.
    if changes.iter().all(|(m, _)| m.is_none()) || changes.len() <= 1 {
        let batch = changes.into_iter().map(|(_, c)| c).collect();
        return Ok(vec![batch]);
    }

    let mut groups: BTreeMap<i64, Vec<FileChange>> = BTreeMap::new();
    let mut no_mtime = Vec::new();

    for (mtime_key, change) in changes {
        match mtime_key {
            Some(key) => groups.entry(key).or_default().push(change),
            None => no_mtime.push(change),
        }
    }

    let mut result: Vec<Vec<FileChange>> = groups.into_values().collect();
    if !no_mtime.is_empty() {
        result.push(no_mtime);
    }

    Ok(result)
}

fn resolve_mtime(watch_url: &str, event_path: &str) -> Option<i64> {
    let base = watch_url
        .strip_prefix("file://")
        .unwrap_or(watch_url);

    let full_path = Path::new(base).join(event_path);

    std::fs::metadata(&full_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|t| {
            t.duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_mtime_local_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let dir = tmp.path().parent().unwrap();
        let name = tmp.path().file_name().unwrap().to_str().unwrap();

        let mtime = resolve_mtime(&dir.to_string_lossy(), name);
        assert!(mtime.is_some());
    }

    #[test]
    fn resolve_mtime_nonexistent() {
        let mtime = resolve_mtime("/no/such/dir", "missing.file");
        assert!(mtime.is_none());
    }

    #[test]
    fn resolve_mtime_remote_url() {
        let mtime = resolve_mtime("ssh://host/path", "file.sav");
        assert!(mtime.is_none());
    }
}
