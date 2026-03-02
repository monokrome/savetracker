use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Parser, Subcommand};

use savetracker::analyze;
use savetracker::config::Config;
use savetracker::diff;
use savetracker::format::{self, FormatRegistry};
use savetracker::snapshot::CopyStore;
use savetracker::storage::Storage;
use savetracker::tui::{self, TuiOptions};

use watch_path::{PathWatcher, WatchOptions};

#[derive(Parser)]
#[command(name = "savetracker", about = "Track changes in game save files")]
struct Cli {
    #[command(subcommand)]
    command: Command,

    #[arg(long, default_value = "http://localhost:11434")]
    ollama_url: String,

    #[arg(long, help = "Ollama model to use (implies --live for watch)")]
    model: Option<String>,

    #[arg(long)]
    snapshot_dir: Option<PathBuf>,

    #[arg(long, default_value = "2000")]
    debounce_ms: u64,

    #[arg(long, default_value = "50")]
    max_snapshots: usize,

    #[arg(long, help = "Force a specific save format by name")]
    format: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    Watch {
        url: String,

        #[arg(short, long, help = "Interactive TUI mode")]
        interactive: bool,

        #[arg(long, help = "Analyze changes with ollama in real-time")]
        live: bool,

        #[arg(long, help = "Max versions to display in TUI")]
        max_versions: Option<usize>,

        #[arg(
            long,
            default_value = "15",
            help = "Seconds of idle before auto-jumping to latest"
        )]
        idle_timeout: u64,

        #[arg(
            long,
            default_value = "5",
            help = "Poll interval in seconds for remote backends"
        )]
        poll_interval: u64,

        #[arg(
            long,
            default_value = "30",
            help = "Seconds before connection is considered lost"
        )]
        loss_timeout: u64,

        #[arg(long, help = "SSH key file path")]
        key_path: Option<PathBuf>,

        #[arg(long, help = "Password for remote authentication")]
        password: Option<String>,
    },
    Analyze {
        dir: PathBuf,
        #[arg(long)]
        file: Option<String>,
    },
}

fn extract_dynamic_params(args: &mut Vec<String>) -> HashMap<String, String> {
    let mut params = HashMap::new();
    let mut i = 0;

    while i < args.len() {
        if let Some(rest) = args[i].strip_prefix("--d:") {
            if let Some((key, value)) = rest.split_once('=') {
                params.insert(key.to_string(), value.to_string());
            }
            args.remove(i);
        } else {
            i += 1;
        }
    }

    params
}

fn resolve_model(cli: &Cli) -> String {
    cli.model.clone().unwrap_or_else(|| "mistral".to_string())
}

fn build_config(cli: &Cli, watch_url: String, format_params: HashMap<String, String>) -> Config {
    let mut config = Config::new(watch_url)
        .with_ollama_url(cli.ollama_url.clone())
        .with_model(resolve_model(cli))
        .with_debounce(Duration::from_millis(cli.debounce_ms))
        .with_max_snapshots(cli.max_snapshots)
        .with_forced_format(cli.format.clone())
        .with_format_params(format_params);

    if let Some(ref dir) = cli.snapshot_dir {
        config = config.with_snapshot_dir(dir.clone());
    }

    config
}

fn build_storage(config: &Config) -> Box<dyn Storage> {
    Box::new(CopyStore::new(
        config.snapshot_dir.clone(),
        config.max_snapshots,
    ))
}

fn build_watch_options(
    config: &Config,
    poll_interval: u64,
    loss_timeout: u64,
    key_path: &Option<PathBuf>,
    password: &Option<String>,
) -> WatchOptions {
    WatchOptions {
        debounce: config.debounce,
        poll_interval: Duration::from_secs(poll_interval),
        loss_timeout: Duration::from_secs(loss_timeout),
        password: password.clone(),
        key_path: key_path.clone(),
    }
}

#[tokio::main]
async fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    let format_params = extract_dynamic_params(&mut args);

    let cli = Cli::parse_from(&args);
    let registry = format::build_registry();
    let http_client = reqwest::Client::new();

    match &cli.command {
        Command::Watch {
            url,
            interactive,
            live,
            max_versions,
            idle_timeout,
            poll_interval,
            loss_timeout,
            key_path,
            password,
        } => {
            let config = build_config(&cli, url.clone(), format_params);
            let storage = build_storage(&config);
            let use_ollama = *live || cli.model.is_some();
            let watch_opts =
                build_watch_options(&config, *poll_interval, *loss_timeout, key_path, password);

            let watcher = match watch_path::connect(url, &watch_opts) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("error: failed to connect: {e}");
                    std::process::exit(1);
                }
            };

            if *interactive {
                let options = TuiOptions {
                    idle_timeout_secs: *idle_timeout,
                    max_versions: *max_versions,
                    live: use_ollama,
                };
                if let Err(e) = tui::run(&config, &*storage, watcher, &registry, &options) {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            } else if let Err(e) = run_watch(
                config,
                storage,
                watcher,
                &registry,
                use_ollama,
                &http_client,
            )
            .await
            {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Command::Analyze { dir, file } => {
            let config = build_config(&cli, dir.to_string_lossy().to_string(), format_params);
            let storage = build_storage(&config);
            if let Err(e) =
                run_analyze(config, storage, &registry, file.as_deref(), &http_client).await
            {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
}

async fn run_watch(
    config: Config,
    storage: Box<dyn Storage>,
    mut watcher: Box<dyn PathWatcher>,
    registry: &FormatRegistry,
    live: bool,
    http_client: &reqwest::Client,
) -> Result<(), Box<dyn std::error::Error>> {
    let mode = if live { "live" } else { "deferred" };
    eprintln!(
        "Watching {} ({mode} mode, debounce {}ms)",
        config.watch_url,
        config.debounce.as_millis()
    );

    loop {
        let events = watcher.poll()?;

        for event in events {
            eprintln!("Change detected: {}", event.path);

            let new_data = watcher.read(&event.path)?;
            let file_path = Path::new(&event.path);
            let previous = storage.latest(file_path)?;
            storage.save(file_path, &new_data)?;

            let (new_content, format) = format::decode_file(
                registry,
                config.forced_format.as_deref(),
                &event.path,
                &new_data,
                &config.format_params,
                config.transform_to_content.as_deref(),
            );
            eprintln!("  Format: {format}");

            let old_content = previous.as_ref().map(|s| {
                let (decoded, _) = format::decode_file(
                    registry,
                    config.forced_format.as_deref(),
                    &event.path,
                    &s.data,
                    &config.format_params,
                    config.transform_to_content.as_deref(),
                );
                decoded
            });

            let old_ref = old_content.as_deref().unwrap_or(&[]);
            let file_diff = diff::diff(old_ref, &new_content, &format);

            eprintln!("  {}", file_diff.summary);

            if previous.is_none() {
                eprintln!("  First snapshot recorded.");
                continue;
            }

            if !live {
                continue;
            }

            eprintln!("  Asking ollama ({})...", config.model);
            match analyze::analyze_streaming(
                http_client,
                &file_diff,
                &config.ollama_url,
                &config.model,
                None,
                |token| print!("{token}"),
            )
            .await
            {
                Ok(()) => println!(),
                Err(e) => eprintln!("  Ollama error: {e}"),
            }
        }

        if !watcher.has_pending() {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

async fn run_analyze(
    config: Config,
    storage: Box<dyn Storage>,
    registry: &FormatRegistry,
    file_filter: Option<&str>,
    http_client: &reqwest::Client,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tracked = storage.tracked_files()?;

    if tracked.is_empty() {
        return Err(format!("No snapshots found at {}", config.snapshot_dir.display()).into());
    }

    if let Some(filter) = file_filter {
        tracked.retain(|name| name.contains(filter));
    }

    for file_name in tracked {
        let file_path = PathBuf::from(&file_name);
        let versions = storage.list(&file_path)?;

        if versions.len() < 2 {
            eprintln!("{file_name}: not enough snapshots to diff");
            continue;
        }

        eprintln!("Analyzing {file_name} ({} snapshots)", versions.len());

        for window in versions.windows(2) {
            if window[1].description.is_some() {
                continue;
            }

            let old = storage.load(&file_path, &window[0].id)?;
            let new = storage.load(&file_path, &window[1].id)?;

            let (old_content, _) = format::decode_file(
                registry,
                config.forced_format.as_deref(),
                &file_name,
                &old.data,
                &config.format_params,
                config.transform_to_content.as_deref(),
            );
            let (new_content, format) = format::decode_file(
                registry,
                config.forced_format.as_deref(),
                &file_name,
                &new.data,
                &config.format_params,
                config.transform_to_content.as_deref(),
            );

            let file_diff = diff::diff(&old_content, &new_content, &format);

            let user_notes = window[1].description.as_deref();

            eprintln!(
                "  {} -> {}: {}",
                old.version.timestamp.format("%H:%M:%S"),
                new.version.timestamp.format("%H:%M:%S"),
                file_diff.summary,
            );

            eprintln!("  Asking ollama ({})...", config.model);
            match analyze::analyze(
                http_client,
                &file_diff,
                &config.ollama_url,
                &config.model,
                user_notes,
            )
            .await
            {
                Ok(description) => {
                    println!("{description}\n");
                    let _ = storage.set_description(&file_path, &window[1].id, &description);
                }
                Err(e) => eprintln!("  Ollama error: {e}"),
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_dynamic_params_basic() {
        let mut args = vec![
            "savetracker".to_string(),
            "--d:steam-id=12345".to_string(),
            "watch".to_string(),
            "--d:key=value".to_string(),
            "file:///path".to_string(),
        ];
        let params = extract_dynamic_params(&mut args);
        assert_eq!(params.get("steam-id").unwrap(), "12345");
        assert_eq!(params.get("key").unwrap(), "value");
        assert_eq!(args.len(), 3);
        assert!(!args.iter().any(|a| a.starts_with("--d:")));
    }

    #[test]
    fn extract_dynamic_params_empty() {
        let mut args = vec!["savetracker".to_string(), "watch".to_string()];
        let params = extract_dynamic_params(&mut args);
        assert!(params.is_empty());
        assert_eq!(args.len(), 2);
    }
}
