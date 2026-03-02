pub mod app;
pub mod ui;

use std::io;
use std::path::Path;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::CrosstermBackend;
use ratatui::Terminal;
use tui_textarea::{Input, Key, TextArea};

use crate::config::Config;
use crate::detect;
use crate::storage::Storage;
use crate::watcher::SaveWatcher;

use app::{App, View};

pub struct TuiOptions {
    pub idle_timeout_secs: u64,
    pub max_versions: Option<usize>,
    pub live: bool,
}

pub fn run(
    config: Config,
    storage: Box<dyn Storage>,
    options: TuiOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, config, storage, options);

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: Config,
    storage: Box<dyn Storage>,
    options: TuiOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut app = App::new(options.idle_timeout_secs, options.max_versions);
    let mut watcher = SaveWatcher::new(&config.watch_dir, config.debounce)?;
    let mut editor = new_editor(None);

    app.load_versions(&*storage)?;
    sync_editor_to_selection(&app, &mut editor);

    loop {
        terminal.draw(|frame| ui::draw(frame, &app, &editor))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if handle_key(&mut app, &mut editor, &*storage, key)? {
                    break;
                }
            }
        }

        let events = watcher.poll();
        for ev in events {
            let data = std::fs::read(&ev.path)?;
            let previous = storage.latest(&ev.path)?;
            storage.save(&ev.path, &data)?;

            let format = detect::detect(&data);
            app.status_message = Some(format!(
                "Change: {} ({})",
                ev.path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                format
            ));

            if previous.is_some() {
                flush_editor_to_storage(&app, &editor, &*storage);
            }

            app.on_save_change(&ev.path, &*storage)?;
            sync_editor_to_selection(&app, &mut editor);
        }
    }

    flush_editor_to_storage(&app, &editor, &*storage);
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
        KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
            flush_editor_to_storage(app, editor, storage);
            app.select_prev();
            sync_editor_to_selection(app, editor);
        }
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

    let file_path = std::path::PathBuf::from(&entry.file_name);
    let _ = storage.set_description(&file_path, &entry.info.id, trimmed);
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

fn new_editor(content: Option<&str>) -> TextArea<'static> {
    let lines: Vec<String> = match content {
        Some(text) => text.lines().map(|l| l.to_string()).collect(),
        None => vec![String::new()],
    };

    let mut editor = TextArea::new(lines);
    editor.set_cursor_line_style(ratatui::style::Style::default());
    editor
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

            let file_path = Path::new(&entry.file_name);
            let trimmed = new_content.trim();
            if !trimmed.is_empty() {
                storage.set_description(file_path, &entry.info.id, trimmed)?;
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
