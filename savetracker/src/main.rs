use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Args, Parser, Subcommand};

use savetracker::analyze::{Analyzer, AnalyzeError};
use savetracker::batch;
use savetracker::claude::ClaudeAnalyzer;
use savetracker::config::{AnalyzerBackend, Config};
use savetracker::diff;
use savetracker::format::{self, FormatRegistry};
use savetracker::gemini::GeminiAnalyzer;
use savetracker::git_store::GitStore;
use savetracker::ollama::OllamaAnalyzer;
use savetracker::openai::OpenAiAnalyzer;
use savetracker::snapshot::CopyStore;
use savetracker::storage::Storage;
use savetracker::tui::{self, TuiOptions};

use watch_path::{PathWatcher, WatchOptions};

#[derive(Args, Clone)]
struct CommonArgs {
    #[arg(long, default_value = "http://localhost:11434")]
    ollama_url: String,

    #[arg(long, help = "LLM model name (default varies by provider)")]
    model: Option<String>,

    #[arg(long, default_value = "ollama", help = "LLM provider: ollama, openai, claude, gemini")]
    model_provider: String,

    #[arg(long, help = "API base URL for openai-compatible providers")]
    model_provider_url: Option<String>,

    #[arg(long, help = "Environment variable name for API key (default varies by provider)")]
    model_provider_key_env: Option<String>,

    #[arg(long)]
    snapshot_dir: Option<PathBuf>,

    #[arg(long, default_value = "2000")]
    debounce_ms: u64,

    #[arg(long, default_value = "50")]
    max_snapshots: usize,

    #[arg(long, help = "Force a specific save format by name")]
    format: Option<String>,

    #[arg(
        long,
        help = "Command to transform binary save data to structured text (shell-quoted string)"
    )]
    transform_to_content: Option<String>,

    #[arg(long, help = "Use git backend for snapshot storage")]
    git: bool,
}

#[derive(Parser)]
#[command(name = "savetracker", about = "Track changes in game save files")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Watch {
        url: String,

        #[command(flatten)]
        common: CommonArgs,

        #[arg(short, long, help = "Interactive TUI mode")]
        interactive: bool,

        #[arg(long, help = "Analyze changes with LLM in real-time")]
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
    Inspect {
        file: PathBuf,

        #[command(flatten)]
        common: CommonArgs,

        #[arg(short, long, help = "Write decoded output to a file instead of stdout")]
        output: Option<PathBuf>,
    },
    Analyze {
        dir: PathBuf,

        #[command(flatten)]
        common: CommonArgs,

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

fn default_model(provider: &str) -> &'static str {
    match provider {
        "openai" => "gpt-4o-mini",
        "claude" => "claude-sonnet-4-20250514",
        "gemini" => "gemini-2.0-flash",
        _ => "gemma3:4b",
    }
}

fn default_provider_url(provider: &str) -> &'static str {
    match provider {
        "openai" => "https://api.openai.com/v1/chat/completions",
        _ => "",
    }
}

fn default_key_env(provider: &str) -> &'static str {
    match provider {
        "openai" => "OPENAI_API_KEY",
        "claude" => "ANTHROPIC_API_KEY",
        "gemini" => "GEMINI_API_KEY",
        _ => "",
    }
}

fn resolve_backend(common: &CommonArgs, live: bool) -> Option<AnalyzerBackend> {
    if !live && common.model.is_none() {
        return None;
    }

    let provider = common.model_provider.as_str();
    let model = common
        .model
        .clone()
        .unwrap_or_else(|| default_model(provider).to_string());

    match provider {
        "openai" => {
            let url = common
                .model_provider_url
                .clone()
                .unwrap_or_else(|| default_provider_url(provider).to_string());
            let key_env = common
                .model_provider_key_env
                .clone()
                .unwrap_or_else(|| default_key_env(provider).to_string());
            Some(AnalyzerBackend::OpenAi { url, key_env, model })
        }
        "claude" => Some(AnalyzerBackend::Claude { model }),
        "gemini" => Some(AnalyzerBackend::Gemini { model }),
        _ => Some(AnalyzerBackend::Ollama {
            url: common.ollama_url.clone(),
            model,
        }),
    }
}

fn build_analyzer(backend: &AnalyzerBackend) -> Result<Box<dyn Analyzer>, AnalyzeError> {
    match backend {
        AnalyzerBackend::Ollama { url, model } => {
            Ok(Box::new(OllamaAnalyzer::new(url.clone(), model.clone())))
        }
        AnalyzerBackend::OpenAi { url, key_env, model } => {
            let api_key = std::env::var(key_env)
                .map_err(|_| AnalyzeError::MissingApiKey(key_env.clone()))?;
            Ok(Box::new(OpenAiAnalyzer::new(url.clone(), api_key, model.clone())))
        }
        AnalyzerBackend::Claude { model } => {
            let api_key = std::env::var("ANTHROPIC_API_KEY")
                .map_err(|_| AnalyzeError::MissingApiKey("ANTHROPIC_API_KEY".to_string()))?;
            Ok(Box::new(ClaudeAnalyzer::new(api_key, model.clone())))
        }
        AnalyzerBackend::Gemini { model } => {
            let api_key = std::env::var("GEMINI_API_KEY")
                .map_err(|_| AnalyzeError::MissingApiKey("GEMINI_API_KEY".to_string()))?;
            Ok(Box::new(GeminiAnalyzer::new(api_key, model.clone())))
        }
    }
}

fn build_config(
    common: &CommonArgs,
    watch_url: String,
    format_params: HashMap<String, String>,
    analyzer: Option<AnalyzerBackend>,
) -> Config {
    let transform_cmd = common.transform_to_content.as_ref().and_then(|s| {
        shlex::split(s).or_else(|| {
            eprintln!("warning: invalid transform command quoting: {s}");
            None
        })
    });

    let mut config = Config::new(watch_url)
        .with_analyzer(analyzer)
        .with_debounce(Duration::from_millis(common.debounce_ms))
        .with_max_snapshots(common.max_snapshots)
        .with_forced_format(common.format.clone())
        .with_format_params(format_params)
        .with_transform_to_content(transform_cmd)
        .with_use_git(common.git);

    if let Some(ref dir) = common.snapshot_dir {
        config = config.with_snapshot_dir(dir.clone());
    }

    config
}

fn is_git_repo(path: &Path) -> bool {
    path.join("HEAD").exists() && path.join("refs").exists()
}

fn build_storage(config: &Config) -> Box<dyn Storage> {
    if config.use_git || is_git_repo(&config.snapshot_dir) {
        match GitStore::open_or_init(&config.snapshot_dir) {
            Ok(store) => Box::new(store),
            Err(e) => {
                eprintln!("error: failed to open git storage: {e}");
                std::process::exit(1);
            }
        }
    } else {
        Box::new(CopyStore::new(
            config.snapshot_dir.clone(),
            config.max_snapshots,
        ))
    }
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

    match &cli.command {
        Command::Watch {
            url,
            common,
            interactive,
            live,
            max_versions,
            idle_timeout,
            poll_interval,
            loss_timeout,
            key_path,
            password,
        } => {
            let use_llm = *live || common.model.is_some();
            let analyzer_backend = resolve_backend(common, use_llm);
            let config = build_config(common, url.clone(), format_params, analyzer_backend);
            let storage = build_storage(&config);
            let watch_opts =
                build_watch_options(&config, *poll_interval, *loss_timeout, key_path, password);

            let watcher = match watch_path::connect(url, &watch_opts) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("error: failed to connect: {e}");
                    std::process::exit(1);
                }
            };

            let analyzer = if use_llm {
                match config.analyzer.as_ref().map(build_analyzer) {
                    Some(Ok(a)) => Some(a),
                    Some(Err(e)) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                    None => None,
                }
            } else {
                None
            };

            if *interactive {
                let options = TuiOptions {
                    idle_timeout_secs: *idle_timeout,
                    max_versions: *max_versions,
                    live: use_llm,
                };
                if let Err(e) = tui::run(&config, &*storage, watcher, &registry, &options, analyzer)
                {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            } else if let Err(e) =
                run_watch(config, storage, watcher, &registry, analyzer).await
            {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Command::Inspect {
            file,
            common,
            output,
        } => {
            if let Err(e) = run_inspect(common, &registry, file, output.as_deref(), &format_params)
            {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Command::Analyze { dir, common, file } => {
            let analyzer_backend = resolve_backend(common, true);
            let config = build_config(
                common,
                dir.to_string_lossy().to_string(),
                format_params,
                analyzer_backend,
            );
            let storage = build_storage(&config);

            let analyzer = match config.analyzer.as_ref().map(build_analyzer) {
                Some(Ok(a)) => a,
                Some(Err(e)) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
                None => {
                    eprintln!("error: analyzer required for analyze command");
                    std::process::exit(1);
                }
            };

            if let Err(e) = run_analyze(config, storage, &registry, file.as_deref(), &*analyzer) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
}

fn run_inspect(
    common: &CommonArgs,
    registry: &FormatRegistry,
    file: &Path,
    output: Option<&Path>,
    format_params: &HashMap<String, String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let data = std::fs::read(file)?;
    let file_path = file.to_string_lossy();

    let transform_cmd = common.transform_to_content.as_ref().and_then(|s| shlex::split(s));

    let result = format::decode_or_detect(
        registry,
        common.format.as_deref(),
        &file_path,
        &data,
        format_params,
        transform_cmd.as_deref(),
    )?;

    if let Some(ref name) = result.definition_name {
        eprintln!("Definition: {name}");
    }
    eprintln!("Format: {}", result.format);
    eprintln!("Size: {} -> {} bytes", data.len(), result.data.len());

    match output {
        Some(path) => {
            std::fs::write(path, &result.data)?;
            eprintln!("Wrote to {}", path.display());
        }
        None => {
            use std::io::Write;
            std::io::stdout().write_all(&result.data)?;
        }
    }

    Ok(())
}

async fn run_watch(
    config: Config,
    storage: Box<dyn Storage>,
    mut watcher: Box<dyn PathWatcher>,
    registry: &FormatRegistry,
    analyzer: Option<Box<dyn Analyzer>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let live = analyzer.is_some();
    let mode = if live { "live" } else { "deferred" };
    eprintln!(
        "Watching {} ({mode} mode, debounce {}ms)",
        config.watch_url,
        config.debounce.as_millis()
    );

    loop {
        let events = watcher.poll()?;

        if events.is_empty() {
            if !watcher.has_pending() {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            continue;
        }

        let batches = batch::drain_and_batch(&mut *watcher, events, &config.watch_url)?;

        for group in batches {
            let previous_snapshots: Vec<_> = group
                .iter()
                .map(|fc| storage.latest(Path::new(&fc.path)))
                .collect::<Result<_, _>>()?;

            let changed: Vec<_> = group
                .iter()
                .zip(previous_snapshots.iter())
                .filter(|(fc, prev)| prev.as_ref().is_none_or(|p| p.data != fc.data))
                .collect();

            if changed.is_empty() {
                continue;
            }

            let batch_items: Vec<(&Path, &[u8])> = changed
                .iter()
                .map(|(fc, _)| (Path::new(fc.path.as_str()), fc.data.as_slice()))
                .collect();

            storage.save_batch(&batch_items)?;

            let file_count = changed.len();
            if file_count > 1 {
                let names: Vec<&str> = changed.iter().map(|(fc, _)| fc.path.as_str()).collect();
                eprintln!("Batch save ({file_count} files): {}", names.join(", "));
            }

            for (fc, previous) in &changed {
                let display_path = Path::new(&fc.path)
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| fc.path.clone());
                eprintln!("Change detected: {display_path}");

                let (new_content, fmt) = format::decode_file(
                    registry,
                    config.forced_format.as_deref(),
                    &fc.path,
                    &fc.data,
                    &config.format_params,
                    config.transform_to_content.as_deref(),
                );
                eprintln!("  Format: {fmt}");

                let old_content = previous.as_ref().map(|s| {
                    let (decoded, _) = format::decode_file(
                        registry,
                        config.forced_format.as_deref(),
                        &fc.path,
                        &s.data,
                        &config.format_params,
                        config.transform_to_content.as_deref(),
                    );
                    decoded
                });

                let old_ref = old_content.as_deref().unwrap_or(&[]);
                let file_diff = diff::diff(old_ref, &new_content, &fmt);

                eprintln!("  {}", file_diff.summary);

                if previous.is_none() {
                    eprintln!("  First snapshot recorded.");
                    continue;
                }

                let Some(ref analyzer) = analyzer else {
                    continue;
                };

                // Try streaming for ollama, fall back to full response for others
                if let Some(AnalyzerBackend::Ollama { ref url, ref model }) = config.analyzer {
                    let ollama = OllamaAnalyzer::new(url.clone(), model.clone());
                    eprintln!("  Analyzing ({model})...");
                    match ollama.analyze_streaming(&file_diff, None, |token| print!("{token}")) {
                        Ok(()) => println!(),
                        Err(e) => eprintln!("  Analysis error: {e}"),
                    }
                } else {
                    eprintln!("  Analyzing...");
                    match analyzer.analyze(&file_diff, None) {
                        Ok(result) => println!("{result}"),
                        Err(e) => eprintln!("  Analysis error: {e}"),
                    }
                }
            }
        }
    }
}

fn run_analyze(
    config: Config,
    storage: Box<dyn Storage>,
    registry: &FormatRegistry,
    file_filter: Option<&str>,
    analyzer: &dyn Analyzer,
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

            eprintln!(
                "  {} -> {}: {}",
                old.version.timestamp.format("%H:%M:%S"),
                new.version.timestamp.format("%H:%M:%S"),
                file_diff.summary,
            );

            eprintln!("  Analyzing...");
            match analyzer.analyze(&file_diff, window[1].description.as_deref()) {
                Ok(description) => {
                    println!("{description}\n");
                    let _ = storage.set_description(&file_path, &window[1].id, &description);
                }
                Err(e) => eprintln!("  Analysis error: {e}"),
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
