//! The worker manager: a terminal UI over the worker daemon, laid out like
//! lazydocker. Workers on the left with what is coming up and the daemon's
//! health, the selected worker's logs / info / config on the right.
//! `Tab` / `Shift+Tab` (or `1` `2` `3`) move between the three left panels;
//! the right side follows whichever has focus.
//!
//! It only talks to the daemon over its socket and reads log files, so it can
//! be opened and closed at any time without touching the running workers.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{Local, TimeZone};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use super::WorkerPaths;
use super::ipc::{DaemonInfo, Request, WorkerState, WorkerStatus, human_duration, relative, request};
use crate::error::Result;
use crate::paths::log_file;

const REFRESH: Duration = Duration::from_millis(500);
/// How much of the end of a log file is read.
const TAIL_BYTES: u64 = 128 * 1024;

const ACCENT: Color = Color::Rgb(255, 170, 60);
const DIM: Color = Color::Rgb(120, 130, 150);
const HINT: Color = Color::Rgb(130, 170, 220);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Logs,
    Info,
    Config,
}

const TABS: [Tab; 3] = [Tab::Logs, Tab::Info, Tab::Config];

/// The left-hand panel that has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    /// Every worker; ↑↓ pick one.
    Workers,
    /// What is running and what runs next; ↑↓ pick one, Enter jumps to it.
    Upcoming,
    /// The daemon; the right side shows its own log.
    Daemon,
}

const FOCUS_ORDER: [Focus; 3] = [Focus::Workers, Focus::Upcoming, Focus::Daemon];

impl Tab {
    fn title(self) -> &'static str {
        match self {
            Tab::Logs => "Logs",
            Tab::Info => "Info",
            Tab::Config => "Config",
        }
    }
}

pub struct App {
    paths: WorkerPaths,
    workers: Vec<WorkerStatus>,
    daemon: Option<DaemonInfo>,
    /// Why the daemon could not be reached, if it couldn't.
    offline: Option<String>,
    list: ListState,
    focus: Focus,
    /// Selection inside "Running & next".
    upcoming: ListState,
    tab: Tab,
    /// Lines scrolled up from the end of the log (0 = following).
    scroll: usize,
    log: Vec<String>,
    flash: Option<(String, bool, Instant)>,
    quit: bool,
}

/// Opens the worker manager and blocks until it is closed.
pub fn run(paths: WorkerPaths) -> Result<()> {
    let mut app = App::new(paths);
    app.refresh();
    ratatui::run(|terminal| app.event_loop(terminal))
}

impl App {
    pub fn new(paths: WorkerPaths) -> Self {
        Self {
            paths,
            workers: Vec::new(),
            daemon: None,
            offline: None,
            list: ListState::default().with_selected(Some(0)),
            focus: Focus::Workers,
            upcoming: ListState::default(),
            tab: Tab::Logs,
            scroll: 0,
            log: Vec::new(),
            flash: None,
            quit: false,
        }
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let mut last_refresh = Instant::now();
        while !self.quit {
            terminal.draw(|frame| self.render(frame))?;
            if event::poll(REFRESH.saturating_sub(last_refresh.elapsed()))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.on_key(key.code);
            }
            if last_refresh.elapsed() >= REFRESH {
                self.refresh();
                last_refresh = Instant::now();
            }
        }
        Ok(())
    }

    fn selected(&self) -> Option<&WorkerStatus> {
        self.list.selected().and_then(|i| self.workers.get(i))
    }

    /// Pulls the status from the daemon and re-reads the selected log.
    pub fn refresh(&mut self) {
        match request(&self.paths.socket, &Request::Status) {
            Ok(response) => {
                self.workers = response.workers;
                self.daemon = response.daemon;
                self.offline = None;
            }
            Err(e) => {
                self.daemon = None;
                self.offline = Some(e.to_string());
            }
        }
        let count = self.workers.len();
        match self.list.selected() {
            _ if count == 0 => self.list.select(None),
            Some(i) if i >= count => self.list.select(Some(count - 1)),
            None => self.list.select(Some(0)),
            _ => {}
        }
        if self.focus == Focus::Upcoming {
            self.sync_upcoming();
        }
        self.reload_log();
    }

    /// Reads the log the right side shows: the selected worker's, or the
    /// daemon's own while the daemon panel has focus.
    fn reload_log(&mut self) {
        self.log = match self.focus {
            Focus::Daemon => tail_lines(&log_file("worker-daemon.log")),
            _ => self.selected().map(|w| tail_lines(Path::new(&w.log_file))).unwrap_or_default(),
        };
    }

    /// Workers in "Running & next" order (indexes into `workers`): running
    /// ones by start time, then the rest by next run.
    fn upcoming_order(&self) -> Vec<usize> {
        let mut running: Vec<usize> =
            (0..self.workers.len()).filter(|&i| self.workers[i].state == WorkerState::Running).collect();
        running.sort_by_key(|&i| self.workers[i].last_start);
        let mut next: Vec<usize> = (0..self.workers.len())
            .filter(|&i| self.workers[i].state != WorkerState::Running && self.workers[i].next_run.is_some())
            .collect();
        next.sort_by_key(|&i| self.workers[i].next_run);
        running.extend(next);
        running
    }

    /// Keeps the "Running & next" selection on the selected worker while the
    /// list reorders; if that worker left the list, stays at the same row.
    fn sync_upcoming(&mut self) {
        let order = self.upcoming_order();
        if order.is_empty() {
            self.upcoming.select(None);
            return;
        }
        let row = match self.list.selected().and_then(|w| order.iter().position(|&i| i == w)) {
            Some(row) => row,
            None => self.upcoming.selected().unwrap_or(0).min(order.len() - 1),
        };
        self.upcoming.select(Some(row));
        self.list.select(Some(order[row]));
    }

    fn set_focus(&mut self, focus: Focus) {
        if self.focus == focus {
            return;
        }
        self.focus = focus;
        self.scroll = 0;
        if focus == Focus::Upcoming {
            self.sync_upcoming();
        }
        self.reload_log();
    }

    fn cycle_focus(&mut self, delta: isize) {
        let i = FOCUS_ORDER.iter().position(|f| *f == self.focus).unwrap_or(0) as isize;
        self.set_focus(FOCUS_ORDER[(i + delta).rem_euclid(FOCUS_ORDER.len() as isize) as usize]);
    }

    /// ↑↓ in whichever panel has focus.
    fn move_selection(&mut self, delta: isize) {
        match self.focus {
            Focus::Workers => self.select(delta),
            Focus::Upcoming => {
                let order = self.upcoming_order();
                if order.is_empty() {
                    return;
                }
                let last = order.len() as isize - 1;
                let row = self.upcoming.selected().map_or(0, |r| (r as isize + delta).clamp(0, last)) as usize;
                self.upcoming.select(Some(row));
                self.list.select(Some(order[row]));
                self.scroll = 0;
                self.reload_log();
            }
            // The daemon has nothing to pick: ↑↓ scroll its log.
            Focus::Daemon => {
                self.scroll = if delta < 0 {
                    (self.scroll + 1).min(self.log.len())
                } else {
                    self.scroll.saturating_sub(1)
                };
            }
        }
    }

    fn flash(&mut self, text: String, ok: bool) {
        self.flash = Some((text, ok, Instant::now()));
    }

    fn send(&mut self, request_for: impl FnOnce(String) -> Request) {
        let Some(name) = self.selected().map(|w| w.name.clone()) else { return };
        self.command(request_for(name));
    }

    fn command(&mut self, req: Request) {
        match request(&self.paths.socket, &req) {
            Ok(r) => self.flash(r.message.unwrap_or_else(|| "done".into()), true),
            Err(e) => self.flash(e.to_string(), false),
        }
        self.refresh();
    }

    fn select(&mut self, delta: isize) {
        if self.workers.is_empty() {
            return;
        }
        let last = self.workers.len() as isize - 1;
        let i = self.list.selected().map_or(0, |i| (i as isize + delta).clamp(0, last));
        self.list.select(Some(i as usize));
        self.scroll = 0;
        self.reload_log();
    }

    fn switch_tab(&mut self, delta: isize) {
        let i = TABS.iter().position(|t| *t == self.tab).unwrap_or(0) as isize;
        self.tab = TABS[(i + delta).rem_euclid(TABS.len() as isize) as usize];
    }

    pub fn on_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Tab => self.cycle_focus(1),
            KeyCode::BackTab => self.cycle_focus(-1),
            KeyCode::Char('1') => self.set_focus(Focus::Workers),
            KeyCode::Char('2') => self.set_focus(Focus::Upcoming),
            KeyCode::Char('3') => self.set_focus(Focus::Daemon),
            // Enter on "Running & next" jumps to that worker in the full list.
            KeyCode::Enter if self.focus == Focus::Upcoming => self.set_focus(Focus::Workers),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(']') => self.switch_tab(1),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('[') => self.switch_tab(-1),
            KeyCode::PageUp => self.scroll = (self.scroll + 10).min(self.log.len()),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Home | KeyCode::Char('g') => self.scroll = self.log.len(),
            KeyCode::End | KeyCode::Char('G') => self.scroll = 0,
            // Worker actions don't apply while the daemon panel has focus.
            KeyCode::Char('r' | 's' | 'R') if self.focus == Focus::Daemon => {
                self.flash("select a worker first (Tab or 1)".into(), false);
            }
            KeyCode::Char('r') => self.send(|name| Request::Run { name }),
            KeyCode::Char('s') => {
                let paused = self
                    .selected()
                    .is_some_and(|w| matches!(w.state, WorkerState::Stopped | WorkerState::Failed));
                if paused {
                    self.send(|name| Request::Start { name });
                } else {
                    self.send(|name| Request::Stop { name });
                }
            }
            KeyCode::Char('R') => self.send(|name| Request::Restart { name }),
            KeyCode::Char('L') => self.command(Request::Reload),
            KeyCode::Char('D') => {
                if self.daemon.is_some() {
                    self.flash("the daemon is already running".into(), true);
                } else {
                    match spawn_daemon() {
                        Ok(()) => {
                            std::thread::sleep(Duration::from_millis(300));
                            self.refresh();
                            self.flash("daemon started".into(), true);
                        }
                        Err(e) => self.flash(format!("could not start the daemon: {e}"), false),
                    }
                }
            }
            KeyCode::Char('X') => self.command(Request::Shutdown),
            _ => {}
        }
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let [main, footer] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
        let [left, right] = Layout::horizontal([Constraint::Percentage(38), Constraint::Fill(1)]).areas(main);
        let [workers, upcoming, daemon] =
            Layout::vertical([Constraint::Fill(3), Constraint::Fill(2), Constraint::Length(7)]).areas(left);

        self.render_workers(frame, workers);
        self.render_upcoming(frame, upcoming);
        self.render_daemon(frame, daemon);
        self.render_main(frame, right);
        self.render_footer(frame, footer);
    }

    fn render_workers(&mut self, frame: &mut Frame, area: Rect) {
        let width = self.workers.iter().map(|w| w.name.len()).max().unwrap_or(0).min(24);
        let items: Vec<ListItem> = self
            .workers
            .iter()
            .map(|w| {
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:<10}", w.state.label()), state_style(w.state)),
                    Span::raw(format!("{:<width$}  ", w.name)),
                    Span::styled(w.schedule.clone(), Style::new().fg(DIM)),
                ]))
            })
            .collect();
        let title = format!("[1]─Workers ({})", self.workers.len());
        let list = List::new(items)
            .block(panel(&title, self.focus == Focus::Workers))
            .highlight_style(selection_style(self.focus != Focus::Daemon));
        frame.render_stateful_widget(list, area, &mut self.list);
        if self.workers.is_empty() {
            let hint = if self.daemon.is_some() {
                "no workers yet: run `aria worker init`, or add a .json file to the worker directory and press L"
            } else {
                ""
            };
            frame.render_widget(Paragraph::new(hint).fg(DIM).wrap(Default::default()), inner(area));
        }
    }

    fn render_upcoming(&mut self, frame: &mut Frame, area: Rect) {
        let now = Local::now().timestamp();
        let items: Vec<ListItem> = self
            .upcoming_order()
            .into_iter()
            .map(|i| {
                let w = &self.workers[i];
                let when = if w.state == WorkerState::Running {
                    let up = w.last_start.map_or(String::new(), |t| human_duration((now - t).max(0) as u64));
                    Span::styled(format!("{:<12}", format!("up {up}")), state_style(WorkerState::Running))
                } else {
                    let label = if w.state == WorkerState::Backoff { "retry " } else { "" };
                    Span::styled(
                        format!("{:<12}", format!("{label}{}", relative(w.next_run.unwrap_or(now), now))),
                        Style::new().fg(Color::Cyan),
                    )
                };
                ListItem::new(Line::from(vec![when, Span::raw(w.name.clone())]))
            })
            .collect();
        let focused = self.focus == Focus::Upcoming;
        let list = List::new(items)
            .block(panel("[2]─Running & next", focused))
            .highlight_style(selection_style(focused));
        // Only show a selection here while this panel has focus.
        let mut state = if focused { self.upcoming } else { ListState::default() };
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn render_daemon(&self, frame: &mut Frame, area: Rect) {
        let lines: Vec<Line> = match &self.daemon {
            Some(d) => {
                let now = Local::now().timestamp();
                let count = |s: WorkerState| self.workers.iter().filter(|w| w.state == s).count();
                vec![
                    Line::from(vec![
                        Span::styled("● online ", Style::new().fg(Color::Green).bold()),
                        Span::raw(format!("pid {} · up {}", d.pid, human_duration((now - d.started_at).max(0) as u64))),
                    ]),
                    Line::from(format!(
                        "running {} · scheduled {} · failed {}",
                        count(WorkerState::Running),
                        count(WorkerState::Scheduled),
                        count(WorkerState::Failed) + count(WorkerState::Invalid)
                    )),
                    Line::from(format!("parallel runs: up to {}", d.max_parallel)).fg(DIM),
                    Line::from(d.config_dir.clone()).fg(DIM),
                    Line::from(d.socket.clone()).fg(DIM),
                ]
            }
            None => vec![
                Line::from(Span::styled("● offline", Style::new().fg(Color::Red).bold())),
                Line::from("the worker daemon is not running"),
                Line::from(vec![Span::raw("press "), Span::styled("D", Style::new().fg(ACCENT).bold()), Span::raw(" to start it")]),
            ],
        };
        frame.render_widget(Paragraph::new(lines).block(panel("[3]─Daemon", self.focus == Focus::Daemon)), area);
    }

    /// The last lines of `self.log` that fit, `scroll` lines up from the end.
    fn log_view(&self, height: usize, empty: &str) -> Text<'static> {
        let end = self.log.len().saturating_sub(self.scroll);
        let start = end.saturating_sub(height);
        let lines: Vec<Line> = self.log[start..end].iter().map(|l| log_line(l)).collect();
        if lines.is_empty() { Text::from(empty.to_owned()).fg(DIM) } else { Text::from(lines) }
    }

    fn render_main(&self, frame: &mut Frame, area: Rect) {
        if self.focus == Focus::Daemon {
            let mut block = Block::bordered()
                .border_type(BorderType::Rounded)
                .title(Line::from(Span::styled("─Daemon log", Style::new().fg(ACCENT).bold())));
            if self.scroll > 0 {
                block = block.title_bottom(Line::from(format!(" ↑ {} lines up · End to follow ", self.scroll)).fg(ACCENT));
            }
            let body = block.inner(area);
            frame.render_widget(block, area);
            frame.render_widget(Paragraph::new(self.log_view(body.height as usize, "the daemon has not logged anything yet")), body);
            return;
        }
        let mut title = vec![Span::raw("─")];
        for (i, tab) in TABS.iter().enumerate() {
            if i > 0 {
                title.push(Span::raw(" - "));
            }
            let style = if *tab == self.tab { Style::new().fg(ACCENT).bold() } else { Style::new() };
            title.push(Span::styled(tab.title(), style));
        }
        let mut block = Block::bordered().border_type(BorderType::Rounded).title(Line::from(title));
        if self.tab == Tab::Logs && self.scroll > 0 {
            block = block.title_bottom(Line::from(format!(" ↑ {} lines up · End to follow ", self.scroll)).fg(ACCENT));
        }
        let body_area = block.inner(area);
        frame.render_widget(block, area);

        let Some(worker) = self.selected() else {
            let text = match &self.offline {
                Some(e) => format!("{e}\n\nPress D to start the worker daemon."),
                None => "Select a worker.".into(),
            };
            frame.render_widget(Paragraph::new(text).wrap(Default::default()), body_area);
            return;
        };

        match self.tab {
            Tab::Logs => {
                frame.render_widget(Paragraph::new(self.log_view(body_area.height as usize, "no output yet")), body_area);
            }
            Tab::Info => frame.render_widget(Paragraph::new(info_lines(worker)).wrap(Default::default()), body_area),
            Tab::Config => {
                let json = serde_json::to_string_pretty(&worker.spec).unwrap_or_default();
                let mut lines = vec![Line::from(worker.source.clone()).fg(DIM), Line::default()];
                lines.extend(json.lines().map(|l| Line::from(l.to_owned())));
                frame.render_widget(Paragraph::new(lines), body_area);
            }
        }
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let help = match self.focus {
            Focus::Workers => "q: quit, Tab: next panel, ↑↓: select, r: run, s: stop/start, R: restart, ←→: tabs, PgUp/PgDn: scroll, L: reload, D/X: daemon on/off",
            Focus::Upcoming => "q: quit, Tab: next panel, ↑↓: select, Enter: go to worker, r: run, s: stop/start, R: restart, ←→: tabs, L: reload",
            Focus::Daemon => "q: quit, Tab: next panel, ↑↓/PgUp/PgDn: scroll log, D: start daemon, X: stop daemon, L: reload workers",
        };
        let right = match &self.flash {
            Some((text, ok, at)) if at.elapsed() < Duration::from_secs(4) => {
                Span::styled(text.clone(), Style::new().fg(if *ok { Color::Green } else { Color::Red }))
            }
            _ => Span::styled(format!("A.R.I.A {}", env!("CARGO_PKG_VERSION")), Style::new().fg(ACCENT)),
        };
        let right_width = right.width() as u16 + 1;
        let [left_area, right_area] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(right_width)]).areas(area);
        frame.render_widget(Paragraph::new(help).fg(HINT), left_area);
        frame.render_widget(Paragraph::new(right).right_aligned(), right_area);
    }
}

fn panel(title: &str, focused: bool) -> Block<'static> {
    let style = if focused { Style::new().fg(ACCENT) } else { Style::new() };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(style)
        .title(Span::styled(title.to_owned(), style))
}

/// The selected row: bright in the focused panel, muted elsewhere.
fn selection_style(focused: bool) -> Style {
    if focused {
        Style::new().bg(Color::Rgb(60, 80, 120)).add_modifier(Modifier::BOLD)
    } else {
        Style::new().bg(Color::Rgb(45, 50, 62))
    }
}

fn inner(area: Rect) -> Rect {
    Block::bordered().inner(area)
}

fn state_style(state: WorkerState) -> Style {
    let color = match state {
        WorkerState::Running => Color::Green,
        WorkerState::Scheduled => Color::Cyan,
        WorkerState::Backoff => Color::Yellow,
        WorkerState::Failed | WorkerState::Invalid => Color::Red,
        WorkerState::Idle | WorkerState::Stopped | WorkerState::Disabled => DIM,
    };
    Style::new().fg(color)
}

/// The daemon's own markers (`── run #3 ──`) stand out from worker output.
fn log_line(line: &str) -> Line<'static> {
    if line.starts_with("──") {
        Line::from(line.to_owned()).fg(ACCENT)
    } else {
        Line::from(line.to_owned())
    }
}

fn info_lines(w: &WorkerStatus) -> Vec<Line<'static>> {
    let now = Local::now().timestamp();
    let when = |ts: Option<i64>| match ts.and_then(|t| Local.timestamp_opt(t, 0).single()) {
        Some(t) => format!("{} ({})", t.format("%Y-%m-%d %H:%M:%S"), relative(t.timestamp(), now)),
        None => "—".into(),
    };
    let row = |key: &str, value: String| {
        Line::from(vec![Span::styled(format!("{key:<13}"), Style::new().fg(DIM)), Span::raw(value)])
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(w.name.clone(), Style::new().bold()),
            Span::raw("  "),
            Span::styled(w.state.label(), state_style(w.state)),
        ]),
    ];
    if !w.description.is_empty() {
        lines.push(Line::from(w.description.clone()).fg(DIM));
    }
    lines.push(Line::default());
    if let Some(note) = &w.note {
        lines.push(row("note", note.clone()).fg(Color::Yellow));
    }
    if let Some(error) = &w.error {
        lines.push(row("error", error.clone()).fg(Color::Red));
    }
    lines.extend([
        row("command", w.command.clone()),
        row("schedule", w.schedule.clone()),
        row("pid", w.pid.map_or("—".into(), |p| p.to_string())),
        row("next run", when(w.next_run)),
        row("last start", when(w.last_start)),
        row("last end", when(w.last_end)),
        row("last exit", w.last_exit.map_or("—".into(), |c| c.to_string())),
        row("last took", w.last_duration_ms.map_or("—".into(), |ms| format!("{:.1}s", ms as f64 / 1000.0))),
        row("runs", w.runs.to_string()),
        row("failures", w.failures.to_string()),
        row("restarts", w.restarts.to_string()),
        row("skipped", w.skipped.to_string()),
        row("log", w.log_file.clone()),
        row("defined in", w.source.clone()),
    ]);
    lines
}

/// The last lines of a log, read from its end only, with terminal escape
/// sequences and control characters removed so worker output can't garble
/// the screen.
pub fn tail_lines(path: &Path) -> Vec<String> {
    let Ok(mut file) = File::open(path) else { return Vec::new() };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines();
    if start > 0 {
        lines.next(); // probably cut mid-line
    }
    lines.map(clean_line).collect()
}

/// Removes ANSI escape sequences and control characters; tabs become spaces.
pub fn clean_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.peek() {
                // CSI: ESC [ params final-byte
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: ESC ] ... BEL or ESC \
                Some(']') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            },
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Starts `aria worker daemon` in the background, in its own session so it
/// outlives the terminal.
pub fn spawn_daemon() -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["worker", "daemon"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe and is all that runs before exec.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command.spawn().map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn escape_sequences_are_stripped() {
        assert_eq!(clean_line("\x1b[1;31merror\x1b[0m done"), "error done");
        assert_eq!(clean_line("a\tb\rc\x07"), "a    bc");
        assert_eq!(clean_line("\x1b]0;title\x07text"), "text");
    }

    #[test]
    fn tail_reads_only_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.log");
        let body: String = (0..20_000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, body).unwrap();
        let lines = tail_lines(&path);
        assert_eq!(lines.last().unwrap(), "line 19999");
        assert!(lines.len() < 20_000);
        assert!(lines[0].starts_with("line "));
        assert!(tail_lines(&dir.path().join("missing.log")).is_empty());
    }

    fn screen(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
        terminal.draw(|f| app.render(f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_offline_and_with_workers() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(WorkerPaths::under(dir.path()));
        app.refresh();
        let offline = screen(&mut app);
        assert!(offline.contains("[1]─Workers (0)"));
        assert!(offline.contains("offline"));
        assert!(offline.contains("q: quit"));

        app.daemon = Some(DaemonInfo { pid: 42, started_at: Local::now().timestamp(), ..Default::default() });
        app.offline = None;
        app.workers = vec![
            WorkerStatus {
                name: "heartbeat".into(),
                schedule: "every 30s".into(),
                state: WorkerState::Scheduled,
                next_run: Some(Local::now().timestamp() + 20),
                ..Default::default()
            },
            WorkerStatus { name: "ticker".into(), schedule: "daemon".into(), state: WorkerState::Running, ..Default::default() },
        ];
        app.list.select(Some(1));
        app.log = vec!["── run #1 ──".into(), "tick".into()];
        let online = screen(&mut app);
        assert!(online.contains("scheduled heartbeat"), "{online}");
        assert!(online.contains("running   ticker"));
        assert!(online.contains("online pid 42"));
        assert!(online.contains("Logs - Info - Config"));
        assert!(online.contains("tick"));

        // One key press moves the selection exactly one row, and stops at the ends.
        app.on_key(KeyCode::Up);
        assert_eq!(app.list.selected(), Some(0));
        app.on_key(KeyCode::Up);
        assert_eq!(app.list.selected(), Some(0));
        app.on_key(KeyCode::Char('j'));
        assert_eq!(app.list.selected(), Some(1));
        app.on_key(KeyCode::Down);
        assert_eq!(app.list.selected(), Some(1));

        app.on_key(KeyCode::Right);
        assert!(screen(&mut app).contains("ticker  running"));
        app.on_key(KeyCode::Char('q'));
        assert!(app.quit);
    }

    #[test]
    fn tab_moves_between_panels() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(WorkerPaths::under(dir.path()));
        let now = Local::now().timestamp();
        app.daemon = Some(DaemonInfo { pid: 7, started_at: now, ..Default::default() });
        app.workers = vec![
            WorkerStatus { name: "a-later".into(), state: WorkerState::Scheduled, next_run: Some(now + 300), ..Default::default() },
            WorkerStatus { name: "b-manual".into(), state: WorkerState::Idle, ..Default::default() },
            WorkerStatus { name: "c-running".into(), state: WorkerState::Running, last_start: Some(now - 5), ..Default::default() },
            WorkerStatus { name: "d-soon".into(), state: WorkerState::Scheduled, next_run: Some(now + 10), ..Default::default() },
        ];
        app.list.select(Some(1));
        assert_eq!(app.focus, Focus::Workers);

        // Tab: "Running & next". Running first, then by next run; the manual
        // worker isn't listed, so the selection moves to the first row.
        app.on_key(KeyCode::Tab);
        assert_eq!(app.focus, Focus::Upcoming);
        assert_eq!(app.upcoming_order(), [2, 3, 0]);
        assert_eq!(app.upcoming.selected(), Some(0));
        assert_eq!(app.selected().unwrap().name, "c-running");
        app.on_key(KeyCode::Down);
        assert_eq!(app.selected().unwrap().name, "d-soon");
        app.on_key(KeyCode::Down);
        app.on_key(KeyCode::Down);
        assert_eq!(app.selected().unwrap().name, "a-later", "stops at the last row");
        app.tab = Tab::Info;
        let text = screen(&mut app);
        assert!(text.contains("a-later  scheduled"), "the right side follows the selection:\n{text}");

        // Tab: the daemon panel, whose log replaces the worker view.
        app.on_key(KeyCode::Tab);
        assert_eq!(app.focus, Focus::Daemon);
        let text = screen(&mut app);
        assert!(text.contains("Daemon log"), "{text}");
        assert!(text.contains("X: stop daemon"), "{text}");
        app.on_key(KeyCode::Char('r'));
        assert!(!app.flash.as_ref().unwrap().1, "worker actions are refused here");

        // Tab wraps around; Shift+Tab goes back.
        app.on_key(KeyCode::Tab);
        assert_eq!(app.focus, Focus::Workers);
        assert_eq!(app.selected().unwrap().name, "a-later", "the pick from the other panel stays");
        app.on_key(KeyCode::BackTab);
        assert_eq!(app.focus, Focus::Daemon);

        // Number keys jump; Enter in "Running & next" goes to the worker list.
        app.on_key(KeyCode::Char('2'));
        assert_eq!(app.focus, Focus::Upcoming);
        app.on_key(KeyCode::Up);
        app.on_key(KeyCode::Enter);
        assert_eq!(app.focus, Focus::Workers);
        assert_eq!(app.selected().unwrap().name, "d-soon");
        app.on_key(KeyCode::Up);
        assert_eq!(app.selected().unwrap().name, "c-running", "↑↓ now move through every worker");

        // ←→ still switch the right-hand tabs.
        app.on_key(KeyCode::Right);
        assert_eq!(app.tab, Tab::Config);
    }
}
