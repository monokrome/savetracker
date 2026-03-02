use std::collections::HashMap;
use std::time::Instant;

use suppaftp::FtpStream;

use crate::url::WatchTarget;
use crate::watcher::{
    ConnectionState, PathWatcher, WatchError, WatchEvent, WatchEventKind, WatchOptions,
};

pub struct FtpWatcher {
    stream: FtpStream,
    target: WatchTarget,
    known_mtimes: HashMap<String, String>,
    last_poll: Instant,
    poll_interval: std::time::Duration,
    loss_timeout: std::time::Duration,
    last_success: Instant,
    pending: Vec<WatchEvent>,
}

impl FtpWatcher {
    pub fn connect(target: WatchTarget, options: &WatchOptions) -> Result<Self, WatchError> {
        let host = target
            .host
            .as_deref()
            .ok_or_else(|| WatchError::InvalidUrl("FTP requires a host".to_string()))?;

        let port = target.port.unwrap_or(21);
        let addr = format!("{host}:{port}");

        let mut stream = FtpStream::connect(&addr).map_err(|e| WatchError::Ftp(e.to_string()))?;

        let user = target.user.as_deref().unwrap_or("anonymous");
        let pass = options.password.as_deref().unwrap_or("anonymous@");

        stream
            .login(user, pass)
            .map_err(|e| WatchError::Ftp(e.to_string()))?;

        Ok(Self {
            stream,
            target,
            known_mtimes: HashMap::new(),
            last_poll: Instant::now() - options.poll_interval,
            poll_interval: options.poll_interval,
            loss_timeout: options.loss_timeout,
            last_success: Instant::now(),
            pending: Vec::new(),
        })
    }
}

impl PathWatcher for FtpWatcher {
    fn poll(&mut self) -> Result<Vec<WatchEvent>, WatchError> {
        if self.last_poll.elapsed() < self.poll_interval {
            return Ok(Vec::new());
        }
        self.last_poll = Instant::now();

        let listing = self
            .stream
            .nlst(Some(&self.target.path))
            .map_err(|e| WatchError::Ftp(e.to_string()))?;

        self.last_success = Instant::now();

        let mut current: HashMap<String, String> = HashMap::new();
        for file_path in &listing {
            let mdtm = self
                .stream
                .mdtm(file_path)
                .map(|dt| dt.to_string())
                .unwrap_or_default();
            current.insert(file_path.clone(), mdtm);
        }

        for (file_path, mtime) in &current {
            let changed = match self.known_mtimes.get(file_path) {
                Some(old_mtime) => mtime != old_mtime,
                None => true,
            };
            if changed {
                let kind = if self.known_mtimes.contains_key(file_path) {
                    WatchEventKind::Modified
                } else {
                    WatchEventKind::Created
                };
                self.pending.push(WatchEvent {
                    path: file_path.clone(),
                    kind,
                });
            }
        }

        self.known_mtimes = current;
        Ok(std::mem::take(&mut self.pending))
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>, WatchError> {
        let cursor = self
            .stream
            .retr_as_buffer(path)
            .map_err(|e| WatchError::Ftp(e.to_string()))?;

        self.last_success = Instant::now();
        Ok(cursor.into_inner())
    }

    fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    fn connection_state(&self) -> ConnectionState {
        let elapsed = self.last_success.elapsed();
        if elapsed < self.poll_interval * 2 {
            ConnectionState::Connected
        } else if elapsed < self.loss_timeout {
            ConnectionState::Degraded
        } else {
            ConnectionState::Lost
        }
    }
}
