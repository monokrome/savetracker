pub mod ftp;
pub mod http;
pub mod local;
pub mod ssh;
pub mod url;
pub mod watcher;

pub use url::{parse as parse_url, Protocol, WatchTarget};
pub use watcher::{
    ConnectionState, PathWatcher, WatchError, WatchEvent, WatchEventKind, WatchOptions,
};

use std::path::Path;

pub fn connect(url: &str, options: &WatchOptions) -> Result<Box<dyn PathWatcher>, WatchError> {
    let target = url::parse(url)?;

    match target.protocol {
        Protocol::File => {
            let path = Path::new(&target.path);
            let watcher = local::LocalWatcher::new(path, options)?;
            Ok(Box::new(watcher))
        }
        Protocol::Ssh | Protocol::Sftp | Protocol::Scp => {
            let watcher = ssh::SshWatcher::connect(target, options)?;
            Ok(Box::new(watcher))
        }
        Protocol::Ftp => {
            let watcher = ftp::FtpWatcher::connect(target, options)?;
            Ok(Box::new(watcher))
        }
        Protocol::Http | Protocol::Https => {
            let watcher = http::HttpWatcher::connect(&target, options)?;
            Ok(Box::new(watcher))
        }
    }
}
