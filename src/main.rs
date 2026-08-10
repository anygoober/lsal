use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use crossterm::cursor;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Cell, Paragraph, Row, StatefulWidget, Table, TableState, Widget},
};

// Animation tuning: total duration ~= ANIM_STEPS * ANIM_FRAME_MS.
// Offset per frame follows a cubic ease-out curve (fast start, slow settle),
// not a linear one, so more steps here buys smoother deceleration rather
// than a longer animation.
const ANIM_STEPS: u32 = 10;
const ANIM_FRAME_MS: u64 = 10;

/// Cubic ease-out: steep slope near t=0 (fast), flattening toward t=1 (slow).
fn ease_out_cubic(t: f64) -> f64 {
    let inv = 1.0 - t;
    1.0 - inv * inv * inv
}

struct Entry {
    name: String,
    is_dir: bool,
    mode_str: String,
    nlink: u64,
    uid: u32,
    gid: u32,
    size: u64,
    mtime: String,
}

struct App {
    cwd: PathBuf,
    entries: Vec<Entry>,
    show_hidden: bool,
    state: TableState,
}

impl App {
    fn new(start: PathBuf) -> io::Result<Self> {
        let mut app = App {
            cwd: start,
            entries: Vec::new(),
            show_hidden: true,
            state: TableState::default(),
        };
        app.reload()?;
        app.state.select(Some(0));
        Ok(app)
    }

    fn reload(&mut self) -> io::Result<()> {
        let mut entries = Vec::new();

        for e in fs::read_dir(&self.cwd)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().to_string();
            if !self.show_hidden && name.starts_with('.') {
                continue;
            }
            let meta = e.metadata()?;
            entries.push(Entry {
                name,
                is_dir: meta.is_dir(),
                mode_str: format_mode(meta.permissions().mode(), meta.is_dir()),
                nlink: meta.nlink(),
                uid: meta.uid(),
                gid: meta.gid(),
                size: meta.len(),
                mtime: format_time(meta.mtime()),
            });
        }

        // dirs first, then alphabetical
        entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });

        self.entries = entries;
        if self.state.selected().unwrap_or(0) >= self.entries.len() {
            self.state
                .select(Some(self.entries.len().saturating_sub(1)));
        }
        Ok(())
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.state.selected().and_then(|i| self.entries.get(i))
    }

    fn move_down(&mut self) {
        let len = self.entries.len();
        if len == 0 {
            return;
        }
        let next = match self.state.selected() {
            Some(i) if i + 1 < len => i + 1,
            Some(_) => len - 1,
            None => 0,
        };
        self.state.select(Some(next));
    }

    fn move_up(&mut self) {
        let next = match self.state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.state.select(Some(next));
    }

    fn enter_selected(&mut self) -> io::Result<()> {
        if let Some(entry) = self.selected_entry() {
            if entry.is_dir {
                self.cwd.push(entry.name.clone());
                self.reload()?;
                self.state.select(Some(0));
            }
        }
        Ok(())
    }

    fn go_up(&mut self) -> io::Result<()> {
        if self.cwd.pop() {
            self.reload()?;
            self.state.select(Some(0));
        }
        Ok(())
    }
}

fn format_mode(mode: u32, is_dir: bool) -> String {
    let ftype = if is_dir { 'd' } else { '-' };
    let bits = [
        (mode & 0o400, 'r'),
        (mode & 0o200, 'w'),
        (mode & 0o100, 'x'),
        (mode & 0o040, 'r'),
        (mode & 0o020, 'w'),
        (mode & 0o010, 'x'),
        (mode & 0o004, 'r'),
        (mode & 0o002, 'w'),
        (mode & 0o001, 'x'),
    ];
    let mut s = String::with_capacity(10);
    s.push(ftype);
    for (bit, c) in bits {
        s.push(if bit != 0 { c } else { '-' });
    }
    s
}

fn format_time(secs: i64) -> String {
    // Minimal, dependency-free timestamp formatting (UTC, seconds since epoch).
    let dur = UNIX_EPOCH + Duration::from_secs(secs.max(0) as u64);
    // Rough calendar breakdown without pulling in chrono.
    let total_secs = dur.duration_since(UNIX_EPOCH).unwrap().as_secs();
    let days = total_secs / 86400;
    let secs_of_day = total_secs % 86400;
    let (h, m) = (secs_of_day / 3600, (secs_of_day % 3600) / 60);

    // Civil-from-days algorithm (Howard Hinnant), proleptic Gregorian.
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if mth <= 2 { y + 1 } else { y };

    format!("{year:04}-{mth:02}-{d:02} {h:02}:{m:02}")
}

fn format_size(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut sz = size as f64;
    let mut unit = 0;
    while sz >= 1024.0 && unit < UNITS.len() - 1 {
        sz /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size}{}", UNITS[unit])
    } else {
        format!("{sz:.1}{}", UNITS[unit])
    }
}

/// Suspend the TUI, drop into an interactive shell in the current directory,
/// then restore the TUI and refresh the listing once the shell exits.
fn open_shell<B: ratatui::backend::Backend + io::Write>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, cursor::Show)?;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    std::process::Command::new(shell)
        .current_dir(&app.cwd)
        .status()?;

    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen, cursor::Hide)?;
    terminal.clear().ok();
    app.reload()?;
    Ok(())
}

/// Render the whole UI directly into a plain `Buffer` (no live `Frame` needed).
/// Shared by normal drawing and by the animation snapshots below.
fn draw_ui(buf: &mut Buffer, area: Rect, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    let header = Paragraph::new(Line::from(app.cwd.to_string_lossy().to_string())).style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    header.render(chunks[0], buf);

    let rows: Vec<Row> = app
        .entries
        .iter()
        .map(|e| {
            let name = if e.is_dir {
                format!("{}/", e.name)
            } else {
                e.name.clone()
            };
            let style = if e.is_dir {
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(e.mode_str.clone()),
                Cell::from(e.nlink.to_string()),
                Cell::from(e.uid.to_string()),
                Cell::from(e.gid.to_string()),
                Cell::from(format_size(e.size)),
                Cell::from(e.mtime.clone()),
                Cell::from(name),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        Constraint::Length(10),
        Constraint::Length(4),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Length(16),
        Constraint::Min(10),
    ];

    let table = Table::new(rows, widths)
        .header(
            Row::new(vec![
                "Perms", "Lnk", "UID", "GID", "Size", "Modified", "Name",
            ])
            .style(Style::default().add_modifier(Modifier::UNDERLINED)),
        )
        .block(Block::default().borders(Borders::ALL).title(" ls -al "))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    StatefulWidget::render(table, chunks[1], buf, &mut app.state);

    let footer = Paragraph::new(Line::from(
        "↑/k ↓/j move   Enter open   Backspace up   . hidden   ! shell   r refresh   q quit",
    ))
    .style(Style::default().fg(Color::DarkGray));
    footer.render(chunks[2], buf);
}

fn draw(f: &mut ratatui::Frame, app: &mut App) {
    let area = f.area();
    draw_ui(f.buffer_mut(), area, app);
}

/// Render the current app state into an offscreen buffer, for use as the
/// "before" frame of a transition.
fn snapshot(app: &mut App, area: Rect) -> Buffer {
    let mut buf = Buffer::empty(area);
    draw_ui(&mut buf, area, app);
    buf
}

/// Animate a slide-to-the-left transition from `old_buf` to the app's
/// current (already-navigated) state, painting directly into the terminal's
/// cell buffer frame by frame.
fn slide_left_transition<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    old_buf: Buffer,
    area: Rect,
    app: &mut App,
) -> anyhow::Result<()> {
    let width = area.width as u32;
    if width == 0 || area.height == 0 {
        return Ok(());
    }

    let new_buf = {
        let mut b = Buffer::empty(area);
        draw_ui(&mut b, area, app);
        b
    };

    for step in 1..=ANIM_STEPS {
        let t = step as f64 / ANIM_STEPS as f64;
        let eased = ease_out_cubic(t);
        let offset = (width as f64 * eased).round() as u16;
        let offset = offset.min(area.width);
        terminal
            .draw(|f| {
                let buf = f.buffer_mut();
                for y in area.y..area.y + area.height {
                    for x in area.x..area.x + area.width {
                        let col = x - area.x;
                        let src = if (col as u32 + offset as u32) < width {
                            &old_buf[(area.x + col + offset, y)]
                        } else {
                            let nx = (col as u32 + offset as u32 - width) as u16;
                            &new_buf[(area.x + nx, y)]
                        };
                        buf[(x, y)] = src.clone();
                    }
                }
            })
            .ok();
        std::thread::sleep(Duration::from_millis(ANIM_FRAME_MS));
    }
    Ok(())
}

/// Enter the selected directory (if any) with a slide-left transition.
fn enter_with_animation<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> anyhow::Result<()> {
    let is_dir = app.selected_entry().map(|e| e.is_dir).unwrap_or(false);
    if !is_dir {
        return Ok(());
    }

    let size = terminal.size().unwrap();
    let area = Rect::new(0, 0, size.width, size.height);

    let old_buf = snapshot(app, area);
    app.enter_selected()?;
    slide_left_transition(terminal, old_buf, area, app)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let start = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let start = fs::canonicalize(start)?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(start)?;
    let result = run(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run<B: ratatui::backend::Backend + io::Write>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|f| draw(f, app)).ok();

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                    KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                    KeyCode::Enter => enter_with_animation(terminal, app)?,
                    KeyCode::Backspace => app.go_up()?,
                    KeyCode::Char('r') => app.reload()?,
                    KeyCode::Char('.') => {
                        app.show_hidden = !app.show_hidden;
                        app.reload()?;
                    }
                    KeyCode::Char('!') => open_shell(terminal, app)?,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
