//! Interactive (ratatui) device parameter editor.
//!
//! Renders the dynamic parameter form produced by `knx-form` and lets the
//! user change values; visibility is re-evaluated after every change.
//!
//! Layout: page list (ETS panes) on the left, the parameters of the current
//! page on the right, a detail line for the selected parameter (type, range,
//! default, key for `device-set`) and the key hints at the bottom. Values
//! that differ from the knxprod default — the ones stored in the project —
//! are marked with `●` and highlighted; the page list counts them per page.
//! Enumerated parameters open a popup listing every possible value; typed
//! values are validated against the parameter type before they are taken.

use std::collections::{HashMap, HashSet};
use std::io;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal};

use knx_form::{EnumOption, FieldKind, FormPage};
use knx_prod::{ApplicationProgram, Evaluation};

/// Below this terminal width the page list is hidden.
const SIDEBAR_MIN_WIDTH: u16 = 70;

mod theme {
    use ratatui::style::{Color, Modifier, Style};

    pub fn section() -> Style {
        Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)
    }
    pub fn rule() -> Style {
        Style::default().fg(Color::DarkGray)
    }
    pub fn value() -> Style {
        Style::default().fg(Color::Cyan)
    }
    pub fn changed() -> Style {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    }
    pub fn dim() -> Style {
        Style::default().fg(Color::DarkGray)
    }
    pub fn selected() -> Style {
        Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD)
    }
    pub fn key() -> Style {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    }
    pub fn error() -> Style {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    }
    pub fn ok() -> Style {
        Style::default().fg(Color::Green)
    }
    pub fn title() -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

#[derive(Clone)]
struct Field {
    id: String,
    label: String,
    kind: FieldKind,
    value: String,
    default: Option<String>,
    enabled: bool,
}

impl Field {
    /// Differs from the knxprod default, i.e. will be stored in the project.
    fn is_changed(&self) -> bool {
        self.enabled && self.default.as_deref().is_some_and(|d| d != self.value)
    }
}

#[derive(Clone)]
enum Row {
    Spacer,
    Title(String),
    Field(Field),
}

struct Page {
    title: String,
    rows: Vec<Row>,
    /// Indices into `rows` of the selectable (field) rows.
    fields: Vec<usize>,
    changed: usize,
}

enum Mode {
    Normal,
    TextEdit { id: String, label: String, hint: String, buffer: String, error: Option<String> },
    EnumPick { id: String, label: String, options: Vec<EnumOption>, default: Option<String>, list: ListState },
    Search { query: String },
    ConfirmQuit,
    Help,
}

enum Outcome {
    Continue,
    Save,
    Quit,
}

struct App<'a> {
    ap: &'a ApplicationProgram,
    lang: String,
    header: String,
    initial: HashMap<String, String>,
    values: HashMap<String, String>,
    pages: Vec<Page>,
    page: usize,
    /// Index into the current page's `fields`.
    cursor: usize,
    list: ListState,
    mode: Mode,
    /// One-shot message for the footer: (text, is_error).
    status: Option<(String, bool)>,
    last_search: String,
    unsaved: usize,
}

/// Run the parameter editor. Returns `Some(values)` on save, `None` on quit.
///
/// `header` names the device (shown in the title bar).
pub fn edit_device(
    ap: &ApplicationProgram,
    initial: &HashMap<String, String>,
    header: &str,
) -> Result<Option<HashMap<String, String>>> {
    let mut app = App::new(ap, initial, header);

    enable_raw_mode().context("enabling raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("entering alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("creating terminal")?;

    let result = app.run(&mut terminal);

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    result
}

impl<'a> App<'a> {
    fn new(ap: &'a ApplicationProgram, initial: &HashMap<String, String>, header: &str) -> Self {
        let lang = if ap.default_language.is_empty() {
            "en-US".to_string()
        } else {
            ap.default_language.clone()
        };
        let mut app = App {
            ap,
            lang,
            header: header.to_string(),
            initial: initial.clone(),
            values: initial.clone(),
            pages: Vec::new(),
            page: 0,
            cursor: 0,
            list: ListState::default(),
            mode: Mode::Normal,
            status: None,
            last_search: String::new(),
            unsaved: 0,
        };
        app.reevaluate(None);
        app
    }

    fn run<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<Option<HashMap<String, String>>> {
        loop {
            terminal.draw(|f| self.draw(f)).context("drawing")?;
            let Event::Key(key) = event::read().context("reading key")? else {
                continue;
            };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            match self.handle_key(key) {
                Outcome::Continue => {}
                Outcome::Save => return Ok(Some(self.values.clone())),
                Outcome::Quit => return Ok(None),
            }
        }
    }

    // ---- model -----------------------------------------------------------

    /// Rebuild the pages from the current values, keeping the current page
    /// and (if given) the cursor on parameter `keep`.
    fn reevaluate(&mut self, keep: Option<&str>) {
        let ev = Evaluation::new(self.ap, &self.values);
        let form = knx_form::evaluate_with(&ev, &self.lang);
        let old_title = self.pages.get(self.page).map(|p| p.title.clone());
        self.pages = form.iter().map(|p| build_page(p, &ev)).collect();

        // Pages appear and disappear as channels are switched on and off;
        // follow the current page by title.
        if let Some(title) = old_title {
            if self.pages.get(self.page).map(|p| &p.title) != Some(&title) {
                if let Some(i) = self.pages.iter().position(|p| p.title == title) {
                    self.page = i;
                }
            }
        }
        self.page = self.page.min(self.pages.len().saturating_sub(1));
        if let Some(id) = keep {
            let pos = self.current_fields().position(|f| f.id == id);
            if let Some(pos) = pos {
                self.cursor = pos;
            }
        }
        let n = self.page().map_or(0, |p| p.fields.len());
        self.cursor = self.cursor.min(n.saturating_sub(1));

        // Unsaved = effective value differs from the one at start.
        let keys: HashSet<&String> = self.values.keys().chain(self.initial.keys()).collect();
        let effective = |m: &HashMap<String, String>, k: &str| {
            m.get(k).map(String::as_str).or_else(|| ev.default_of(k)).map(str::to_string)
        };
        self.unsaved = keys
            .into_iter()
            .filter(|k| effective(&self.values, k) != effective(&self.initial, k))
            .count();
    }

    fn page(&self) -> Option<&Page> {
        self.pages.get(self.page)
    }

    fn current_fields(&self) -> impl Iterator<Item = &Field> {
        self.page().into_iter().flat_map(|p| {
            p.fields.iter().filter_map(|&i| match &p.rows[i] {
                Row::Field(f) => Some(f),
                _ => None,
            })
        })
    }

    fn selected(&self) -> Option<&Field> {
        self.current_fields().nth(self.cursor)
    }

    fn set(&mut self, id: &str, value: String) {
        self.values.insert(id.to_string(), value);
        self.reevaluate(Some(id));
    }

    // ---- input -----------------------------------------------------------

    fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Outcome::Quit;
        }
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => return self.handle_normal(key),
            Mode::Help => {}
            Mode::ConfirmQuit => {
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                    return Outcome::Quit;
                }
                if key.code == KeyCode::Char('s') {
                    return Outcome::Save;
                }
            }
            Mode::Search { mut query } => match key.code {
                KeyCode::Enter => {
                    self.last_search = query;
                    self.search_next();
                }
                KeyCode::Esc => {}
                KeyCode::Backspace => {
                    query.pop();
                    self.mode = Mode::Search { query };
                }
                KeyCode::Char(c) => {
                    query.push(c);
                    self.mode = Mode::Search { query };
                }
                _ => self.mode = Mode::Search { query },
            },
            Mode::TextEdit { id, label, hint, mut buffer, .. } => match key.code {
                KeyCode::Enter => match knx_form::validate(self.ap, &id, &buffer) {
                    Ok(()) => self.set(&id, buffer),
                    Err(e) => {
                        let error = Some(format!("{e:#}"));
                        self.mode = Mode::TextEdit { id, label, hint, buffer, error };
                    }
                },
                KeyCode::Esc => {}
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.mode = Mode::TextEdit { id, label, hint, buffer: String::new(), error: None };
                }
                KeyCode::Char(c) => {
                    buffer.push(c);
                    self.mode = Mode::TextEdit { id, label, hint, buffer, error: None };
                }
                KeyCode::Backspace => {
                    buffer.pop();
                    self.mode = Mode::TextEdit { id, label, hint, buffer, error: None };
                }
                _ => self.mode = Mode::TextEdit { id, label, hint, buffer, error: None },
            },
            Mode::EnumPick { id, label, options, default, mut list } => {
                let n = options.len();
                let at = list.selected().unwrap_or(0);
                match key.code {
                    KeyCode::Esc => return Outcome::Continue,
                    KeyCode::Enter => {
                        let value = options[at].value.clone();
                        self.set(&id, value);
                        return Outcome::Continue;
                    }
                    KeyCode::Up | KeyCode::Char('k') => list.select(Some((at + n - 1) % n)),
                    KeyCode::Down | KeyCode::Char('j') => list.select(Some((at + 1) % n)),
                    KeyCode::Home | KeyCode::Char('g') => list.select(Some(0)),
                    KeyCode::End | KeyCode::Char('G') => list.select(Some(n - 1)),
                    KeyCode::PageUp => list.select(Some(at.saturating_sub(10))),
                    KeyCode::PageDown => list.select(Some((at + 10).min(n - 1))),
                    _ => {}
                }
                self.mode = Mode::EnumPick { id, label, options, default, list };
            }
        }
        Outcome::Continue
    }

    fn handle_normal(&mut self, key: KeyEvent) -> Outcome {
        self.status = None;
        let n = self.page().map_or(0, |p| p.fields.len());
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.unsaved == 0 {
                    return Outcome::Quit;
                }
                self.mode = Mode::ConfirmQuit;
            }
            KeyCode::Char('s') => return Outcome::Save,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Tab | KeyCode::Char(']') => self.goto_page(self.page + 1),
            KeyCode::BackTab | KeyCode::Char('[') => {
                self.goto_page(self.page + self.pages.len().max(1) - 1)
            }
            KeyCode::Down | KeyCode::Char('j') if n > 0 => self.cursor = (self.cursor + 1) % n,
            KeyCode::Up | KeyCode::Char('k') if n > 0 => self.cursor = (self.cursor + n - 1) % n,
            KeyCode::PageDown => self.cursor = (self.cursor + 10).min(n.saturating_sub(1)),
            KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(10),
            KeyCode::Home | KeyCode::Char('g') => self.cursor = 0,
            KeyCode::End | KeyCode::Char('G') => self.cursor = n.saturating_sub(1),
            KeyCode::Enter => self.edit(),
            KeyCode::Char(' ') | KeyCode::Right | KeyCode::Char('l') => self.step(1),
            KeyCode::Left | KeyCode::Char('h') => self.step(-1),
            KeyCode::Char('d') => self.reset_default(),
            KeyCode::Char('/') => self.mode = Mode::Search { query: String::new() },
            KeyCode::Char('n') => self.search_next(),
            _ => {}
        }
        Outcome::Continue
    }

    fn goto_page(&mut self, page: usize) {
        if !self.pages.is_empty() {
            self.page = page % self.pages.len();
            self.cursor = 0;
            self.list = ListState::default();
        }
    }

    /// The selected field if it can be changed, else a status message.
    fn editable(&mut self) -> Option<Field> {
        match self.selected() {
            Some(f) if f.enabled => Some(f.clone()),
            Some(_) => {
                self.status = Some(("read-only parameter".to_string(), true));
                None
            }
            None => None,
        }
    }

    fn edit(&mut self) {
        let Some(f) = self.editable() else { return };
        match &f.kind {
            FieldKind::Checkbox { .. } => self.step(1),
            FieldKind::Enum { options } => {
                let pos = options.iter().position(|o| o.value == f.value).unwrap_or(0);
                self.mode = Mode::EnumPick {
                    id: f.id.clone(),
                    label: f.label.trim().to_string(),
                    options: options.clone(),
                    default: f.default.clone(),
                    list: ListState::default().with_selected(Some(pos)),
                };
            }
            FieldKind::Number { .. } | FieldKind::Text => {
                self.mode = Mode::TextEdit {
                    id: f.id.clone(),
                    label: f.label.trim().to_string(),
                    hint: describe(&f),
                    buffer: f.value.clone(),
                    error: None,
                };
            }
        }
    }

    /// Change the value in place: toggle a checkbox, move an enum to the
    /// next/previous option, or add `delta` to an integer.
    fn step(&mut self, delta: i64) {
        let Some(f) = self.editable() else { return };
        let next = match &f.kind {
            FieldKind::Checkbox { off, on } => {
                Some(if f.value == on.value { off.value.clone() } else { on.value.clone() })
            }
            FieldKind::Enum { options } if !options.is_empty() => {
                let n = options.len() as i64;
                let pos = options.iter().position(|o| o.value == f.value).unwrap_or(0) as i64;
                Some(options[(pos + delta).rem_euclid(n) as usize].value.clone())
            }
            FieldKind::Number { min, max, .. } => step_number(&f.value, delta, min, max),
            _ => None,
        };
        match next {
            Some(v) if knx_form::validate(self.ap, &f.id, &v).is_ok() => self.set(&f.id, v),
            Some(_) => {}
            None => self.status = Some(("press Enter to type a value".to_string(), false)),
        }
    }

    fn reset_default(&mut self) {
        let Some(f) = self.editable() else { return };
        self.values.remove(&f.id);
        self.reevaluate(Some(&f.id));
    }

    /// Jump to the next field (after the cursor, across pages, wrapping)
    /// whose label or shown value contains the last search text.
    fn search_next(&mut self) {
        let needle = self.last_search.to_lowercase();
        if needle.is_empty() {
            return;
        }
        let hits = |f: &Field| {
            f.label.to_lowercase().contains(&needle)
                || value_text(f).to_lowercase().contains(&needle)
        };
        let order: Vec<(usize, usize)> = self
            .pages
            .iter()
            .enumerate()
            .flat_map(|(pi, p)| (0..p.fields.len()).map(move |fi| (pi, fi)))
            .collect();
        let start = order
            .iter()
            .position(|&pos| pos == (self.page, self.cursor))
            .map_or(0, |i| i + 1);
        let found = (0..order.len()).map(|i| order[(start + i) % order.len()]).find(|&(pi, fi)| {
            let p = &self.pages[pi];
            matches!(&p.rows[p.fields[fi]], Row::Field(f) if hits(f))
        });
        match found {
            Some((pi, fi)) => {
                if pi != self.page {
                    self.list = ListState::default();
                }
                self.page = pi;
                self.cursor = fi;
                self.status = Some((format!("/{}  (n: next match)", self.last_search), false));
            }
            None => self.status = Some((format!("no match for \"{}\"", self.last_search), true)),
        }
    }

    // ---- drawing ---------------------------------------------------------

    fn draw(&mut self, f: &mut Frame) {
        let [header, body, detail, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .areas(f.area());

        self.draw_header(f, header);

        let sidebar_w = if body.width >= SIDEBAR_MIN_WIDTH {
            let longest = self.pages.iter().map(|p| p.title.chars().count()).max().unwrap_or(0);
            (longest as u16 + 8).clamp(16, body.width / 3)
        } else {
            0
        };
        let [side, main] =
            Layout::horizontal([Constraint::Length(sidebar_w), Constraint::Min(20)]).areas(body);
        if sidebar_w > 0 {
            self.draw_pages(f, side);
        }
        self.draw_fields(f, main, sidebar_w == 0);
        self.draw_detail(f, detail);
        self.draw_footer(f, footer);

        match &mut self.mode {
            Mode::EnumPick { label, options, default, list, .. } => {
                let current = self.pages.get(self.page).and_then(|p| {
                    p.fields.get(self.cursor).and_then(|&i| match &p.rows[i] {
                        Row::Field(f) => Some(f.value.clone()),
                        _ => None,
                    })
                });
                draw_enum_popup(f, label, options, current.as_deref(), default.as_deref(), list);
            }
            Mode::TextEdit { label, hint, buffer, error, .. } => {
                draw_text_popup(f, label, hint, buffer, error.as_deref());
            }
            Mode::Help => draw_help(f),
            _ => {}
        }
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let right = if self.unsaved > 0 {
            let s = if self.unsaved == 1 { "" } else { "s" };
            Span::styled(format!("● {} unsaved change{s} ", self.unsaved), theme::changed())
        } else {
            Span::styled("no changes ", theme::dim())
        };
        let badge = Span::styled(
            " knxcfg edit ",
            Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD),
        );
        let room = (area.width as usize).saturating_sub(badge.width() + right.width() + 2);
        let header = truncate(&self.header, room);
        let room = room.saturating_sub(header.chars().count() + 2);
        let mut left = vec![badge, Span::raw(" "), Span::styled(header, theme::title())];
        if room > 0 {
            left.push(Span::styled(format!("  {}", truncate(&self.ap.name, room)), theme::dim()));
        }
        let used: usize = left.iter().map(|s| s.width()).sum::<usize>() + right.width();
        left.push(Span::raw(" ".repeat((area.width as usize).saturating_sub(used))));
        left.push(right);
        f.render_widget(Paragraph::new(Line::from(left)), area);
    }

    fn draw_pages(&self, f: &mut Frame, area: Rect) {
        let inner_w = area.width.saturating_sub(4) as usize;
        let items: Vec<ListItem> = self
            .pages
            .iter()
            .map(|p| {
                let badge = if p.changed > 0 { format!(" ●{}", p.changed) } else { String::new() };
                let title = truncate(&p.title, inner_w.saturating_sub(badge.chars().count()));
                let pad = inner_w.saturating_sub(title.chars().count() + badge.chars().count());
                ListItem::new(Line::from(vec![
                    Span::raw(title),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(badge, theme::changed()),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).border_style(theme::dim()).title(" Pages "))
            .highlight_style(theme::selected())
            .highlight_symbol("▌");
        let mut state = ListState::default().with_selected(Some(self.page));
        f.render_stateful_widget(list, area, &mut state);
    }

    fn draw_fields(&mut self, f: &mut Frame, area: Rect, show_position: bool) {
        let Some(page) = self.pages.get(self.page) else {
            let p = Paragraph::new("This application program has no parameters.")
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(p, area);
            return;
        };
        let inner_w = area.width.saturating_sub(4) as usize;
        let longest = page
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Field(fl) => Some(fl.label.trim_end().chars().count()),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let label_w = longest.min(inner_w * 55 / 100);

        let items: Vec<ListItem> = page
            .rows
            .iter()
            .map(|r| ListItem::new(render_row(r, label_w, inner_w)))
            .collect();
        let mut title = format!(" {} ", page.title);
        if show_position {
            title = format!(" {} ({}/{}) ", page.title, self.page + 1, self.pages.len());
        }
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::dim())
                    .title(Span::styled(title, theme::title())),
            )
            .highlight_style(theme::selected())
            .highlight_symbol("▌");
        self.list.select(page.fields.get(self.cursor).copied());
        f.render_stateful_widget(list, area, &mut self.list);
    }

    fn draw_detail(&self, f: &mut Frame, area: Rect) {
        let lines = match self.selected() {
            None => vec![Line::default(), Line::default()],
            Some(fl) => {
                let mut first = vec![
                    Span::styled(format!(" {}", fl.label.trim()), theme::title()),
                    Span::styled(format!("   {}", describe(fl)), theme::dim()),
                ];
                if !fl.enabled {
                    first.push(Span::styled("   read-only", theme::error()));
                }
                let mut second = vec![Span::styled(" default: ", theme::dim())];
                match &fl.default {
                    Some(d) => second.push(Span::styled(
                        display_value(&fl.kind, d),
                        if fl.is_changed() { theme::dim() } else { theme::value() },
                    )),
                    None => second.push(Span::styled("—", theme::dim())),
                }
                if fl.is_changed() {
                    second.push(Span::styled("   ● changed (d: reset)", theme::changed()));
                }
                second.push(Span::styled(
                    format!("   key: {}", knx_prod::eval::short_key(self.ap, &fl.id)),
                    theme::dim(),
                ));
                vec![Line::from(first), Line::from(second)]
            }
        };
        f.render_widget(Paragraph::new(lines), area);
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let line = match &self.mode {
            Mode::Search { query } => Line::from(vec![
                Span::styled(" /", theme::key()),
                Span::raw(query.clone()),
                Span::styled("▏", theme::key()),
                Span::styled("   Enter: find · Esc: cancel", theme::dim()),
            ]),
            Mode::ConfirmQuit => Line::from(vec![
                Span::styled(
                    format!(" Discard {} unsaved change(s)? ", self.unsaved),
                    theme::error(),
                ),
                Span::styled("y", theme::key()),
                Span::raw(" discard  "),
                Span::styled("s", theme::key()),
                Span::raw(" save  "),
                Span::styled("any other key", theme::key()),
                Span::raw(" back"),
            ]),
            _ => match &self.status {
                Some((msg, true)) => Line::from(Span::styled(format!(" {msg}"), theme::error())),
                Some((msg, false)) => Line::from(Span::styled(format!(" {msg}"), theme::ok())),
                None => hints(&[
                    ("↑↓", "move"),
                    ("←→", "change"),
                    ("⏎", "edit"),
                    ("d", "default"),
                    ("Tab", "page"),
                    ("/", "search"),
                    ("s", "save"),
                    ("q", "quit"),
                    ("?", "help"),
                ]),
            },
        };
        f.render_widget(Paragraph::new(line), area);
    }
}

fn build_page(p: &FormPage, ev: &Evaluation) -> Page {
    let mut rows = Vec::new();
    let mut fields = Vec::new();
    let mut changed = 0;
    for section in &p.sections {
        if let Some(t) = &section.title {
            if !rows.is_empty() {
                rows.push(Row::Spacer);
            }
            rows.push(Row::Title(t.clone()));
        }
        for fl in &section.fields {
            let field = Field {
                id: fl.id.clone(),
                label: fl.label.clone(),
                kind: fl.kind.clone(),
                value: fl.value.clone(),
                default: ev.default_of(&fl.id).map(str::to_string),
                enabled: fl.enabled,
            };
            if field.is_changed() {
                changed += 1;
            }
            fields.push(rows.len());
            rows.push(Row::Field(field));
        }
    }
    Page { title: p.title.clone(), rows, fields, changed }
}

fn render_row(row: &Row, label_w: usize, inner_w: usize) -> Line<'static> {
    match row {
        Row::Spacer => Line::default(),
        Row::Title(t) => {
            let t = truncate(t, inner_w.saturating_sub(4));
            let rule = "─".repeat(inner_w.saturating_sub(t.chars().count() + 2));
            Line::from(vec![
                Span::styled(t, theme::section()),
                Span::raw(" "),
                Span::styled(rule, theme::rule()),
            ])
        }
        Row::Field(f) => {
            let changed = f.is_changed();
            let label = truncate(f.label.trim_end(), label_w);
            let pad = label_w.saturating_sub(label.chars().count());
            let (label_style, value_style) = if !f.enabled {
                (theme::dim(), theme::dim())
            } else if changed {
                (Style::default(), theme::changed())
            } else {
                (Style::default(), theme::value())
            };
            let value = value_text(f);
            let value_span = if value.is_empty() {
                Span::styled("‹empty›", theme::dim())
            } else {
                let value_w = inner_w.saturating_sub(label_w + 4);
                Span::styled(truncate(&value, value_w), value_style)
            };
            Line::from(vec![
                Span::styled(if changed { "● " } else { "  " }, theme::changed()),
                Span::styled(label, label_style),
                Span::raw(" ".repeat(pad + 2)),
                value_span,
            ])
        }
    }
}

/// The value as shown in the list: option label, checkbox, number + unit.
fn value_text(f: &Field) -> String {
    display_value(&f.kind, &f.value)
}

fn display_value(kind: &FieldKind, value: &str) -> String {
    match kind {
        FieldKind::Checkbox { off, on } => {
            if value == on.value {
                format!("☑ {}", on.label)
            } else if value == off.value {
                format!("☐ {}", off.label)
            } else {
                format!("? {value}")
            }
        }
        FieldKind::Enum { options } => options
            .iter()
            .find(|o| o.value == value)
            .map(|o| o.label.clone())
            .unwrap_or_else(|| value.to_string()),
        FieldKind::Number { unit: Some(u), .. } if !value.is_empty() => format!("{value} {u}"),
        _ => value.to_string(),
    }
}

/// Type and range of a field, e.g. "number 0…30000 s" or "choice of 5".
fn describe(f: &Field) -> String {
    match &f.kind {
        FieldKind::Checkbox { .. } => "on/off".to_string(),
        FieldKind::Enum { options } => {
            let raw = options
                .iter()
                .find(|o| o.value == f.value)
                .map(|o| format!(" · raw value {}", o.value))
                .unwrap_or_default();
            format!("choice of {}{raw}", options.len())
        }
        FieldKind::Number { min, max, unit } => {
            let range = match (min, max) {
                (Some(lo), Some(hi)) => format!(" {lo}…{hi}"),
                (Some(lo), None) => format!(" ≥ {lo}"),
                (None, Some(hi)) => format!(" ≤ {hi}"),
                (None, None) => String::new(),
            };
            let unit = unit.as_ref().map(|u| format!(" {u}")).unwrap_or_default();
            format!("number{range}{unit}")
        }
        FieldKind::Text => "text".to_string(),
    }
}

/// Integer `value + delta`, clamped to the range; `None` for non-integers.
fn step_number(value: &str, delta: i64, min: &Option<String>, max: &Option<String>) -> Option<String> {
    let v: i64 = value.trim().parse().ok()?;
    let mut next = v.saturating_add(delta);
    if let Some(lo) = min.as_deref().and_then(|s| s.parse::<i64>().ok()) {
        next = next.max(lo);
    }
    if let Some(hi) = max.as_deref().and_then(|s| s.parse::<i64>().ok()) {
        next = next.min(hi);
    }
    Some(next.to_string())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let mut t: String = s.chars().take(max - 1).collect();
        t.push('…');
        t
    }
}

fn hints(keys: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (k, what) in keys {
        spans.push(Span::styled(k.to_string(), theme::key()));
        spans.push(Span::styled(format!(" {what}  "), theme::dim()));
    }
    Line::from(spans)
}

fn draw_enum_popup(
    f: &mut Frame,
    label: &str,
    options: &[EnumOption],
    current: Option<&str>,
    default: Option<&str>,
    list: &mut ListState,
) {
    let widest = options
        .iter()
        .map(|o| o.label.chars().count() + o.value.chars().count() + 16)
        .chain([label.chars().count() + 4])
        .max()
        .unwrap_or(20);
    let area = popup_rect(f.area(), widest as u16 + 4, options.len() as u16 + 2);
    f.render_widget(Clear, area);

    let items: Vec<ListItem> = options
        .iter()
        .map(|o| {
            let mark = if Some(o.value.as_str()) == current { "● " } else { "  " };
            let mut spans = vec![
                Span::styled(mark, theme::changed()),
                Span::raw(o.label.clone()),
                Span::styled(format!("  ({})", o.value), theme::dim()),
            ];
            if Some(o.value.as_str()) == default {
                spans.push(Span::styled("  default", theme::value()));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list_widget = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(Span::styled(format!(" {label} "), theme::title())),
        )
        .highlight_style(theme::selected());
    f.render_stateful_widget(list_widget, area, list);
}

fn draw_text_popup(f: &mut Frame, label: &str, hint: &str, buffer: &str, error: Option<&str>) {
    let width = (label.chars().count().max(buffer.chars().count()).max(40) + 6) as u16;
    let area = popup_rect(f.area(), width, 5);
    f.render_widget(Clear, area);
    let lines = vec![
        Line::from(vec![
            Span::raw(buffer.to_string()),
            Span::styled("▏", theme::key()),
        ]),
        match error {
            Some(e) => Line::from(Span::styled(e.to_string(), theme::error())),
            None => Line::from(Span::styled(hint.to_string(), theme::dim())),
        },
        hints(&[("⏎", "ok"), ("Esc", "cancel"), ("^U", "clear")]),
    ];
    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(Span::styled(format!(" {label} "), theme::title())),
    );
    f.render_widget(p, area);
}

fn draw_help(f: &mut Frame) {
    let rows: &[(&str, &str)] = &[
        ("↑ ↓  j k", "move between parameters"),
        ("PgUp PgDn  g G", "jump 10 / to top / to bottom"),
        ("Tab S-Tab  ] [", "next / previous page"),
        ("← →  h l  Space", "change value in place (toggle, next option, ±1)"),
        ("Enter", "edit: choose from list or type a value"),
        ("d", "reset parameter to its default"),
        ("/  n", "search labels and values / next match"),
        ("s", "save and exit"),
        ("q  Esc", "quit (asks when there are unsaved changes)"),
        ("", ""),
        ("●", "differs from the default (stored in the project)"),
        ("●3", "number of such values on a page"),
    ];
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, what)| {
            Line::from(vec![
                Span::styled(format!(" {k:<17}"), theme::key()),
                Span::raw(what.to_string()),
            ])
        })
        .collect();
    let area = popup_rect(f.area(), 72, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(Span::styled(" Keys — any key to close ", theme::title())),
    );
    f.render_widget(p, area);
}

/// A centered rectangle of (at most) `width` × `height`.
fn popup_rect(r: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(r.width.saturating_sub(4)).max(10.min(r.width));
    let h = height.min(r.height.saturating_sub(2)).max(3.min(r.height));
    Rect {
        x: r.x + (r.width - w) / 2,
        y: r.y + (r.height - h) / 2,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;

    fn ap() -> Option<ApplicationProgram> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../refs/knxprod/MDT_KP_AKU_03_Universal_Actuator_V41a.knxprod");
        if !path.is_file() {
            eprintln!("skipping: {} not found", path.display());
            return None;
        }
        let kp = knx_prod::KnxProd::open(path).unwrap();
        Some(kp.application_program("M-0083_H-286-1_HP-00C4-41-29EF").unwrap())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn screen(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn step_number_clamps() {
        let (lo, hi) = (Some("2".to_string()), Some("240".to_string()));
        assert_eq!(step_number("2", -1, &lo, &hi).as_deref(), Some("2"));
        assert_eq!(step_number("2", 1, &lo, &hi).as_deref(), Some("3"));
        assert_eq!(step_number("240", 1, &lo, &hi).as_deref(), Some("240"));
        assert_eq!(step_number("0.5", 1, &None, &None), None);
    }

    #[test]
    fn enabling_channels_adds_pages_and_tracks_changes() {
        let Some(ap) = ap() else { return };
        let mut app = App::new(&ap, &HashMap::new(), "1.1.1");
        assert_eq!(app.unsaved, 0);
        let pages = app.pages.len();

        // "Channel selection" page, "Channel A / B": not active → switch.
        app.goto_page(1);
        app.handle_key(key(KeyCode::Left));
        assert_eq!(app.unsaved, 1);
        assert_eq!(app.pages[1].changed, 1);
        assert!(app.selected().unwrap().is_changed());

        // Pick "switching" for channel A.
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Right));
        assert!(app.pages.len() > pages, "channel pages appear");
        assert!(app.pages.iter().any(|p| p.title == "Channel A"));

        let s = screen(&mut app, 120, 30);
        assert!(s.contains("Channel A"), "{s}");
        assert!(!s.contains("{{"), "{s}");
        assert!(s.contains("2 unsaved changes"), "{s}");

        // Reset to default, then quitting no longer asks.
        app.handle_key(key(KeyCode::Char('d')));
        app.handle_key(key(KeyCode::Up));
        app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(app.unsaved, 0);
        assert!(matches!(app.handle_key(key(KeyCode::Char('q'))), Outcome::Quit));
    }

    #[test]
    fn quit_with_changes_asks_first() {
        let Some(ap) = ap() else { return };
        let mut app = App::new(&ap, &HashMap::new(), "1.1.1");
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Right));
        assert!(matches!(app.handle_key(key(KeyCode::Char('q'))), Outcome::Continue));
        assert!(matches!(app.mode, Mode::ConfirmQuit));
        assert!(matches!(app.handle_key(key(KeyCode::Char('n'))), Outcome::Continue));
        assert!(matches!(app.mode, Mode::Normal));
        app.handle_key(key(KeyCode::Char('q')));
        assert!(matches!(app.handle_key(key(KeyCode::Char('y'))), Outcome::Quit));
    }

    #[test]
    fn search_jumps_across_pages() {
        let Some(ap) = ap() else { return };
        let mut app = App::new(&ap, &HashMap::new(), "1.1.1");
        app.handle_key(key(KeyCode::Char('/')));
        for c in "channel c".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.pages[app.page].title, "Channel selection");
        assert!(app.selected().unwrap().label.contains("Channel C"));
    }
}
