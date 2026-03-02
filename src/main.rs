use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};

use savetracker::analyze;
use savetracker::config::Config;
use savetracker::decompress;
use savetracker::detect;
use savetracker::diff;
use savetracker::snapshot::CopyStore;
use savetracker::storage::Storage;
use savetracker::tui::{self, TuiOptions};
use savetracker::watcher::SaveWatcher;

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
}

#[derive(Subcommand)]
enum Command {
    Watch {
        dir: PathBuf,

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
    },
    Analyze {
        dir: PathBuf,
        #[arg(long)]
        file: Option<String>,
    },
}

fn resolve_model(cli: &Cli) -> String {
    cli.model.clone().unwrap_or_else(|| "mistral".to_string())
}

fn build_config(cli: &Cli, watch_dir: PathBuf) -> Config {
    let mut config = Config::new(watch_dir)
        .with_ollama_url(cli.ollama_url.clone())
        .with_model(resolve_model(cli))
        .with_debounce(Duration::from_millis(cli.debounce_ms))
        .with_max_snapshots(cli.max_snapshots);

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

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Command::Watch {
            dir,
            interactive,
            live,
            max_versions,
            idle_timeout,
        } => {
            let config = build_config(&cli, dir.clone());
            let storage = build_storage(&config);
            let use_ollama = *live || cli.model.is_some();

            if *interactive {
                let options = TuiOptions {
                    idle_timeout_secs: *idle_timeout,
                    max_versions: *max_versions,
                    live: use_ollama,
                };
                if let Err(e) = tui::run(config, storage, options) {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            } else if let Err(e) = run_watch(config, storage, use_ollama).await {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Command::Analyze { dir, file } => {
            let config = build_config(&cli, dir.clone());
            let storage = build_storage(&config);
            if let Err(e) = run_analyze(config, storage, file.as_deref()).await {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
}

async fn run_watch(
    config: Config,
    storage: Box<dyn Storage>,
    live: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut watcher = SaveWatcher::new(&config.watch_dir, config.debounce)?;

    let mode = if live { "live" } else { "deferred" };
    eprintln!(
        "Watching {} ({mode} mode, debounce {}ms)",
        config.watch_dir.display(),
        config.debounce.as_millis()
    );

    loop {
        let events = watcher.poll();

        for event in events {
            eprintln!("Change detected: {}", event.path.display());

            let new_data = std::fs::read(&event.path)?;
            let previous = storage.latest(&event.path)?;
            storage.save(&event.path, &new_data)?;

            let format = detect::detect(&new_data);
            eprintln!("  Format: {format}");

            let new_content = decompress_if_needed(&new_data, &format);
            let old_content = previous
                .as_ref()
                .map(|s| decompress_if_needed(&s.data, &format));

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
    file_filter: Option<&str>,
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

            let format = detect::detect(&new.data);
            let old_content = decompress_if_needed(&old.data, &format);
            let new_content = decompress_if_needed(&new.data, &format);

            let file_diff = diff::diff(&old_content, &new_content, &format);

            let user_notes = window[1].description.as_deref();

            eprintln!(
                "  {} -> {}: {}",
                old.version.timestamp.format("%H:%M:%S"),
                new.version.timestamp.format("%H:%M:%S"),
                file_diff.summary,
            );

            eprintln!("  Asking ollama ({})...", config.model);
            match analyze::analyze(&file_diff, &config.ollama_url, &config.model, user_notes).await
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

fn decompress_if_needed(data: &[u8], format: &detect::FileFormat) -> Vec<u8> {
    match format {
        detect::FileFormat::Compressed(ct, _) => {
            decompress::decompress(data, ct.clone()).unwrap_or_else(|_| data.to_vec())
        }
        _ => data.to_vec(),
    }
}
