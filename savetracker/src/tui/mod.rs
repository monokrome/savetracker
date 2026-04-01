pub mod app;
pub mod ui;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

fn storage_err(e: crate::storage::StorageError) -> Box<dyn std::error::Error> {
    e.to_string().into()
}

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::CrosstermBackend;
use ratatui::Terminal;
use tui_textarea::{Input, Key, TextArea};

use watch_path::PathWatcher;

use crate::analyze::Analyzer;
use crate::batch;
use crate::config::Config;
use crate::diff::FileDiff;
use crate::format::{self, FormatRegistry};
use crate::storage::Storage;

use app::{App, View};

struct AnalysisResult {
    file_name: String,
    version_id: String,
    description: String,
}

struct AnalysisRequest {
    diff: FileDiff,
    file_name: String,
    version_id: String,
}

fn spawn_analysis_worker(
    request_rx: mpsc::Receiver<AnalysisRequest>,
    result_tx: mpsc::Sender<AnalysisResult>,
    analyzer: Box<dyn Analyzer>,
) {
    std::thread::spawn(move || {
        while let Ok(req) = request_rx.recv() {
            let mut pending: std::collections::HashMap<String, AnalysisRequest> =
                std::collections::HashMap::new();
            pending.insert(req.file_name.clone(), req);
            while let Ok(newer) = request_rx.try_recv() {
                pending.insert(newer.file_name.clone(), newer);
            }

            for (_, req) in pending {
                let description = match analyzer.analyze(&req.diff, None) {
                    Ok(desc) => desc,
                    Err(e) => format!("Analysis failed: {e}"),
                };

                let _ = result_tx.send(AnalysisResult {
                    file_name: req.file_name,
                    version_id: req.version_id,
                    description,
                });
            }
        }
    });
}

pub struct TuiOptions {
    pub idle_timeout_secs: u64,
    pub max_versions: Option<usize>,
    pub live: bool,
}

pub fn run(
    config: &Config,
    storage: &dyn Storage,
    watcher: Box<dyn PathWatcher>,
    registry: &FormatRegistry,
    options: &TuiOptions,
    analyzer: Option<Box<dyn Analyzer>>,
) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, config, storage, watcher, registry, options, analyzer);

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: &Config,
    storage: &dyn Storage,
    mut watcher: Box<dyn PathWatcher>,
    registry: &FormatRegistry,
    options: &TuiOptions,
    analyzer: Option<Box<dyn Analyzer>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut app = App::new(
        options.idle_timeout_secs,
        options.max_versions,
        config.watch_url.clone(),
    );
    let mut editor = new_editor(None);
    let (result_tx, analysis_rx) = mpsc::channel::<AnalysisResult>();
    let (request_tx, request_rx) = mpsc::channel::<AnalysisRequest>();

    if let Some(analyzer) = analyzer {
        spawn_analysis_worker(request_rx, result_tx, analyzer);
    }

    app.load_versions(storage, registry, config).map_err(storage_err)?;
    sync_editor_to_selection(&app, &mut editor);

    loop {
        terminal.draw(|frame| ui::draw(frame, &app, &editor))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if handle_key(&mut app, &mut editor, storage, key)? {
                    break;
                }
                rewrap_editor(&mut editor);
            }
        }

        while let Ok(result) = analysis_rx.try_recv() {
            let _ = storage.set_description(&result.file_name, &result.version_id, &result.description);

            if let Some(entry) = app
                .versions
                .iter_mut()
                .find(|e| e.file_name == result.file_name && e.info.id == result.version_id)
            {
                entry.info.description = Some(result.description);
            }

            sync_editor_to_selection(&app, &mut editor);
            app.status_message = Some("Analysis complete".to_string());
        }

        app.connection_state = watcher.connection_state();
        let events = watcher.poll()?;

        if !events.is_empty() {
            let batches = batch::drain_and_batch(&mut *watcher, events, &config.watch_url)?;

            for group in batches {
                let mut batch_items: Vec<(String, Vec<u8>)> = Vec::new();
                for fc in group.iter() {
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
                }

                let had_previous: Vec<bool> = group
                    .iter()
                    .map(|fc| storage.latest(&fc.path).ok().flatten().is_some())
                    .collect();

                let batch_refs: Vec<(&str, &[u8])> = batch_items
                    .iter()
                    .map(|(p, d)| (p.as_str(), d.as_slice()))
                    .collect();
                storage.save_batch(&batch_refs).map_err(storage_err)?;

                for (fc, had_prev) in group.iter().zip(had_previous.iter()) {
                    let file_path = Path::new(&fc.path);
                    let fmt = crate::decode::decode_with_transform(
                        registry, config, &fc.path, &fc.data,
                    ).format;
                    let file_name = file_path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| fc.path.clone());

                    if group.len() > 1 {
                        app.status_message = Some(format!("Batch: {} files saved", group.len()));
                    } else {
                        app.status_message = Some(format!("Change: {file_name} ({fmt})"));
                    }

                    if *had_prev {
                        flush_editor_to_storage(&app, &editor, storage);
                    }

                    app.on_save_change(&fc.path, storage, registry, config).map_err(storage_err)?;

                    if options.live {
                        if let Some(entry) = app.versions.last() {
                            if let Some(ref diff) = entry.diff {
                                let _ = request_tx.send(AnalysisRequest {
                                    diff: diff.clone(),
                                    file_name: entry.file_name.clone(),
                                    version_id: entry.info.id.clone(),
                                });
                                app.status_message = Some("Queued for analysis...".to_string());
                            }
                        }
                    }

                    sync_editor_to_selection(&app, &mut editor);
                }
            }
        }
    }

    flush_editor_to_storage(&app, &editor, storage);
    Ok(())
}

fn handle_key(
    app: &mut App,
    editor: &mut TextArea,
    storage: &dyn Storage,
    key: KeyEvent,
) -> Result<bool, Box<dyn std::error::Error>> {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Ok(true);
    }

    match &app.view {
        View::DetailDiff => {
            match key.code {
                KeyCode::Esc => app.exit_overlay(),
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.exit_overlay();
                }
                KeyCode::PageUp => app.scroll_diff_up(),
                KeyCode::PageDown => app.scroll_diff_down(),
                _ => {}
            }
            return Ok(false);
        }
        View::Main => {}
    }

    match key.code {
        KeyCode::BackTab => {
            flush_editor_to_storage(app, editor, storage);
            app.select_prev();
            sync_editor_to_selection(app, editor);
        }
        KeyCode::Tab => {
            flush_editor_to_storage(app, editor, storage);
            app.select_next();
            sync_editor_to_selection(app, editor);
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.toggle_detail_diff();
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::ALT) => {
            open_external_editor(app, editor, storage)?;
        }
        KeyCode::PageUp => app.scroll_diff_up(),
        KeyCode::PageDown => app.scroll_diff_down(),
        _ => {
            app.touch_input();
            editor.input(to_textarea_input(key));
        }
    }

    Ok(false)
}

fn flush_editor_to_storage(app: &App, editor: &TextArea, storage: &dyn Storage) {
    let Some(entry) = app.selected_entry() else {
        return;
    };

    let text = editor.lines().join("\n");
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }

    let _ = storage.set_description(&entry.file_name, &entry.info.id, trimmed);
}

fn sync_editor_to_selection(app: &App, editor: &mut TextArea) {
    let content = app
        .selected_entry()
        .and_then(|e| e.info.description.as_deref())
        .unwrap_or("");

    *editor = new_editor(if content.is_empty() {
        None
    } else {
        Some(content)
    });
}

const WRAP_WIDTH: usize = 50;

fn word_wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for line in text.lines() {
        if line.len() <= width {
            lines.push(line.to_string());
            continue;
        }
        let mut current = String::new();
        for word in line.split_whitespace() {
            if current.is_empty() {
                current = word.to_string();
            } else if current.len() + 1 + word.len() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(current);
                current = word.to_string();
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn new_editor(content: Option<&str>) -> TextArea<'static> {
    let lines = match content {
        Some(text) => word_wrap(text, WRAP_WIDTH),
        None => vec![String::new()],
    };

    let mut editor = TextArea::new(lines);
    editor.set_cursor_line_style(ratatui::style::Style::default());
    editor
}

fn rewrap_editor(editor: &mut TextArea<'static>) {
    let (row, col) = editor.cursor();
    let text = editor.lines().join("\n");
    let lines = word_wrap(&text, WRAP_WIDTH);
    *editor = TextArea::new(lines);
    editor.set_cursor_line_style(ratatui::style::Style::default());
    let max_row = editor.lines().len().saturating_sub(1);
    let target_row = row.min(max_row);
    let max_col = editor
        .lines()
        .get(target_row)
        .map(|l| l.len())
        .unwrap_or(0);
    let target_col = col.min(max_col);
    editor.move_cursor(tui_textarea::CursorMove::Jump(
        target_row as u16,
        target_col as u16,
    ));
}

fn open_external_editor(
    app: &mut App,
    editor: &mut TextArea,
    storage: &dyn Storage,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(entry) = app.selected_entry() else {
        return Ok(());
    };

    let editor_cmd = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());

    let tmp_dir = std::env::temp_dir();
    let tmp_path = tmp_dir.join(format!("savetracker_{}.md", entry.info.id));

    let current_text = editor.lines().join("\n");
    std::fs::write(&tmp_path, &current_text)?;

    crossterm::terminal::disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    let status = std::process::Command::new(&editor_cmd)
        .arg(&tmp_path)
        .status();

    io::stdout().execute(EnterAlternateScreen)?;
    crossterm::terminal::enable_raw_mode()?;

    if let Ok(s) = status {
        if s.success() {
            let new_content = std::fs::read_to_string(&tmp_path)?;
            *editor = new_editor(Some(&new_content));

            let trimmed = new_content.trim();
            if !trimmed.is_empty() {
                storage.set_description(&entry.file_name, &entry.info.id, trimmed).map_err(storage_err)?;
            }
        }
    }

    let _ = std::fs::remove_file(&tmp_path);
    Ok(())
}

fn to_textarea_input(key: KeyEvent) -> Input {
    Input {
        key: match key.code {
            KeyCode::Char(c) => Key::Char(c),
            KeyCode::Backspace => Key::Backspace,
            KeyCode::Enter => Key::Enter,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Home => Key::Home,
            KeyCode::End => Key::End,
            KeyCode::Delete => Key::Delete,
            _ => Key::Null,
        },
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
    }
}
