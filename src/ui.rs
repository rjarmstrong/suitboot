use std::io::Stdout;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::{Frame, Terminal};

const TEAL: Color = Color::Rgb(45, 212, 191);
const AMBER: Color = Color::Rgb(251, 191, 36);
const DIM: Color = Color::Rgb(148, 163, 184);

/// One screen, redrawn in place. Each frame is only the current task.
pub struct Ui {
    terminal: Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    active: bool,
    heading: String,
    notes: Vec<String>,
    progress: Option<Progress>,
    action: Vec<String>,
    choices: Vec<String>,
    selected: usize,
    input: String,
    footer: String,
    drawn: bool,
}

struct Progress {
    ratio: f64,
    detail: String,
}

impl Ui {
    pub fn open() -> Result<Self> {
        let terminal = ratatui::try_init().context("could not open the terminal")?;
        let ui = Self {
            terminal,
            active: true,
            heading: String::new(),
            notes: Vec::new(),
            progress: None,
            action: Vec::new(),
            choices: Vec::new(),
            selected: 0,
            input: String::new(),
            footer: String::new(),
            drawn: false,
        };
        Ok(ui)
    }

    /// The current task. Does not ask for a keypress.
    pub fn show(&mut self, heading: &str, notes: &[String]) -> Result<()> {
        self.heading = heading.to_string();
        self.notes = notes.to_vec();
        self.choices.clear();
        self.input.clear();
        self.footer = "ctrl-c  stop".to_string();
        self.draw()
    }

    /// Amber box. Empty lines hide it.
    pub fn set_action(&mut self, lines: &[String]) -> Result<()> {
        self.action = lines.to_vec();
        self.draw()
    }

    pub fn clear_action(&mut self) -> Result<()> {
        self.action.clear();
        self.draw()
    }

    pub fn set_progress(&mut self, ratio: f64, detail: &str) -> Result<()> {
        self.progress = Some(Progress {
            ratio: ratio.clamp(0.0, 1.0),
            detail: detail.to_string(),
        });
        self.draw()
    }

    pub fn clear_progress(&mut self) -> Result<()> {
        self.progress = None;
        self.draw()
    }

    /// Returns the chosen index, or `None` when cancelled.
    pub fn choose(
        &mut self,
        heading: &str,
        notes: &[String],
        items: &[String],
    ) -> Result<Option<usize>> {
        self.heading = heading.to_string();
        self.notes = notes.to_vec();
        self.choices = items.to_vec();
        self.selected = 0;
        self.action.clear();
        self.progress = None;
        self.input.clear();
        self.footer = "↑↓  move     enter  select     esc  cancel".to_string();
        self.draw()?;
        loop {
            if !event::poll(Duration::from_millis(200)).context("terminal input failed")? {
                continue;
            }
            let Event::Key(key) = event::read().context("terminal input failed")? else {
                self.draw()?;
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Up => {
                    self.selected = self.selected.saturating_sub(1);
                    self.draw()?;
                }
                KeyCode::Down => {
                    if self.selected + 1 < self.choices.len() {
                        self.selected += 1;
                    }
                    self.draw()?;
                }
                KeyCode::Enter if !self.choices.is_empty() => {
                    self.choices.clear();
                    return Ok(Some(self.selected));
                }
                KeyCode::Esc => {
                    self.choices.clear();
                    return Ok(None);
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.choices.clear();
                    return Ok(None);
                }
                _ => {}
            }
        }
    }

    /// Returns the typed line, or `None` when cancelled.
    pub fn prompt(
        &mut self,
        heading: &str,
        notes: &[String],
        action: &[String],
    ) -> Result<Option<String>> {
        self.heading = heading.to_string();
        self.notes = notes.to_vec();
        self.action = action.to_vec();
        self.choices.clear();
        self.progress = None;
        self.input.clear();
        self.footer = "type, then enter     esc  cancel".to_string();
        self.draw()?;
        loop {
            if !event::poll(Duration::from_millis(200)).context("terminal input failed")? {
                continue;
            }
            let Event::Key(key) = event::read().context("terminal input failed")? else {
                self.draw()?;
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Enter => {
                    let value = self.input.trim().to_string();
                    self.input.clear();
                    return Ok(Some(value));
                }
                KeyCode::Backspace => {
                    self.input.pop();
                    self.draw()?;
                }
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None);
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.input.push(ch);
                    self.draw()?;
                }
                _ => {}
            }
        }
    }

    pub fn wait_enter(&mut self) -> Result<()> {
        self.footer = "enter  close".to_string();
        self.draw()?;
        loop {
            if !event::poll(Duration::from_millis(200)).context("terminal input failed")? {
                continue;
            }
            let Event::Key(key) = event::read().context("terminal input failed")? else {
                self.draw()?;
                continue;
            };
            if key.kind == KeyEventKind::Press
                && (key.code == KeyCode::Enter
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)))
            {
                return Ok(());
            }
        }
    }

    /// True when ctrl-c was pressed since the last check. Used while a write is running.
    pub fn ctrl_c(&mut self) -> bool {
        let mut hit = false;
        while event::poll(Duration::ZERO).unwrap_or(false) {
            let Ok(Event::Key(key)) = event::read() else {
                continue;
            };
            if key.kind == KeyEventKind::Press
                && key.code == KeyCode::Char('c')
                && key.modifiers.contains(KeyModifiers::CONTROL)
            {
                hit = true;
            }
        }
        hit
    }

    /// Leaves the screen so a normal prompt (the macOS password) can run, then redraws.
    pub fn suspend<T>(&mut self, f: impl FnOnce() -> T) -> T {
        self.active = false;
        ratatui::restore();
        let value = f();
        match ratatui::try_init() {
            Ok(terminal) => {
                self.terminal = terminal;
                self.active = true;
                self.drawn = false;
                let _ = self.draw();
            }
            Err(_) => self.active = false,
        }
        value
    }

    fn draw(&mut self) -> Result<()> {
        if !self.drawn {
            self.terminal
                .clear()
                .context("could not clear the screen")?;
            self.drawn = true;
        }
        let snap = Snapshot {
            heading: self.heading.clone(),
            notes: self.notes.clone(),
            progress: self.progress.as_ref().map(|p| (p.ratio, p.detail.clone())),
            action: self.action.clone(),
            choices: self.choices.clone(),
            selected: self.selected,
            input: self.input.clone(),
            footer: self.footer.clone(),
        };
        self.terminal
            .draw(|frame| paint(frame, &snap))
            .context("could not draw the screen")?;
        Ok(())
    }
}

struct Snapshot {
    heading: String,
    notes: Vec<String>,
    progress: Option<(f64, String)>,
    action: Vec<String>,
    choices: Vec<String>,
    selected: usize,
    input: String,
    footer: String,
}

impl Drop for Ui {
    fn drop(&mut self) {
        if self.active {
            ratatui::restore();
            self.active = false;
        }
    }
}

fn paint(frame: &mut Frame, view: &Snapshot) {
    let Snapshot {
        heading,
        notes,
        progress,
        action,
        choices,
        selected,
        input,
        footer,
    } = view;
    let area = frame.area();
    let note_h = notes.len().min(6) as u16;
    let progress_h = u16::from(progress.is_some());
    let action_h = if action.is_empty() && input.is_empty() {
        0
    } else {
        (action.len() as u16 + 3).min(8)
    };
    let chunks = Layout::vertical([
        Constraint::Length(1 + note_h),
        Constraint::Length(progress_h),
        Constraint::Min(0),
        Constraint::Length(action_h),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(guidance(heading, notes), chunks[0]);
    if let Some((ratio, detail)) = progress {
        frame.render_widget(
            Gauge::default()
                .ratio(*ratio)
                .label(detail.as_str())
                .gauge_style(Style::default().fg(TEAL)),
            chunks[1],
        );
    }
    frame.render_widget(choice_list(choices, *selected, chunks[2].height), chunks[2]);
    if action_h > 0 {
        frame.render_widget(action_box(action, input), chunks[3]);
    }
    frame.render_widget(
        Paragraph::new(Span::styled(footer, Style::default().fg(DIM))),
        chunks[4],
    );
}

fn guidance(heading: &str, notes: &[String]) -> Paragraph<'static> {
    let mut lines = vec![Line::from(Span::styled(
        heading.to_string(),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))];
    for note in notes.iter().take(6) {
        lines.push(Line::from(Span::styled(
            note.clone(),
            Style::default().fg(DIM),
        )));
    }
    Paragraph::new(lines)
}

fn choice_list(choices: &[String], selected: usize, height: u16) -> Paragraph<'static> {
    if choices.is_empty() || height == 0 {
        return Paragraph::new("");
    }
    let visible = height as usize;
    let start = selected
        .saturating_sub(visible / 2)
        .min(choices.len().saturating_sub(visible));
    let lines: Vec<Line> = choices
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(index, item)| {
            if index == selected {
                Line::from(Span::styled(
                    format!("▸ {item}"),
                    Style::default().fg(TEAL).add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(Span::raw(format!("  {item}")))
            }
        })
        .collect();
    Paragraph::new(lines)
}

fn action_box(lines: &[String], input: &str) -> Paragraph<'static> {
    let mut body: Vec<Line> = lines
        .iter()
        .map(|line| {
            Line::from(Span::styled(
                line.clone(),
                Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
            ))
        })
        .collect();
    if !input.is_empty() || lines.iter().any(|line| line.starts_with("Type ")) {
        body.push(Line::from(Span::styled(
            format!("{input}_"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )));
    }
    Paragraph::new(body).block(
        Block::default()
            .title(Span::styled(
                " Action ",
                Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(AMBER)),
    )
}
