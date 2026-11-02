//! Interactive (ratatui) device parameter editor.
//!
//! Renders the dynamic parameter form produced by `knx-form` and lets the
//! user change values; visibility is re-evaluated after every change.
//! Pages (ETS panes) are shown as tabs; navigate with Tab / Shift-Tab.
//! Enumerated parameters open a popup listing every possible value.

use std::collections::HashMap;
use std::io;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs};
use ratatui::{Frame, Terminal};

use knx_form::{evaluate, EnumOption, FieldKind, FormPage};
use knx_prod::ApplicationProgram;

#[derive(Clone)]
enum Row {
    Title(String),
    Field {
        id: String,
        label: String,
        kind: FieldKind,
        value: String,
        enabled: bool,
    },
}

#[derive(Clone)]
enum Mode {
    Normal,
    TextEdit { id: String, buffer: String },
    EnumPick { id: String, options: Vec<EnumOption>, cursor: usize },
}

/// Run the parameter editor. Returns `Some(values)` on save, `None` on quit.
pub fn edit_device(
    ap: &ApplicationProgram,
    initial: &HashMap<String, String>,
) -> Result<Option<HashMap<String, String>>> {
    let lang = if ap.default_language.is_empty() {
        "en-US".to_string()
    } else {
        ap.default_language.clone()
    };
    let mut values = initial.clone();
    let pages = evaluate(ap, &values, &lang);
    let mut page_index = 0usize;
    let mut cursor = 0usize;
    let mut list_state = ListState::default();
    let mut mode = Mode::Normal;

    enable_raw_mode().context("enabling raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("entering alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("creating terminal")?;

    let result = run_loop(
        &mut terminal,
        ap,
        &mut values,
        pages,
        &mut page_index,
        &mut cursor,
        &mut list_state,
        &mut mode,
        &lang,
    );

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    result
}

#[allow(clippy::too_many_arguments)]
fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ap: &ApplicationProgram,
    values: &mut HashMap<String, String>,
    mut pages: Vec<FormPage>,
    page_index: &mut usize,
    cursor: &mut usize,
    list_state: &mut ListState,
    mode: &mut Mode,
    lang: &str,
) -> Result<Option<HashMap<String, String>>> {
    loop {
        let (rows, field_indices) = flatten_page(pages.get(*page_index));
        update_selection(list_state, &field_indices, *cursor);

        terminal
            .draw(|f| draw(f, &rows, list_state, mode, &ap.name, &pages, *page_index))
            .context("drawing")?;

        let Event::Key(key) = event::read().context("reading key")? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }

        // Modal editing states operate on an owned copy so we can freely
        // write `*mode` back without borrow conflicts.
        match mode.clone() {
            Mode::Normal => {}
            Mode::TextEdit { id, mut buffer } => {
                match key.code {
                    KeyCode::Enter => {
                        values.insert(id.clone(), buffer.clone());
                        *mode = Mode::Normal;
                        pages = evaluate(ap, values, lang);
                        restore_cursor(&pages, *page_index, cursor, &id);
                    }
                    KeyCode::Esc => *mode = Mode::Normal,
                    KeyCode::Char(c) => {
                        buffer.push(c);
                        *mode = Mode::TextEdit { id, buffer };
                    }
                    KeyCode::Backspace => {
                        buffer.pop();
                        *mode = Mode::TextEdit { id, buffer };
                    }
                    _ => {}
                }
                continue;
            }
            Mode::EnumPick { id, options, cursor: mut pick_cursor } => {
                match key.code {
                    KeyCode::Esc => *mode = Mode::Normal,
                    KeyCode::Up | KeyCode::Char('k') => {
                        pick_cursor = pick_cursor.saturating_sub(1);
                        *mode = Mode::EnumPick { id, options, cursor: pick_cursor };
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        pick_cursor = (pick_cursor + 1) % options.len();
                        *mode = Mode::EnumPick { id, options, cursor: pick_cursor };
                    }
                    KeyCode::Enter => {
                        let value = options[pick_cursor].value.clone();
                        values.insert(id.clone(), value);
                        *mode = Mode::Normal;
                        pages = evaluate(ap, values, lang);
                        restore_cursor(&pages, *page_index, cursor, &id);
                    }
                    _ => {}
                }
                continue;
            }
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(None),
            KeyCode::Char('s') => return Ok(Some(values.clone())),
            KeyCode::Tab => {
                if !pages.is_empty() {
                    *page_index = (*page_index + 1) % pages.len();
                    *cursor = 0;
                }
            }
            KeyCode::BackTab => {
                if !pages.is_empty() {
                    *page_index = (*page_index + pages.len() - 1) % pages.len();
                    *cursor = 0;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !field_indices.is_empty() {
                    *cursor = (*cursor + 1) % field_indices.len();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                *cursor = cursor.saturating_sub(1);
            }
            KeyCode::Enter => {
                let Some(idx) = field_indices.get(*cursor) else {
                    continue;
                };
                let Some(Row::Field { id, kind, value, enabled, .. }) = rows.get(*idx) else {
                    continue;
                };
                if !enabled {
                    continue;
                }
                let id = id.clone();
                let kind = kind.clone();
                let current = values.get(&id).cloned().unwrap_or_else(|| value.clone());
                match kind {
                    FieldKind::Checkbox { off, on } => {
                        let next = if current == on { off } else { on };
                        values.insert(id.clone(), next);
                        pages = evaluate(ap, values, lang);
                        restore_cursor(&pages, *page_index, cursor, &id);
                    }
                    FieldKind::Enum { options } => {
                        let pos = options.iter().position(|o| o.value == current).unwrap_or(0);
                        *mode = Mode::EnumPick { id, options, cursor: pos };
                    }
                    FieldKind::Number { .. } | FieldKind::Text => {
                        *mode = Mode::TextEdit { id, buffer: current };
                    }
                }
            }
            _ => {}
        }
    }
}

fn flatten_page(page: Option<&FormPage>) -> (Vec<Row>, Vec<usize>) {
    let mut rows = Vec::new();
    let mut field_indices = Vec::new();
    if let Some(p) = page {
        for section in &p.sections {
            if let Some(t) = &section.title {
                rows.push(Row::Title(t.clone()));
            }
            for f in &section.fields {
                let idx = rows.len();
                rows.push(Row::Field {
                    id: f.id.clone(),
                    label: f.label.clone(),
                    kind: f.kind.clone(),
                    value: f.value.clone(),
                    enabled: f.enabled,
                });
                field_indices.push(idx);
            }
        }
    }
    (rows, field_indices)
}

fn restore_cursor(pages: &[FormPage], page_index: usize, cursor: &mut usize, id: &str) {
    let (rows, field_indices) = flatten_page(pages.get(page_index));
    if let Some(pos) = field_indices.iter().position(|&ri| {
        matches!(&rows[ri], Row::Field { id: fid, .. } if fid == id)
    }) {
        *cursor = pos;
    } else {
        *cursor = 0;
    }
}

fn update_selection(list_state: &mut ListState, field_indices: &[usize], cursor: usize) {
    if let Some(idx) = field_indices.get(cursor) {
        list_state.select(Some(*idx));
    } else {
        list_state.select(None);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw(
    f: &mut Frame,
    rows: &[Row],
    list_state: &mut ListState,
    mode: &Mode,
    device: &str,
    pages: &[FormPage],
    page_index: usize,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(f.area());

    let titles: Vec<String> = pages.iter().map(|p| p.title.clone()).collect();
    let tabs = Tabs::new(titles)
        .select(page_index)
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        .divider(" │ ");
    f.render_widget(tabs, chunks[0]);

    let items: Vec<ListItem> = rows.iter().map(render_row).collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(format!("Parameters — {device}")))
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, chunks[1], list_state);

    let help = match mode {
        Mode::TextEdit { buffer, .. } => format!("> {buffer}_   (Enter: confirm · Esc: cancel)"),
        Mode::EnumPick { .. } => {
            "↑/↓: choose value · Enter: confirm · Esc: cancel".to_string()
        }
        Mode::Normal => {
            "↑/↓ or j/k: move · Enter: edit · Tab: next page · s: save · q: quit".to_string()
        }
    };
    f.render_widget(Paragraph::new(help), chunks[2]);

    if let Mode::EnumPick { options, cursor, .. } = mode {
        draw_enum_popup(f, options, *cursor);
    }
}

fn draw_enum_popup(f: &mut Frame, options: &[EnumOption], cursor: usize) {
    let area = centered_rect(70, 80, f.area());
    f.render_widget(Clear, area);

    let items: Vec<ListItem> = options
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let text = format!("{} ({})", o.label, o.value);
            let style = if i == cursor {
                Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Span::styled(text, style))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Select value"),
    );
    f.render_widget(list, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn render_row(row: &Row) -> ListItem<'static> {
    match row {
        Row::Title(t) => ListItem::new(Line::from(Span::styled(
            t.clone(),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ))),
        Row::Field { label, kind, value, enabled, .. } => {
            let shown = match kind {
                FieldKind::Enum { options } => options
                    .iter()
                    .find(|o| o.value == *value)
                    .map(|o| format!("{} ({})", o.label, o.value))
                    .unwrap_or_else(|| value.clone()),
                _ => value.clone(),
            };
            let line = format!("{label:<48} {:<18} = {shown}", kind_tag(kind));
            let style = if *enabled {
                Style::default()
            } else {
                Style::default().fg(Color::DarkGray)
            };
            ListItem::new(Line::from(Span::styled(line, style)))
        }
    }
}

fn kind_tag(kind: &FieldKind) -> String {
    match kind {
        FieldKind::Checkbox { .. } => "[bool]".to_string(),
        FieldKind::Enum { .. } => "[enum]".to_string(),
        FieldKind::Number { min, max, unit } => {
            let range = match (min, max) {
                (Some(lo), Some(hi)) => format!("{lo}..{hi}"),
                (Some(lo), None) => format!("{lo}.."),
                (None, Some(hi)) => format!("..{hi}"),
                (None, None) => String::new(),
            };
            match unit {
                Some(u) => format!("[num {range} {u}]"),
                None => format!("[num {range}]"),
            }
        }
        FieldKind::Text => "[text]".to_string(),
    }
}
