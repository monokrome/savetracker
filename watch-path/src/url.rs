use crate::watcher::WatchError;

#[derive(Debug, Clone)]
pub struct WatchTarget {
    pub protocol: Protocol,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Protocol {
    File,
    Ssh,
    Sftp,
    Scp,
    Ftp,
    Http,
    Https,
}

pub fn parse(url: &str) -> Result<WatchTarget, WatchError> {
    if let Some(rest) = url.strip_prefix("file://") {
        return Ok(WatchTarget {
            protocol: Protocol::File,
            host: None,
            port: None,
            user: None,
            path: rest.to_string(),
        });
    }

    if let Some((scheme, rest)) = url.split_once("://") {
        let protocol = match scheme {
            "ssh" => Protocol::Ssh,
            "sftp" => Protocol::Sftp,
            "scp" => Protocol::Scp,
            "ftp" => Protocol::Ftp,
            "http" => Protocol::Http,
            "https" => Protocol::Https,
            other => return Err(WatchError::UnsupportedProtocol(other.to_string())),
        };

        let (authority, path) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, "/"),
        };

        let (user_part, host_part) = match authority.find('@') {
            Some(idx) => (Some(&authority[..idx]), &authority[idx + 1..]),
            None => (None, authority),
        };

        let (host, port) = match host_part.find(':') {
            Some(idx) => {
                let port_str = &host_part[idx + 1..];
                match port_str.parse::<u16>() {
                    Ok(p) => (Some(host_part[..idx].to_string()), Some(p)),
                    Err(_) => (Some(host_part.to_string()), None),
                }
            }
            None => (Some(host_part.to_string()), None),
        };

        return Ok(WatchTarget {
            protocol,
            host,
            port,
            user: user_part.map(|s| s.to_string()),
            path: path.to_string(),
        });
    }

    Ok(WatchTarget {
        protocol: Protocol::File,
        host: None,
        port: None,
        user: None,
        path: url.to_string(),
    })
}

impl std::fmt::Display for WatchTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.protocol {
            Protocol::File => write!(f, "file://{}", self.path),
            _ => {
                let scheme = match self.protocol {
                    Protocol::Ssh => "ssh",
                    Protocol::Sftp => "sftp",
                    Protocol::Scp => "scp",
                    Protocol::Ftp => "ftp",
                    Protocol::Http => "http",
                    Protocol::Https => "https",
                    Protocol::File => unreachable!(),
                };
                write!(f, "{scheme}://")?;
                if let Some(ref user) = self.user {
                    write!(f, "{user}@")?;
                }
                if let Some(ref host) = self.host {
                    write!(f, "{host}")?;
                }
                if let Some(port) = self.port {
                    write!(f, ":{port}")?;
                }
                write!(f, "{}", self.path)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_path() {
        let target = parse("./saves").unwrap();
        assert_eq!(target.protocol, Protocol::File);
        assert_eq!(target.path, "./saves");
        assert!(target.host.is_none());
    }

    #[test]
    fn parse_file_url() {
        let target = parse("file:///home/user/saves").unwrap();
        assert_eq!(target.protocol, Protocol::File);
        assert_eq!(target.path, "/home/user/saves");
    }

    #[test]
    fn parse_ssh_url() {
        let target = parse("ssh://user@switch:22/saves").unwrap();
        assert_eq!(target.protocol, Protocol::Ssh);
        assert_eq!(target.user.as_deref(), Some("user"));
        assert_eq!(target.host.as_deref(), Some("switch"));
        assert_eq!(target.port, Some(22));
        assert_eq!(target.path, "/saves");
    }

    #[test]
    fn parse_sftp_url_no_port() {
        let target = parse("sftp://admin@mydevice/data/saves").unwrap();
        assert_eq!(target.protocol, Protocol::Sftp);
        assert_eq!(target.user.as_deref(), Some("admin"));
        assert_eq!(target.host.as_deref(), Some("mydevice"));
        assert!(target.port.is_none());
        assert_eq!(target.path, "/data/saves");
    }

    #[test]
    fn parse_ftp_url() {
        let target = parse("ftp://anbernic/roms/saves").unwrap();
        assert_eq!(target.protocol, Protocol::Ftp);
        assert!(target.user.is_none());
        assert_eq!(target.host.as_deref(), Some("anbernic"));
        assert_eq!(target.path, "/roms/saves");
    }

    #[test]
    fn parse_http_url() {
        let target = parse("http://device:8080/saves").unwrap();
        assert_eq!(target.protocol, Protocol::Http);
        assert_eq!(target.host.as_deref(), Some("device"));
        assert_eq!(target.port, Some(8080));
    }

    #[test]
    fn roundtrip_display() {
        let target = parse("ssh://user@switch:22/saves").unwrap();
        assert_eq!(target.to_string(), "ssh://user@switch:22/saves");
    }

    #[test]
    fn unsupported_protocol() {
        let result = parse("gopher://host/path");
        assert!(result.is_err());
    }
}
