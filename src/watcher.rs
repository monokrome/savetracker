use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct SaveEvent {
    pub path: PathBuf,
    pub kind: SaveEventKind,
}

#[derive(Debug, Clone)]
pub enum SaveEventKind {
    Modified,
    Created,
}

#[derive(Debug, Error)]
pub enum WatchError {
    #[error("notify error: {0}")]
    Notify(#[from] notify::Error),

    #[error("channel receive error: {0}")]
    Recv(#[from] mpsc::RecvTimeoutError),
}

pub struct SaveWatcher {
    _watcher: RecommendedWatcher,
    rx: mpsc::Receiver<notify::Result<Event>>,
    debounce: Duration,
    pending: HashMap<PathBuf, Instant>,
}

impl SaveWatcher {
    pub fn new(watch_dir: &Path, debounce: Duration) -> Result<Self, WatchError> {
        let (tx, rx) = mpsc::channel();

        let mut watcher = RecommendedWatcher::new(
            move |result| {
                let _ = tx.send(result);
            },
            notify::Config::default(),
        )?;

        watcher.watch(watch_dir, RecursiveMode::Recursive)?;

        Ok(Self {
            _watcher: watcher,
            rx,
            debounce,
            pending: HashMap::new(),
        })
    }

    pub fn poll(&mut self) -> Vec<SaveEvent> {
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
                let kind = if path.exists() {
                    SaveEventKind::Modified
                } else {
                    return false;
                };
                ready.push(SaveEvent {
                    path: path.clone(),
                    kind,
                });
                false
            } else {
                true
            }
        });

        ready
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}
