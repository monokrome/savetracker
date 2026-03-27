use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;
use tui_textarea::TextArea;
use watch_path::ConnectionState;

use super::app::{App, View};

const VERSION_COL_WIDTH: u16 = 12;
const MIN_DIFF_WIDTH: u16 = 30;

pub fn draw(frame: &mut Frame, app: &App, editor: &TextArea) {
    match app.view {
        View::Main => draw_main(frame, app, editor),
        View::DetailDiff => draw_detail_diff(frame, app),
    }
}

fn draw_main(frame: &mut Frame, app: &App, editor: &TextArea) {
    let area = frame.area();

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let main_area = rows[0];
    let status_area = rows[1];

    let has_diff_space = main_area.width > VERSION_COL_WIDTH + 40 + MIN_DIFF_WIDTH;

    let columns = if has_diff_space {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(VERSION_COL_WIDTH),
                Constraint::Percentage(50),
                Constraint::Min(MIN_DIFF_WIDTH),
            ])
            .split(main_area)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(VERSION_COL_WIDTH), Constraint::Min(20)])
            .split(main_area)
    };

    draw_version_list(frame, app, columns[0]);
    draw_editor(frame, editor, columns[1]);

    if has_diff_space {
        draw_diff_panel(frame, app, columns[2]);
    }

    draw_status_bar(frame, app, status_area);
}

fn draw_version_list(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .versions
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let time = entry.info.timestamp.format("%H:%M:%S").to_string();
            let style = if i == app.selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if entry.info.description.is_some() {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let marker = if i == app.selected { ">" } else { " " };
            ListItem::new(Line::from(Span::styled(format!("{marker}{time}"), style)))
        })
        .collect();

    let block = Block::default().borders(Borders::RIGHT).title("Versions");

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_editor(frame: &mut Frame, editor: &TextArea, area: Rect) {
    let block = Block::default()
        .borders(Borders::RIGHT)
        .title("Notes")
        .padding(ratatui::widgets::Padding::uniform(1));

    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(editor, inner);
}

fn draw_diff_panel(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::NONE).title("Diff");

    let content = match app.selected_entry() {
        Some(entry) => match &entry.diff {
            Some(d) => format_diff_lines(&d.detail),
            None => vec![Line::from(Span::styled(
                "First version (no previous to diff)",
                Style::default().fg(Color::DarkGray),
            ))],
        },
        None => vec![Line::from("No version selected")],
    };

    let paragraph = Paragraph::new(content)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.diff_scroll, 0));

    frame.render_widget(paragraph, area);
}

fn draw_detail_diff(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let title = match app.selected_entry() {
        Some(entry) => format!(
            "Diff Detail - {} @ {}",
            entry.file_name,
            entry.info.timestamp.format("%H:%M:%S")
        ),
        None => "Diff Detail".to_string(),
    };

    let block = Block::default().borders(Borders::ALL).title(title);

    let content = match app.selected_entry() {
        Some(entry) => match &entry.diff {
            Some(d) => {
                let mut lines = vec![
                    Line::from(Span::styled(
                        &d.summary,
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                ];
                lines.extend(format_diff_lines(&d.detail));
                lines
            }
            None => vec![Line::from("First version (no previous to diff)")],
        },
        None => vec![Line::from("No version selected")],
    };

    let paragraph = Paragraph::new(content)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.diff_scroll, 0));

    frame.render_widget(paragraph, area);
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let color = match app.connection_state {
        ConnectionState::Connected => Color::Blue,
        ConnectionState::Degraded => Color::Yellow,
        ConnectionState::Lost => Color::Red,
    };

    let indicator = Span::styled("\u{2588}\u{2588}", Style::default().fg(color));

    let url_width = area.width.saturating_sub(5) as usize;
    let url_display = if app.watch_url.len() > url_width {
        format!(
            "...{}",
            &app.watch_url[app.watch_url.len() - url_width + 3..]
        )
    } else {
        app.watch_url.clone()
    };

    let padding = area.width.saturating_sub(url_display.len() as u16 + 4) as usize;

    let line = Line::from(vec![
        Span::raw(" "),
        Span::styled(url_display, Style::default().fg(Color::White)),
        Span::raw(" ".repeat(padding)),
        indicator,
        Span::raw(" "),
    ]);

    let bar = Paragraph::new(line).style(Style::default().bg(Color::DarkGray));
    frame.render_widget(bar, area);
}

fn format_diff_lines(detail: &str) -> Vec<Line<'static>> {
    detail
        .lines()
        .map(|line| {
            let style = if line.starts_with('+') {
                Style::default().fg(Color::Green)
            } else if line.starts_with('-') {
                Style::default().fg(Color::Red)
            } else if line.starts_with("───") {
                Style::default().fg(Color::DarkGray)
            } else if line.starts_with("  0x") {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };

            Line::from(Span::styled(line.to_string(), style))
        })
        .collect()
}
