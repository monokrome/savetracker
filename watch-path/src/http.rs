use std::collections::HashMap;
use std::time::Instant;

use crate::url::WatchTarget;
use crate::watcher::{
    ConnectionState, PathWatcher, WatchError, WatchEvent, WatchEventKind, WatchOptions,
};

pub struct HttpWatcher {
    client: reqwest::blocking::Client,
    base_url: String,
    known_etags: HashMap<String, String>,
    last_poll: Instant,
    poll_interval: std::time::Duration,
    loss_timeout: std::time::Duration,
    last_success: Instant,
    pending: Vec<WatchEvent>,
}

impl HttpWatcher {
    pub fn connect(target: &WatchTarget, options: &WatchOptions) -> Result<Self, WatchError> {
        let scheme = match target.protocol {
            crate::url::Protocol::Https => "https",
            _ => "http",
        };
        let host = target
            .host
            .as_deref()
            .ok_or_else(|| WatchError::InvalidUrl("HTTP requires a host".to_string()))?;

        let base_url = match target.port {
            Some(port) => format!("{scheme}://{host}:{port}{}", target.path),
            None => format!("{scheme}://{host}{}", target.path),
        };

        let client = reqwest::blocking::Client::new();

        let response = client
            .head(&base_url)
            .send()
            .map_err(|e| WatchError::Http(e.to_string()))?;

        if !response.status().is_success() {
            return Err(WatchError::Connection(format!(
                "HTTP {}: {}",
                response.status(),
                base_url
            )));
        }

        Ok(Self {
            client,
            base_url,
            known_etags: HashMap::new(),
            last_poll: Instant::now() - options.poll_interval,
            poll_interval: options.poll_interval,
            loss_timeout: options.loss_timeout,
            last_success: Instant::now(),
            pending: Vec::new(),
        })
    }
}

impl PathWatcher for HttpWatcher {
    fn poll(&mut self) -> Result<Vec<WatchEvent>, WatchError> {
        if self.last_poll.elapsed() < self.poll_interval {
            return Ok(Vec::new());
        }
        self.last_poll = Instant::now();

        let response = self
            .client
            .head(&self.base_url)
            .send()
            .map_err(|e| WatchError::Http(e.to_string()))?;

        if !response.status().is_success() {
            return Err(WatchError::Http(format!(
                "HTTP {} from {}",
                response.status(),
                self.base_url
            )));
        }

        self.last_success = Instant::now();

        let etag = response
            .headers()
            .get("etag")
            .or_else(|| response.headers().get("last-modified"))
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();

        if !etag.is_empty() {
            let changed = match self.known_etags.get(&self.base_url) {
                Some(old_etag) => &etag != old_etag,
                None => true,
            };
            if changed {
                let kind = if self.known_etags.contains_key(&self.base_url) {
                    WatchEventKind::Modified
                } else {
                    WatchEventKind::Created
                };
                self.pending.push(WatchEvent {
                    path: self.base_url.clone(),
                    kind,
                });
                self.known_etags.insert(self.base_url.clone(), etag);
            }
        }

        Ok(std::mem::take(&mut self.pending))
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>, WatchError> {
        let url = if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}/{}", self.base_url.trim_end_matches('/'), path)
        };

        let response = self
            .client
            .get(&url)
            .send()
            .map_err(|e| WatchError::Http(e.to_string()))?;

        if !response.status().is_success() {
            return Err(WatchError::Http(format!(
                "HTTP {} reading {}",
                response.status(),
                url
            )));
        }

        self.last_success = Instant::now();
        response
            .bytes()
            .map(|b| b.to_vec())
            .map_err(|e| WatchError::Http(e.to_string()))
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
