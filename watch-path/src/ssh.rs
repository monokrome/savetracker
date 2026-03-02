use std::collections::HashMap;
use std::io::Read;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use ssh2::Session;

use crate::url::WatchTarget;
use crate::watcher::{
    ConnectionState, PathWatcher, WatchError, WatchEvent, WatchEventKind, WatchOptions,
};

enum WatchMode {
    InotifyPush {
        channel: ssh2::Channel,
        buf: Vec<u8>,
    },
    StatPoll {
        known_mtimes: HashMap<String, i64>,
        last_poll: Instant,
    },
}

pub struct SshWatcher {
    session: Session,
    target: WatchTarget,
    mode: WatchMode,
    pending: Vec<WatchEvent>,
    poll_interval: Duration,
    loss_timeout: Duration,
    last_success: Instant,
}

impl SshWatcher {
    pub fn connect(target: WatchTarget, options: &WatchOptions) -> Result<Self, WatchError> {
        let host = target
            .host
            .as_deref()
            .ok_or_else(|| WatchError::InvalidUrl("SSH requires a host".to_string()))?;
        let port = target.port.unwrap_or(22);

        let tcp = TcpStream::connect(format!("{host}:{port}"))
            .map_err(|e| WatchError::Connection(e.to_string()))?;

        let mut session = Session::new().map_err(|e| WatchError::Ssh(e.to_string()))?;
        session.set_tcp_stream(tcp);
        session
            .handshake()
            .map_err(|e| WatchError::Ssh(e.to_string()))?;

        let user = target.user.as_deref().unwrap_or("root");
        authenticate(&session, user, options)?;

        let mode = try_inotifywait(&session, &target.path).unwrap_or_else(|| WatchMode::StatPoll {
            known_mtimes: HashMap::new(),
            last_poll: Instant::now() - options.poll_interval,
        });

        Ok(Self {
            session,
            target,
            mode,
            pending: Vec::new(),
            poll_interval: options.poll_interval,
            loss_timeout: options.loss_timeout,
            last_success: Instant::now(),
        })
    }
}

fn authenticate(session: &Session, user: &str, options: &WatchOptions) -> Result<(), WatchError> {
    if let Some(key_path) = &options.key_path {
        session
            .userauth_pubkey_file(user, None, key_path, options.password.as_deref())
            .map_err(|e| WatchError::Ssh(format!("key auth failed: {e}")))?;
    } else if let Some(password) = &options.password {
        session
            .userauth_password(user, password)
            .map_err(|e| WatchError::Ssh(format!("password auth failed: {e}")))?;
    } else {
        session
            .userauth_agent(user)
            .map_err(|e| WatchError::Ssh(format!("agent auth failed: {e}")))?;
    }
    Ok(())
}

fn try_inotifywait(session: &Session, path: &str) -> Option<WatchMode> {
    let mut check = session.channel_session().ok()?;
    check.exec("which inotifywait").ok()?;
    let mut output = String::new();
    check.read_to_string(&mut output).ok()?;
    check.wait_close().ok()?;
    if check.exit_status().ok()? != 0 {
        return None;
    }

    let mut channel = session.channel_session().ok()?;
    let cmd = format!("inotifywait -m -r --format '%w%f %e' '{path}'");
    channel.exec(&cmd).ok()?;

    Some(WatchMode::InotifyPush {
        channel,
        buf: Vec::new(),
    })
}

impl PathWatcher for SshWatcher {
    fn poll(&mut self) -> Result<Vec<WatchEvent>, WatchError> {
        match &mut self.mode {
            WatchMode::InotifyPush { channel, buf } => {
                self.session.set_blocking(false);
                let mut tmp = [0u8; 4096];
                loop {
                    match channel.read(&mut tmp) {
                        Ok(0) => break,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(e) => {
                            self.session.set_blocking(true);
                            return Err(WatchError::Ssh(e.to_string()));
                        }
                    }
                }
                self.session.set_blocking(true);

                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line = String::from_utf8_lossy(&buf[..pos]).to_string();
                    buf.drain(..=pos);
                    if let Some(event) = parse_inotify_line(&line) {
                        self.pending.push(event);
                    }
                }

                if !self.pending.is_empty() {
                    self.last_success = Instant::now();
                }
            }
            WatchMode::StatPoll {
                known_mtimes,
                last_poll,
            } => {
                if last_poll.elapsed() < self.poll_interval {
                    return Ok(Vec::new());
                }
                *last_poll = Instant::now();

                let path = self.target.path.clone();
                let mut channel = self
                    .session
                    .channel_session()
                    .map_err(|e| WatchError::Ssh(e.to_string()))?;

                let cmd = format!("find '{path}' -type f -printf '%p %T@\\n'");
                channel
                    .exec(&cmd)
                    .map_err(|e| WatchError::Ssh(e.to_string()))?;

                let mut output = String::new();
                channel
                    .read_to_string(&mut output)
                    .map_err(|e| WatchError::Ssh(e.to_string()))?;
                let _ = channel.wait_close();

                self.last_success = Instant::now();

                let mut current_mtimes: HashMap<String, i64> = HashMap::new();
                for line in output.lines() {
                    let parts: Vec<&str> = line.rsplitn(2, ' ').collect();
                    if parts.len() != 2 {
                        continue;
                    }
                    let mtime_str = parts[0];
                    let file_path = parts[1];
                    if let Ok(mtime) = mtime_str.parse::<f64>() {
                        current_mtimes.insert(file_path.to_string(), mtime as i64);
                    }
                }

                for (file_path, mtime) in &current_mtimes {
                    let changed = match known_mtimes.get(file_path) {
                        Some(old_mtime) => *mtime != *old_mtime,
                        None => true,
                    };
                    if changed {
                        let kind = if known_mtimes.contains_key(file_path) {
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

                *known_mtimes = current_mtimes;
            }
        }

        Ok(std::mem::take(&mut self.pending))
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>, WatchError> {
        let sftp = self
            .session
            .sftp()
            .map_err(|e| WatchError::Ssh(e.to_string()))?;

        let mut file = sftp
            .open(std::path::Path::new(path))
            .map_err(|e| WatchError::Ssh(e.to_string()))?;

        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .map_err(|e| WatchError::Ssh(e.to_string()))?;

        self.last_success = Instant::now();
        Ok(buf)
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

fn parse_inotify_line(line: &str) -> Option<WatchEvent> {
    let parts: Vec<&str> = line.splitn(2, ' ').collect();
    if parts.len() != 2 {
        return None;
    }

    let path = parts[0].to_string();
    let events_str = parts[1];

    let kind = if events_str.contains("CREATE") {
        WatchEventKind::Created
    } else if events_str.contains("MODIFY") || events_str.contains("CLOSE_WRITE") {
        WatchEventKind::Modified
    } else {
        return None;
    };

    Some(WatchEvent { path, kind })
}
