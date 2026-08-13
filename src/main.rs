// THIS FILE IS AI-GENERATED WITH VERY LITTLE HELP FROM A HUMAN.
//
// PLEASE DO NOT TAKE THIS SERIOUSLY!

use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use crossterm::cursor;
use crossterm::event::KeyModifiers;
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

/// What kind of operation is currently sitting in the clipboard.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ClipOp {
    Copy,
    Move,
}

/// An action awaiting explicit user confirmation before it runs.
enum PendingConfirm {
    Delete(Vec<PathBuf>),
}

/// Recursively copy `src` into `dst`. Works for both files and directories.
fn copy_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    if src.is_dir() {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let dest_path = dst.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_recursive(&entry.path(), &dest_path)?;
            } else {
                fs::copy(entry.path(), &dest_path)?;
            }
        }
        Ok(())
    } else {
        fs::copy(src, dst).map(|_| ())
    }
}

/// Move `src` to `dst`. Tries a fast rename first (same filesystem); falls
/// back to copy-then-remove for cross-device moves.
fn move_path(src: &Path, dst: &Path) -> io::Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => {
            copy_recursive(src, dst)?;
            remove_path(src)
        }
    }
}

/// Remove a file or a directory (recursively).
fn remove_path(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
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
    /// Full paths of entries the user has explicitly marked (persists across
    /// directory navigation, unlike the single "selected" cursor row).
    marked: HashSet<PathBuf>,
    /// Paths staged for a copy or move, along with which operation to run.
    clipboard: Option<(ClipOp, Vec<PathBuf>)>,
    /// A destructive action waiting on a y/n confirmation.
    confirm: Option<PendingConfirm>,
    /// Last operation's result, shown in the status line until replaced.
    status: Option<String>,
}

impl App {
    fn new(start: PathBuf) -> io::Result<Self> {
        let mut app = App {
            cwd: start,
            entries: Vec::new(),
            show_hidden: true,
            state: TableState::default(),
            marked: HashSet::new(),
            clipboard: None,
            confirm: None,
            status: None,
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
        let child_name = self
            .cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_string());

        if self.cwd.pop() {
            self.reload()?;
            let idx = child_name
                .and_then(|name| self.entries.iter().position(|e| e.name == name))
                .unwrap_or(0);
            self.state.select(Some(idx));
        }
        Ok(())
    }

    fn toggle_mark(&mut self) {
        if let Some(e) = self.selected_entry() {
            let path = self.cwd.join(&e.name);
            if !self.marked.remove(&path) {
                self.marked.insert(path);
            }
        }
    }

    fn clear_marks(&mut self) {
        self.marked.clear();
        self.status = Some("Marks cleared".to_string());
    }

    /// The set an operation should act on: marked entries if any are
    /// marked, otherwise just the currently highlighted row.
    fn marked_or_selected(&self) -> Vec<PathBuf> {
        if !self.marked.is_empty() {
            let mut v: Vec<PathBuf> = self.marked.iter().cloned().collect();
            v.sort();
            v
        } else if let Some(e) = self.selected_entry() {
            vec![self.cwd.join(&e.name)]
        } else {
            Vec::new()
        }
    }

    fn copy_to_clipboard(&mut self) {
        let paths = self.marked_or_selected();
        if paths.is_empty() {
            self.status = Some("Nothing to copy".to_string());
            return;
        }
        let n = paths.len();
        self.clipboard = Some((ClipOp::Copy, paths));
        self.status = Some(format!("Copied {n} item(s) to clipboard — ctrl-C to paste"));
    }

    fn cut_to_clipboard(&mut self) {
        let paths = self.marked_or_selected();
        if paths.is_empty() {
            self.status = Some("Nothing to cut".to_string());
            return;
        }
        let n = paths.len();
        self.clipboard = Some((ClipOp::Move, paths));
        self.status = Some(format!("Cut {n} item(s) to clipboard — press p to paste"));
    }

    /// Paste whatever is in the clipboard into the current directory.
    fn paste(&mut self) -> io::Result<()> {
        let Some((op, paths)) = self.clipboard.clone() else {
            self.status = Some("Clipboard is empty".to_string());
            return Ok(());
        };

        let mut done = 0;
        let mut skipped = 0;
        let mut errors = 0;

        for src in &paths {
            let Some(name) = src.file_name() else {
                continue;
            };
            let dest = self.cwd.join(name);

            // Don't paste a thing onto itself, and don't clobber existing files.
            if &dest == src || dest.exists() {
                skipped += 1;
                continue;
            }

            let result = match op {
                ClipOp::Copy => copy_recursive(src, &dest),
                ClipOp::Move => move_path(src, &dest),
            };
            match result {
                Ok(()) => done += 1,
                Err(_) => errors += 1,
            }
        }

        if op == ClipOp::Move {
            // Moved items no longer exist at their old paths.
            self.marked.clear();
            self.clipboard = None;
        }

        let verb = match op {
            ClipOp::Copy => "Copied",
            ClipOp::Move => "Moved",
        };
        self.status = Some(format!(
            "{verb} {done}, skipped {skipped} (already exists), errors {errors}"
        ));
        self.reload()
    }

    /// Stage a delete for confirmation (marked entries, or the highlighted one).
    fn request_delete(&mut self) {
        let paths = self.marked_or_selected();
        if paths.is_empty() {
            self.status = Some("Nothing to delete".to_string());
            return;
        }
        self.confirm = Some(PendingConfirm::Delete(paths));
    }

    fn confirm_yes(&mut self) -> io::Result<()> {
        if let Some(PendingConfirm::Delete(paths)) = self.confirm.take() {
            let mut removed = 0;
            let mut errors = 0;
            for p in &paths {
                match remove_path(p) {
                    Ok(()) => {
                        removed += 1;
                        self.marked.remove(p);
                    }
                    Err(_) => errors += 1,
                }
            }
            self.status = Some(format!("Deleted {removed}, errors {errors}"));
            self.reload()?;
        }
        Ok(())
    }

    fn confirm_no(&mut self) {
        self.confirm = None;
        self.status = Some("Delete cancelled".to_string());
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
            let marked = app.marked.contains(&app.cwd.join(&e.name));
            let mark = if marked { "*" } else { " " };
            let name = if e.is_dir {
                format!("{mark}{}/", e.name)
            } else {
                format!("{mark}{}", e.name)
            };
            let style = if marked {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if e.is_dir {
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

    let (status_text, status_style) = if let Some(PendingConfirm::Delete(paths)) = &app.confirm {
        (
            format!(
                "Delete {} item(s)? This cannot be undone. (y/n)",
                paths.len()
            ),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )
    } else if let Some(status) = &app.status {
        (status.clone(), Style::default().fg(Color::Green))
    } else if let Some((op, paths)) = &app.clipboard {
        let verb = match op {
            ClipOp::Copy => "copy",
            ClipOp::Move => "move",
        };
        (
            format!(
                "Clipboard: {} item{s} staged to {verb}",
                paths.len(),
                s = if paths.len() == 1 { "" } else { "s" }
            ),
            Style::default().fg(Color::DarkGray),
        )
    } else if !app.marked.is_empty() {
        (
            format!(
                "selected {} item{s}",
                app.marked.len(),
                s = if app.marked.len() == 1 { "" } else { "s" }
            ),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (String::new(), Style::default())
    };
    Paragraph::new(Line::from(status_text))
        .style(status_style)
        .render(chunks[2], buf);

    let footer = Paragraph::new(Line::from("↑/k ↓/j move  q quit"))
        .style(Style::default().fg(Color::DarkGray));
    footer.render(chunks[3], buf);
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

// Animate a slide-to-the-right transition: the mirror image of
/// `slide_left_transition`, used when navigating back up a directory. The
/// old (child) view exits to the right while the new (parent) view enters
/// from the left.
fn slide_right_transition<B: ratatui::backend::Backend>(
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
        let offset = (width as f64 * eased).round() as u32;
        let offset = offset.min(width);
        terminal
            .draw(|f| {
                let buf = f.buffer_mut();
                for y in area.y..area.y + area.height {
                    for x in area.x..area.x + area.width {
                        let col = (x - area.x) as u32;
                        let src = if col < offset {
                            let nx = (width - offset + col) as u16;
                            &new_buf[(area.x + nx, y)]
                        } else {
                            let ox = (col - offset) as u16;
                            &old_buf[(area.x + ox, y)]
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

/// Go up to the parent directory (if any) with a slide-right transition.
fn go_up_with_animation<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> anyhow::Result<()> {
    if app.cwd.parent().is_none() {
        return Ok(());
    }

    let size = terminal.size().unwrap();
    let area = Rect::new(0, 0, size.width, size.height);

    let old_buf = snapshot(app, area);
    app.go_up()?;
    slide_right_transition(terminal, old_buf, area, app)?;
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

                // While a destructive action is pending, only y/n/esc are live.
                if app.confirm.is_some() {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => app.confirm_yes()?,
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => app.confirm_no(),
                        _ => {}
                    }
                    continue;
                }

                match (key.modifiers, key.code) {
                    (KeyModifiers::NONE, KeyCode::Char('q') | KeyCode::Esc) => break,
                    (KeyModifiers::NONE, KeyCode::Down | KeyCode::Char('j')) => app.move_down(),
                    (KeyModifiers::NONE, KeyCode::Up | KeyCode::Char('k')) => app.move_up(),
                    (KeyModifiers::NONE, KeyCode::Enter | KeyCode::Right) => {
                        enter_with_animation(terminal, app)?
                    }
                    (KeyModifiers::NONE, KeyCode::Backspace | KeyCode::Left) => {
                        go_up_with_animation(terminal, app)?
                    }
                    (KeyModifiers::NONE, KeyCode::Char('r')) => app.reload()?,
                    (KeyModifiers::NONE, KeyCode::Char('.')) => {
                        app.show_hidden = !app.show_hidden;
                        app.reload()?;
                    }
                    (KeyModifiers::NONE, KeyCode::Char('!')) => open_shell(terminal, app)?,

                    (KeyModifiers::NONE, KeyCode::Char(' ')) => app.toggle_mark(),
                    (KeyModifiers::NONE, KeyCode::Char('u')) => app.clear_marks(),

                    (KeyModifiers::SHIFT, KeyCode::Up | KeyCode::Char('k')) => {
                        app.toggle_mark();
                        app.move_up();
                    }

                    (KeyModifiers::SHIFT, KeyCode::Down | KeyCode::Char('j')) => {
                        app.toggle_mark();
                        app.move_down();
                    }

                    (KeyModifiers::CONTROL, KeyCode::Char('c')) => app.copy_to_clipboard(),
                    (KeyModifiers::CONTROL, KeyCode::Char('x')) => app.cut_to_clipboard(),
                    (KeyModifiers::CONTROL, KeyCode::Char('v')) => app.paste()?,

                    (KeyModifiers::NONE, KeyCode::Char('d')) => app.request_delete(),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
