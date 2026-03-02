use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::watcher::{
    ConnectionState, PathWatcher, WatchError, WatchEvent, WatchEventKind, WatchOptions,
};

pub struct LocalWatcher {
    _watcher: RecommendedWatcher,
    rx: mpsc::Receiver<notify::Result<Event>>,
    debounce: Duration,
    pending: HashMap<PathBuf, Instant>,
    base_dir: PathBuf,
}

impl LocalWatcher {
    pub fn new(dir: &Path, options: &WatchOptions) -> Result<Self, WatchError> {
        let (tx, rx) = mpsc::channel();

        let mut watcher = RecommendedWatcher::new(
            move |result| {
                let _ = tx.send(result);
            },
            notify::Config::default(),
        )
        .map_err(|e| WatchError::Notify(e.to_string()))?;

        watcher
            .watch(dir, RecursiveMode::Recursive)
            .map_err(|e| WatchError::Notify(e.to_string()))?;

        Ok(Self {
            _watcher: watcher,
            rx,
            debounce: options.debounce,
            pending: HashMap::new(),
            base_dir: dir.to_path_buf(),
        })
    }
}

impl PathWatcher for LocalWatcher {
    fn poll(&mut self) -> Result<Vec<WatchEvent>, WatchError> {
        while let Ok(event_result) = self.rx.try_recv() {
            if let Ok(event) = event_result {
                let dominated_by_kind =
                    matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_));
                if !dominated_by_kind {
                    continue;
                }
                for path in event.paths {
                    if path.is_file() {
                        self.pending.insert(path, Instant::now());
                    }
                }
            }
        }

        let now = Instant::now();
        let mut ready = Vec::new();

        self.pending.retain(|path, last_seen| {
            if now.duration_since(*last_seen) >= self.debounce {
                if !path.exists() {
                    return false;
                }
                let kind = WatchEventKind::Modified;
                ready.push(WatchEvent {
                    path: path.to_string_lossy().to_string(),
                    kind,
                });
                false
            } else {
                true
            }
        });

        Ok(ready)
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>, WatchError> {
        let full_path = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.base_dir.join(path)
        };
        std::fs::read(&full_path).map_err(WatchError::Io)
    }

    fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    fn connection_state(&self) -> ConnectionState {
        ConnectionState::Connected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_watcher_reads_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, b"hello").unwrap();

        let opts = WatchOptions::default();
        let mut watcher = LocalWatcher::new(dir.path(), &opts).unwrap();

        let data = watcher.read(&file_path.to_string_lossy()).unwrap();
        assert_eq!(data, b"hello");
    }

    #[test]
    fn local_watcher_always_connected() {
        let dir = tempfile::tempdir().unwrap();
        let opts = WatchOptions::default();
        let watcher = LocalWatcher::new(dir.path(), &opts).unwrap();
        assert_eq!(watcher.connection_state(), ConnectionState::Connected);
    }
}
