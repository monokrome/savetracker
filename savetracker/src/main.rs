use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use tokio::sync::Semaphore;

use savetracker::analyze::{AnalyzeError, Analyzer};
use savetracker::storage::StorageError;

fn storage_err(e: StorageError) -> Box<dyn std::error::Error> {
    e.to_string().into()
}
use savetracker::batch;
use savetracker::claude::ClaudeAnalyzer;
use savetracker::config::{AnalyzerBackend, Config};
use savetracker::decode::decode_with_transform;
use savetracker::detect::FileFormat;
use savetracker::diff;
use savetracker::format::{self, FormatRegistry};
use savetracker::gemini::GeminiAnalyzer;
use savetracker::git_store::GitStore;
use savetracker::ollama::OllamaAnalyzer;
use savetracker::openai::OpenAiAnalyzer;
use savetracker::snapshot::CopyStore;
use savetracker::storage::Storage;
use savetracker::transform;
use savetracker::tui::{self, TuiOptions};

use watch_path::{PathWatcher, WatchOptions};

struct AnalysisContext<'a> {
    config: Config,
    storage: Box<dyn Storage>,
    registry: &'a FormatRegistry,
    file_filter: Option<&'a str>,
    analyzer: Arc<dyn Analyzer>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    concurrent: usize,
    limit: Option<usize>,
}

#[derive(Args, Clone)]
struct CommonArgs {
    #[arg(long, default_value = "http://localhost:11434")]
    ollama_url: String,

    #[arg(long, help = "LLM model name (default varies by provider)")]
    model: Option<String>,

    #[arg(
        long,
        default_value = "ollama",
        help = "LLM provider: ollama, openai, claude, gemini"
    )]
    model_provider: String,

    #[arg(long, help = "API base URL for openai-compatible providers")]
    model_provider_url: Option<String>,

    #[arg(
        long,
        help = "Environment variable name for API key (default varies by provider)"
    )]
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

    #[arg(short = 'l', long, help = "Max total items to analyze before exiting")]
    limit: Option<usize>,

    #[arg(
        short = 'j',
        long,
        default_value = "1",
        help = "Max concurrent LLM requests"
    )]
    concurrent: usize,
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
        #[command(flatten)]
        common: CommonArgs,

        #[arg(long)]
        file: Option<String>,

        #[arg(long, help = "Re-analyze existing descriptions (LLM reviews LLM)")]
        review: bool,

        #[arg(
            long,
            default_value = "forever",
            help = "Time filter: \"forever\", \"2h\", \"30m\", \"1d\""
        )]
        since: String,
    },
    Compare {
        #[command(flatten)]
        common: CommonArgs,

        #[arg(long)]
        file: Option<String>,

        #[arg(
            long,
            default_value = "forever",
            help = "Time filter: \"forever\", \"2h\", \"30m\", \"1d\""
        )]
        since: String,
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

fn parse_since(since: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if since == "forever" {
        return None;
    }

    let trimmed = since.trim().to_lowercase();
    let (num_str, unit) = trimmed.split_at(
        trimmed
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(trimmed.len()),
    );
    let num: i64 = num_str.parse().ok()?;
    let duration = match unit.trim() {
        "m" | "min" | "mins" => chrono::Duration::minutes(num),
        "h" | "hr" | "hrs" | "hour" | "hours" => chrono::Duration::hours(num),
        "d" | "day" | "days" => chrono::Duration::days(num),
        _ => return None,
    };

    Some(chrono::Utc::now() - duration)
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
            Some(AnalyzerBackend::OpenAi {
                url,
                key_env,
                model,
            })
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
        AnalyzerBackend::OpenAi {
            url,
            key_env,
            model,
        } => {
            let api_key =
                std::env::var(key_env).map_err(|_| AnalyzeError::MissingApiKey(key_env.clone()))?;
            Ok(Box::new(OpenAiAnalyzer::new(
                url.clone(),
                api_key,
                model.clone(),
            )))
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
            } else if let Err(e) = run_watch(
                config,
                storage,
                watcher,
                &registry,
                analyzer,
                common.concurrent,
            )
            .await
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
        Command::Analyze {
            common,
            file,
            review,
            since,
        } => {
            let analyzer_backend = resolve_backend(common, true);
            let config = build_config(common, String::new(), format_params, analyzer_backend);
            let storage = build_storage(&config);
            let since_time = parse_since(since);

            let analyzer: Arc<dyn Analyzer> = match config.analyzer.as_ref().map(build_analyzer) {
                Some(Ok(a)) => Arc::from(a),
                Some(Err(e)) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
                None => {
                    eprintln!("error: analyzer required for analyze command");
                    std::process::exit(1);
                }
            };

            if let Err(e) = run_analyze(
                AnalysisContext {
                    config,
                    storage,
                    registry: &registry,
                    file_filter: file.as_deref(),
                    analyzer,
                    since: since_time,
                    concurrent: common.concurrent,
                    limit: common.limit,
                },
                *review,
            )
            .await
            {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Command::Compare {
            common,
            file,
            since,
        } => {
            let analyzer_backend = resolve_backend(common, true);
            let config = build_config(common, String::new(), format_params, analyzer_backend);
            let storage = build_storage(&config);
            let since_time = parse_since(since);

            let analyzer: Arc<dyn Analyzer> = match config.analyzer.as_ref().map(build_analyzer) {
                Some(Ok(a)) => Arc::from(a),
                Some(Err(e)) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
                None => {
                    eprintln!("error: analyzer required for compare command");
                    std::process::exit(1);
                }
            };

            if let Err(e) = run_compare(AnalysisContext {
                config,
                storage,
                registry: &registry,
                file_filter: file.as_deref(),
                analyzer,
                since: since_time,
                concurrent: common.concurrent,
                limit: common.limit,
            })
            .await
            {
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

    let mut result = format::decode_or_detect(
        registry,
        common.format.as_deref(),
        &file_path,
        &data,
        format_params,
    )?;

    if let Some(ref cmd_str) = common.transform_to_content {
        if let Some(argv) = shlex::split(cmd_str) {
            if let Ok(transformed) = transform::execute(&argv, &result.data, format_params, None) {
                result.format = savetracker::detect::detect(&transformed);
                result.data = transformed;
            }
        }
    }

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
    concurrent: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let live = analyzer.is_some();
    let mode = if live { "live" } else { "deferred" };
    eprintln!(
        "Watching {} ({mode} mode, debounce {}ms)",
        config.watch_url,
        config.debounce.as_millis()
    );

    let analyzer: Option<Arc<dyn Analyzer>> = analyzer.map(Arc::from);
    let semaphore = Arc::new(Semaphore::new(concurrent));

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
                .map(|fc| storage.latest(&fc.path).map_err(storage_err))
                .collect::<Result<_, _>>()?;

            let changed: Vec<_> = group
                .iter()
                .zip(previous_snapshots.iter())
                .filter(|(fc, prev)| prev.as_ref().is_none_or(|p| p.data != fc.data))
                .collect();

            if changed.is_empty() {
                continue;
            }

            // Decode all changed files, then save raw + decoded together
            struct WatchAnalyzeItem {
                display_path: String,
                diff: savetracker::diff::FileDiff,
                is_first: bool,
                is_ollama_streaming: bool,
                ollama_url: String,
                ollama_model: String,
            }

            let mut batch_items: Vec<(String, Vec<u8>)> = Vec::new();
            let mut items = Vec::new();

            for (fc, previous) in &changed {
                let display_path = Path::new(&fc.path)
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| fc.path.clone());
                eprintln!("Change detected: {display_path}");

                let decode_out = decode_with_transform(registry, &config, &fc.path, &fc.data);
                let new_content = decode_out.data;
                let fmt = decode_out.format;
                eprintln!("  Format: {fmt}");

                // Queue raw file + decoded sidecar
                batch_items.push((fc.path.clone(), fc.data.clone()));

                if let Some((sidecar_name, sidecar_data)) = format::decoded_sidecar(
                    registry,
                    config.forced_format.as_deref(),
                    &fc.path,
                    &fc.data,
                    &config.format_params,
                ) {
                    let sidecar_path = PathBuf::from(&fc.path)
                        .with_file_name(sidecar_name)
                        .to_string_lossy()
                        .into_owned();
                    batch_items.push((sidecar_path, sidecar_data));
                }

                let old_content = previous
                    .as_ref()
                    .map(|s| decode_with_transform(registry, &config, &fc.path, &s.data).data);

                let old_ref = old_content.as_deref().unwrap_or(&[]);
                let file_diff = diff::diff(old_ref, &new_content, &fmt);
                eprintln!("  {}", file_diff.summary);

                let (is_ollama, url, model) = match config.analyzer {
                    Some(AnalyzerBackend::Ollama { ref url, ref model }) => {
                        (true, url.clone(), model.clone())
                    }
                    _ => (false, String::new(), String::new()),
                };

                items.push(WatchAnalyzeItem {
                    display_path,
                    diff: file_diff,
                    is_first: previous.is_none(),
                    is_ollama_streaming: is_ollama,
                    ollama_url: url,
                    ollama_model: model,
                });
            }

            // Save raw + decoded files
            let batch_refs: Vec<(&str, &[u8])> = batch_items
                .iter()
                .map(|(p, d)| (p.as_str(), d.as_slice()))
                .collect();
            storage.save_batch(&batch_refs).map_err(storage_err)?;

            let file_count = changed.len();
            if file_count > 1 {
                let names: Vec<&str> = changed.iter().map(|(fc, _)| fc.path.as_str()).collect();
                eprintln!("Batch save ({file_count} files): {}", names.join(", "));
            }

            // Fan out LLM calls
            let mut handles = Vec::new();
            for item in items {
                if item.is_first {
                    eprintln!("  First snapshot recorded.");
                    continue;
                }

                let Some(ref analyzer) = analyzer else {
                    continue;
                };

                let permit = semaphore.clone().acquire_owned().await?;
                let analyzer = analyzer.clone();

                if item.is_ollama_streaming {
                    // Streaming must stay sequential per-item for coherent output
                    let ollama = OllamaAnalyzer::new(item.ollama_url, item.ollama_model.clone());
                    eprintln!("  Analyzing ({})...", item.ollama_model);
                    handles.push(tokio::task::spawn_blocking(move || {
                        let result = ollama
                            .analyze_streaming(&item.diff, None, |token| print!("{token}"))
                            .map(|()| None);
                        drop(permit);
                        (item.display_path, result)
                    }));
                } else {
                    eprintln!("  Analyzing...");
                    handles.push(tokio::task::spawn_blocking(move || {
                        let result = analyzer.analyze(&item.diff, None).map(Some);
                        drop(permit);
                        (item.display_path, result)
                    }));
                }
            }

            for handle in handles {
                let (_display_path, result) = handle.await?;
                match result {
                    Ok(Some(text)) => println!("{text}"),
                    Ok(None) => println!(),
                    Err(e) => eprintln!("  Analysis error: {e}"),
                }
            }
        }
    }
}

struct AnalyzeWork {
    file_path: String,
    version_id: String,
    diff: savetracker::diff::FileDiff,
    existing_description: Option<String>,
    label: String,
}

async fn run_analyze(
    ctx: AnalysisContext<'_>,
    review: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let AnalysisContext {
        config,
        storage,
        registry,
        file_filter,
        analyzer,
        since,
        concurrent,
        limit,
    } = ctx;

    let mut tracked = storage.tracked_files().map_err(storage_err)?;

    if tracked.is_empty() {
        return Err(format!("No snapshots found at {}", config.snapshot_dir.display()).into());
    }

    if let Some(filter) = file_filter {
        tracked.retain(|name| name.contains(filter));
    }

    // Phase 1: collect work items (sequential, uses storage)
    let identity = analyzer.identity();
    let mut items = Vec::new();

    for file_name in &tracked {
        let versions = storage.list(file_name).map_err(storage_err)?;

        if versions.len() < 2 {
            eprintln!("{file_name}: not enough snapshots to diff");
            continue;
        }

        eprintln!(
            "Collecting diffs for {file_name} ({} snapshots)",
            versions.len()
        );

        let windows: Vec<_> = versions.windows(2).collect();
        for window in windows.iter().rev() {
            if let Some(cutoff) = since {
                if window[1].timestamp < cutoff {
                    continue;
                }
            }

            if review {
                let Some(ref existing) = window[1].description else {
                    continue;
                };

                let reviewers = storage
                    .reviewed_by(file_name, &window[1].id)
                    .map_err(storage_err)?;
                if reviewers.iter().any(|r| r == &identity) {
                    continue;
                }

                let file_diff =
                    build_diff(&config, registry, &*storage, file_name, file_name, window)?;
                let label = format!(
                    "  Reviewing {} ({})...",
                    window[1].id,
                    window[1].timestamp.format("%H:%M:%S"),
                );

                items.push(AnalyzeWork {
                    file_path: file_name.clone(),
                    version_id: window[1].id.clone(),
                    diff: file_diff,
                    existing_description: Some(existing.clone()),
                    label,
                });
            } else {
                if window[1].description.is_some() {
                    continue;
                }

                let file_diff =
                    build_diff(&config, registry, &*storage, file_name, file_name, window)?;
                let label = format!(
                    "  {} -> {}: {}",
                    window[0].timestamp.format("%H:%M:%S"),
                    window[1].timestamp.format("%H:%M:%S"),
                    file_diff.summary,
                );

                items.push(AnalyzeWork {
                    file_path: file_name.clone(),
                    version_id: window[1].id.clone(),
                    diff: file_diff,
                    existing_description: None,
                    label,
                });
            }
        }
    }

    if let Some(max) = limit {
        items.truncate(max);
    }

    if items.is_empty() {
        eprintln!("Nothing to analyze.");
        return Ok(());
    }

    let total = items.len();
    eprintln!("Processing {total} items (concurrency: {concurrent})");

    // Fan out LLM calls, save each result as it arrives via channel
    let semaphore = Arc::new(Semaphore::new(concurrent));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    for item in items {
        let permit = semaphore.clone().acquire_owned().await?;
        let analyzer = analyzer.clone();
        let tx = tx.clone();

        tokio::task::spawn_blocking(move || {
            eprintln!("{}", item.label);
            let result = if let Some(ref existing) = item.existing_description {
                analyzer.review(&item.diff, existing)
            } else {
                analyzer.analyze(&item.diff, None)
            };
            drop(permit);
            let _ = tx.send((item.file_path, item.version_id, result));
        });
    }

    drop(tx);

    let mut completed = 0;
    while let Some((file_path, version_id, result)) = rx.recv().await {
        completed += 1;
        match result {
            Ok(description) => {
                println!("{description}\n");
                if let Err(e) = storage.set_description(&file_path, &version_id, &description) {
                    eprintln!("  Failed to save description: {e}");
                }
                if let Err(e) = storage.mark_reviewed(&file_path, &version_id, &identity) {
                    eprintln!("  Failed to mark reviewed: {e}");
                }
                eprintln!("  [{completed}/{total}] saved {version_id}");
            }
            Err(e) => eprintln!("  [{completed}/{total}] Analysis error: {e}"),
        }
    }

    Ok(())
}

fn build_diff(
    config: &Config,
    registry: &FormatRegistry,
    storage: &dyn Storage,
    file_path: &str,
    file_name: &str,
    window: &[savetracker::storage::VersionInfo],
) -> Result<savetracker::diff::FileDiff, Box<dyn std::error::Error>> {
    let old = storage
        .load(file_path, &window[0].id)
        .map_err(storage_err)?;
    let new = storage
        .load(file_path, &window[1].id)
        .map_err(storage_err)?;

    let old_content = decode_with_transform(registry, config, file_name, &old.data).data;
    let new_decoded = decode_with_transform(registry, config, file_name, &new.data);

    Ok(diff::diff(
        &old_content,
        &new_decoded.data,
        &new_decoded.format,
    ))
}

struct CompareWork {
    file_path: String,
    file_name: String,
    version_id: String,
    existing_description: String,
    diff: savetracker::diff::FileDiff,
    time_label: String,
}

async fn run_compare(ctx: AnalysisContext<'_>) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{self, Write};

    let AnalysisContext {
        config,
        storage,
        registry,
        file_filter,
        analyzer,
        since,
        concurrent,
        limit,
    } = ctx;

    let mut tracked = storage.tracked_files().map_err(storage_err)?;

    if tracked.is_empty() {
        return Err(format!("No snapshots found at {}", config.snapshot_dir.display()).into());
    }

    if let Some(filter) = file_filter {
        tracked.retain(|name| name.contains(filter));
    }

    // Phase 1: collect work items
    let mut items = Vec::new();

    for file_name in &tracked {
        let versions = storage.list(file_name).map_err(storage_err)?;

        if versions.len() < 2 {
            continue;
        }

        let windows: Vec<_> = versions.windows(2).collect();
        for window in windows.iter().rev() {
            if let Some(cutoff) = since {
                if window[1].timestamp < cutoff {
                    continue;
                }
            }

            let Some(ref existing) = window[1].description else {
                continue;
            };

            let file_diff = build_diff(&config, registry, &*storage, file_name, file_name, window)?;
            let time_label = format!(
                "{} @ {} -> {}",
                file_name,
                window[0].timestamp.format("%H:%M:%S"),
                window[1].timestamp.format("%H:%M:%S"),
            );

            items.push(CompareWork {
                file_path: file_name.clone(),
                file_name: file_name.clone(),
                version_id: window[1].id.clone(),
                existing_description: existing.clone(),
                diff: file_diff,
                time_label,
            });
        }
    }

    if let Some(max) = limit {
        items.truncate(max);
    }

    if items.is_empty() {
        eprintln!("Nothing to compare.");
        return Ok(());
    }

    eprintln!(
        "Generating {} analyses (concurrency: {concurrent})...",
        items.len()
    );

    // Phase 2: fan out LLM calls
    let semaphore = Arc::new(Semaphore::new(concurrent));
    let mut handles = Vec::new();

    for item in items {
        let permit = semaphore.clone().acquire_owned().await?;
        let analyzer = analyzer.clone();

        handles.push(tokio::task::spawn_blocking(move || {
            let result = analyzer.analyze(&item.diff, None);
            drop(permit);
            (
                item.file_path,
                item.file_name,
                item.version_id,
                item.existing_description,
                item.time_label,
                result,
            )
        }));
    }

    // Phase 3: prompt user sequentially
    for handle in handles {
        let (file_path, _file_name, version_id, existing, time_label, result) = handle.await?;

        let new_description = match result {
            Ok(desc) => desc,
            Err(e) => {
                eprintln!("\n{time_label}");
                eprintln!("  Analysis error: {e}");
                continue;
            }
        };

        eprintln!("\n{time_label}");
        println!("\n--- Current ---");
        println!("{existing}");
        println!("\n--- Suggested ---");
        println!("{new_description}");
        print!("\nKeep [c]urrent, use [s]uggested, or [skip]? ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        match input.trim().to_lowercase().as_str() {
            "s" | "suggested" => {
                if let Err(e) = storage.set_description(&file_path, &version_id, &new_description) {
                    eprintln!("  Failed to save description: {e}");
                } else {
                    let identity = analyzer.identity();
                    if let Err(e) = storage.mark_reviewed(&file_path, &version_id, &identity) {
                        eprintln!("  Failed to mark reviewed: {e}");
                    }
                    eprintln!("  Updated.");
                }
            }
            "c" | "current" => {
                eprintln!("  Kept current.");
            }
            _ => {
                eprintln!("  Skipped.");
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
