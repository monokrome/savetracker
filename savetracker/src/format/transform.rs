use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use thiserror::Error;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const ENV_PREFIX: &str = "SAVETRACKER_PARAM_";

#[derive(Debug, Error)]
pub enum TransformError {
    #[error("transform command is empty")]
    EmptyCommand,

    #[error("transform command not found: {0}")]
    NotFound(String),

    #[error("transform failed (exit {code}): {stderr}")]
    Failed { code: i32, stderr: String },

    #[error("transform timed out after {0}s")]
    Timeout(u64),

    #[error("transform I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn execute(
    argv: &[String],
    data: &[u8],
    params: &HashMap<String, String>,
    timeout: Option<Duration>,
) -> Result<Vec<u8>, TransformError> {
    let (cmd, args) = argv.split_first().ok_or(TransformError::EmptyCommand)?;

    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(param_env_vars(params))
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => TransformError::NotFound(cmd.clone()),
            _ => TransformError::Io(e),
        })?;

    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(data)?;

    let deadline = Instant::now() + timeout.unwrap_or(DEFAULT_TIMEOUT);

    loop {
        match child.try_wait()? {
            Some(status) => {
                let output = child.wait_with_output()?;
                if status.success() {
                    return Ok(output.stdout);
                }
                return Err(TransformError::Failed {
                    code: status.code().unwrap_or(-1),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                });
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let secs = timeout.unwrap_or(DEFAULT_TIMEOUT).as_secs();
                return Err(TransformError::Timeout(secs));
            }
            None => std::thread::sleep(POLL_INTERVAL),
        }
    }
}

fn param_env_vars(params: &HashMap<String, String>) -> Vec<(String, String)> {
    params
        .iter()
        .map(|(k, v)| {
            let env_key = format!("{ENV_PREFIX}{}", k.to_uppercase().replace('-', "_"));
            (env_key, v.clone())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_transform_with_cat() {
        let argv: Vec<String> = vec!["cat".into()];
        let data = b"hello world";
        let result = execute(&argv, data, &HashMap::new(), None).unwrap();
        assert_eq!(result, data);
    }

    #[test]
    fn binary_data_roundtrip() {
        let argv: Vec<String> = vec!["cat".into()];
        let data: Vec<u8> = (0..=255).collect();
        let result = execute(&argv, &data, &HashMap::new(), None).unwrap();
        assert_eq!(result, data);
    }

    #[test]
    fn empty_command_returns_error() {
        let argv: Vec<String> = vec![];
        let result = execute(&argv, b"", &HashMap::new(), None);
        assert!(matches!(result, Err(TransformError::EmptyCommand)));
    }

    #[test]
    fn missing_command_returns_not_found() {
        let argv: Vec<String> = vec!["__nonexistent_command_99__".into()];
        let result = execute(&argv, b"", &HashMap::new(), None);
        assert!(matches!(result, Err(TransformError::NotFound(_))));
    }

    #[test]
    fn nonzero_exit_returns_failed() {
        let argv: Vec<String> = vec!["sh".into(), "-c".into(), "echo err >&2; exit 42".into()];
        let result = execute(&argv, b"", &HashMap::new(), None);
        match result {
            Err(TransformError::Failed { code, stderr }) => {
                assert_eq!(code, 42);
                assert!(stderr.contains("err"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn params_passed_as_env_vars() {
        let mut params = HashMap::new();
        params.insert("steam_id".into(), "12345".into());
        params.insert("save-slot".into(), "3".into());

        let argv: Vec<String> = vec![
            "sh".into(),
            "-c".into(),
            "echo -n $SAVETRACKER_PARAM_STEAM_ID:$SAVETRACKER_PARAM_SAVE_SLOT".into(),
        ];
        let result = execute(&argv, b"", &params, None).unwrap();
        assert_eq!(String::from_utf8(result).unwrap(), "12345:3");
    }

    #[test]
    fn command_with_args() {
        let argv: Vec<String> = vec!["tr".into(), "a-z".into(), "A-Z".into()];
        let result = execute(&argv, b"hello", &HashMap::new(), None).unwrap();
        assert_eq!(result, b"HELLO");
    }

    #[test]
    fn timeout_kills_slow_command() {
        let argv: Vec<String> = vec!["sleep".into(), "60".into()];
        let timeout = Some(Duration::from_millis(200));
        let result = execute(&argv, b"", &HashMap::new(), timeout);
        assert!(matches!(result, Err(TransformError::Timeout(_))));
    }

    #[test]
    fn empty_input_empty_output() {
        let argv: Vec<String> = vec!["cat".into()];
        let result = execute(&argv, b"", &HashMap::new(), None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn param_env_var_naming() {
        let mut params = HashMap::new();
        params.insert("my-key".into(), "val".into());
        let vars = param_env_vars(&params);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].0, "SAVETRACKER_PARAM_MY_KEY");
        assert_eq!(vars[0].1, "val");
    }
}
