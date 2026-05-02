use crate::app::{AppState, MsgKind, NetLevel, Theme};
use crate::util::{clip, color_idx, fmt_duration, fmt_time, truncate_pid};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Gauge, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::Duration;

pub type AppTerminal = Terminal<CrosstermBackend<Stdout>>;

pub struct InputState {
    pub buf: String,
    pub cursor: usize,
    pub scroll: usize,
}

impl InputState {
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            cursor: 0,
            scroll: 0,
        }
    }

    fn insert(&mut self, ch: char) {
        if self.buf.len() < crate::constants::MAX_TEXT_LEN - 1 {
            self.buf.insert(self.cursor, ch);
            self.cursor += ch.len_utf8();
        }
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.buf[..self.cursor]
            .char_indices()
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.buf.drain(prev..self.cursor);
        self.cursor = prev;
    }

    fn delete(&mut self) {
        if self.cursor >= self.buf.len() {
            return;
        }
        let next = self.buf[self.cursor..]
            .char_indices()
            .nth(1)
            .map(|(i, _)| self.cursor + i)
            .unwrap_or(self.buf.len());
        self.buf.drain(self.cursor..next);
    }

    fn take(&mut self) -> String {
        self.cursor = 0;
        self.scroll = 0;
        std::mem::take(&mut self.buf)
    }
}

pub struct TerminalGuard {
    terminal: AppTerminal,
}

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, Hide, Clear(ClearType::All))?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;
        Ok(Self { terminal })
    }

    pub fn terminal(&mut self) -> &mut AppTerminal {
        &mut self.terminal
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), Show, LeaveAlternateScreen);
    }
}

pub enum UiAction {
    None,
    Redraw,
    Submit(String),
    Quit,
}

pub fn poll_input(input: &mut InputState) -> io::Result<UiAction> {
    if !event::poll(Duration::from_millis(30))? {
        return Ok(UiAction::None);
    }
    let Event::Key(key) = event::read()? else {
        return Ok(UiAction::None);
    };
    if key.kind != KeyEventKind::Press {
        return Ok(UiAction::None);
    }
    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL)
        | (KeyCode::Char('d'), KeyModifiers::CONTROL) => Ok(UiAction::Quit),
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
            input.buf.clear();
            input.cursor = 0;
            Ok(UiAction::Redraw)
        }
        (KeyCode::Char(ch), _) => {
            input.insert(ch);
            Ok(UiAction::Redraw)
        }
        (KeyCode::Backspace, _) => {
            input.backspace();
            Ok(UiAction::Redraw)
        }
        (KeyCode::Delete, _) => {
            input.delete();
            Ok(UiAction::Redraw)
        }
        (KeyCode::Left, _) => {
            if input.cursor > 0 {
                input.cursor = input.buf[..input.cursor]
                    .char_indices()
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
            }
            Ok(UiAction::Redraw)
        }
        (KeyCode::Right, _) => {
            if input.cursor < input.buf.len() {
                input.cursor = input.buf[input.cursor..]
                    .char_indices()
                    .nth(1)
                    .map(|(i, _)| input.cursor + i)
                    .unwrap_or(input.buf.len());
            }
            Ok(UiAction::Redraw)
        }
        (KeyCode::Home, _) => {
            input.cursor = 0;
            Ok(UiAction::Redraw)
        }
        (KeyCode::End, _) => {
            input.cursor = input.buf.len();
            Ok(UiAction::Redraw)
        }
        (KeyCode::PageUp, _) => {
            input.scroll = input.scroll.saturating_add(1);
            Ok(UiAction::Redraw)
        }
        (KeyCode::PageDown, _) => {
            input.scroll = input.scroll.saturating_sub(1);
            Ok(UiAction::Redraw)
        }
        (KeyCode::Enter, _) => {
            let line = input.take();
            Ok(if line.is_empty() {
                UiAction::None
            } else {
                UiAction::Submit(line)
            })
        }
        _ => Ok(UiAction::None),
    }
}

pub fn render(
    terminal: &mut AppTerminal,
    app: &Arc<AppState>,
    input: &InputState,
) -> io::Result<()> {
    terminal.draw(|frame| draw_ui(frame, app, input))?;
    Ok(())
}

fn draw_ui(frame: &mut Frame<'_>, app: &Arc<AppState>, input: &InputState) {
    let area = frame.area();
    let theme = *app.theme.lock().unwrap();
    let sidebar_w = if area.width > 96 {
        28
    } else if area.width > 68 {
        23
    } else {
        0
    };
    let netlog_w = if area.width > 138 {
        42
    } else if area.width > 116 {
        34
    } else {
        0
    };

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(sidebar_w),
            Constraint::Min(24),
            Constraint::Length(netlog_w),
        ])
        .split(outer[1]);

    render_header(frame, app, theme, outer[0]);
    render_sidebar(frame, app, theme, body[0]);
    render_messages(frame, app, theme, body[1], input.scroll);
    render_netlog(frame, app, theme, body[2]);
    render_input(frame, app, theme, outer[2], input);
    render_status(frame, app, theme, outer[3]);

    let nick_len = app.nick().len() as u16;
    let cursor_x = outer[2].x + 2 + nick_len + 3 + input.cursor as u16;
    let cursor_y = outer[2].y + 1;
    frame.set_cursor_position((cursor_x.min(outer[2].right().saturating_sub(2)), cursor_y));
}

fn block<'a>(title: impl Into<Line<'a>>, theme: Theme) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border))
        .title(title)
        .style(Style::default().bg(theme.bg).fg(theme.fg))
}

fn render_header(frame: &mut Frame<'_>, app: &Arc<AppState>, theme: Theme, area: Rect) {
    let connected = app.connected_peers().len();
    let lan = app.lan_ip.lock().unwrap().clone();
    let port = app.listen_port.load(std::sync::atomic::Ordering::Relaxed);
    let (rx, tx) = collect_msg_stats(app);
    let status_color = if connected > 0 {
        theme.peers[2]
    } else {
        theme.dim
    };
    let line = Line::from(vec![
        Span::styled(
            " speer-chat ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("Noise", Style::default().fg(theme.peers[3])),
        Span::styled(" / ", Style::default().fg(theme.dim)),
        Span::styled("Yamux", Style::default().fg(theme.peers[1])),
        Span::styled(" / ", Style::default().fg(theme.dim)),
        Span::styled("mDNS+WAN", Style::default().fg(theme.peers[2])),
        Span::raw("   "),
        Span::styled(format!("{lan}:{port}"), Style::default().fg(theme.fg)),
        Span::raw("   "),
        Span::styled(format!("rx {rx} / tx {tx}"), Style::default().fg(theme.dim)),
        Span::raw("   "),
        Span::styled(
            if connected > 0 {
                format!("{connected} online")
            } else {
                "discovering".to_string()
            },
            Style::default().fg(status_color),
        ),
    ]);
    let p = Paragraph::new(line)
        .block(block("", theme).style(Style::default().bg(theme.panel)))
        .style(Style::default().bg(theme.panel));
    frame.render_widget(p, area);
}

fn render_sidebar(frame: &mut Frame<'_>, app: &Arc<AppState>, theme: Theme, area: Rect) {
    if area.width == 0 {
        return;
    }
    let mut lines = vec![
        Line::from(Span::styled(
            "local",
            Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("● {}", app.nick()),
            Style::default().fg(theme.peers[0]),
        )),
        Line::from(Span::styled(
            truncate_pid(&app.identity.peer_id),
            Style::default().fg(theme.dim),
        )),
        Line::raw(""),
        Line::from(vec![
            Span::styled("uptime ", Style::default().fg(theme.dim)),
            Span::styled(
                fmt_duration(app.started_at.elapsed()),
                Style::default().fg(theme.fg),
            ),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            "peers",
            Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
        )),
    ];
    let peers = app.connected_peers();
    if peers.is_empty() {
        lines.push(Line::from(Span::styled(
            "waiting for peers",
            Style::default().fg(theme.dim),
        )));
        lines.push(Line::from(Span::styled(
            "/connect <addr>",
            Style::default().fg(theme.accent),
        )));
    } else {
        for (i, peer) in peers.iter().enumerate() {
            let info = peer.info.lock().unwrap();
            let name = if info.remote_nick.is_empty() {
                &info.remote_pid_short
            } else {
                &info.remote_nick
            };
            lines.push(Line::from(Span::styled(
                format!("● {name}"),
                Style::default().fg(theme.peers[i % 6]),
            )));
            lines.push(Line::from(Span::styled(
                format!("  rx{} tx{}", info.msgs_rx, info.msgs_tx),
                Style::default().fg(theme.dim),
            )));
        }
    }
    let p = Paragraph::new(Text::from(lines))
        .block(
            block(
                Line::from(Span::styled(" Peers ", Style::default().fg(theme.accent))),
                theme,
            )
            .style(Style::default().bg(theme.panel)),
        )
        .wrap(Wrap { trim: true })
        .style(Style::default().bg(theme.panel).fg(theme.fg));
    frame.render_widget(p, area);
}

fn render_messages(
    frame: &mut Frame<'_>,
    app: &Arc<AppState>,
    theme: Theme,
    area: Rect,
    scroll: usize,
) {
    let history = app.history.lock().unwrap();
    if history.is_empty() {
        let welcome = Paragraph::new(Text::from(vec![
            Line::from(Span::styled(
                "speer-chat midnight",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "LAN peers appear automatically. Use /connect <addr> for wide-area peers.",
                Style::default().fg(theme.dim),
            )),
        ]))
        .block(block(
            Line::from(Span::styled(" Chat ", Style::default().fg(theme.accent))),
            theme,
        ))
        .wrap(Wrap { trim: true });
        frame.render_widget(welcome, area);
        return;
    }
    let max = area.height.saturating_sub(2) as usize;
    let end = history.len().saturating_sub(scroll);
    let start = end.saturating_sub(max);
    let mut items = Vec::new();
    for msg in history.iter().skip(start).take(max) {
        let ts = fmt_time(msg.timestamp);
        let (marker, name, color) = match msg.kind {
            MsgKind::Chat => {
                let name = if msg.nick.is_empty() {
                    "unknown"
                } else {
                    &msg.nick
                };
                ("▌", format!("{name:<12}"), theme.peers[msg.color_idx])
            }
            MsgKind::Join => ("+", "join        ".to_string(), theme.peers[msg.color_idx]),
            MsgKind::Leave => ("-", "leave       ".to_string(), theme.peers[4]),
            MsgKind::System => ("·", "system      ".to_string(), theme.dim),
            MsgKind::Error => ("!", "error       ".to_string(), theme.peers[0]),
        };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(format!("[{ts}] "), Style::default().fg(theme.timestamp)),
            Span::styled(
                marker,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                name,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                clip(&msg.text, area.width.saturating_sub(32) as usize),
                Style::default().fg(theme.fg),
            ),
        ])));
    }
    let list = List::new(items)
        .block(block(
            Line::from(Span::styled(" Chat ", Style::default().fg(theme.accent))),
            theme,
        ))
        .style(Style::default().fg(theme.fg).bg(theme.bg));
    frame.render_widget(list, area);
}

fn render_netlog(frame: &mut Frame<'_>, app: &Arc<AppState>, theme: Theme, area: Rect) {
    if area.width == 0 {
        return;
    }
    let top = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(4)])
        .split(area);
    let lan = app.lan_ip.lock().unwrap().clone();
    let port = app.listen_port.load(std::sync::atomic::Ordering::Relaxed);
    let meta = Paragraph::new(Text::from(vec![
        Line::from(Span::styled(
            "mesh  discovering",
            Style::default()
                .fg(theme.peers[2])
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("TCP   ", Style::default().fg(theme.dim)),
            Span::styled(format!("{lan}:{port}"), Style::default().fg(theme.fg)),
        ]),
        Line::from(vec![
            Span::styled("path  ", Style::default().fg(theme.dim)),
            Span::styled("Noise / Yamux / Chat", Style::default().fg(theme.peers[3])),
        ]),
    ]))
    .block(
        block(
            Line::from(Span::styled(" Network ", Style::default().fg(theme.accent))),
            theme,
        )
        .style(Style::default().bg(theme.panel)),
    )
    .style(Style::default().bg(theme.panel));
    frame.render_widget(meta, top[0]);

    let netlog = app.netlog.lock().unwrap();
    let max = top[1].height.saturating_sub(2) as usize;
    let start = netlog.len().saturating_sub(max);
    let mut items = Vec::new();
    for entry in netlog.iter().skip(start) {
        let (level, color) = match entry.level {
            NetLevel::Info => ("net", theme.dim),
            NetLevel::Ok => ("ok ", theme.peers[2]),
            NetLevel::Warn => ("wrn", theme.peers[4]),
            NetLevel::Error => ("err", theme.peers[0]),
            NetLevel::Traffic => ("io ", theme.peers[3]),
        };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(
                fmt_time(entry.timestamp),
                Style::default().fg(theme.timestamp),
            ),
            Span::raw(" "),
            Span::styled(
                level,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                clip(&entry.text, top[1].width.saturating_sub(17) as usize),
                Style::default().fg(color),
            ),
        ])));
    }
    let list = List::new(items)
        .block(
            block(
                Line::from(Span::styled(" Events ", Style::default().fg(theme.accent))),
                theme,
            )
            .style(Style::default().bg(theme.panel)),
        )
        .style(Style::default().bg(theme.panel));
    frame.render_widget(list, top[1]);
}

fn render_input(
    frame: &mut Frame<'_>,
    app: &Arc<AppState>,
    theme: Theme,
    area: Rect,
    input: &InputState,
) {
    let nick = app.nick();
    let line = Line::from(vec![
        Span::styled(
            nick,
            Style::default()
                .fg(theme.peers[color_idx(&app.identity.peer_id)])
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " : ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(input.buf.as_str(), Style::default().fg(theme.fg)),
    ]);
    let p = Paragraph::new(line)
        .block(
            block(
                Line::from(Span::styled(" Message ", Style::default().fg(theme.accent))),
                theme,
            )
            .style(Style::default().bg(theme.panel)),
        )
        .style(Style::default().bg(theme.panel).fg(theme.fg));
    frame.render_widget(p, area);
}

fn render_status(frame: &mut Frame<'_>, app: &Arc<AppState>, theme: Theme, area: Rect) {
    let peers = app.connected_peers().len();
    let (rx, tx) = collect_msg_stats(app);
    let label = if area.width < 90 {
        format!(" /connect /help     {peers} peers  {rx}/{tx} msg ")
    } else {
        format!(
            " /connect /status /inspect /id /peers /clear /send /accept /theme /quit     {peers} peers  {rx}/{tx} msg "
        )
    };
    let gauge = Gauge::default()
        .block(Block::default().style(Style::default().bg(theme.panel)))
        .gauge_style(
            Style::default()
                .fg(if peers > 0 {
                    theme.peers[2]
                } else {
                    theme.accent
                })
                .bg(theme.panel),
        )
        .label(label)
        .ratio(if peers > 0 { 1.0 } else { 0.08 });
    frame.render_widget(gauge, area);
}

pub fn collect_msg_stats(app: &Arc<AppState>) -> (u64, u64) {
    let peers = app.peers.lock().unwrap().clone();
    let mut rx = 0;
    let mut tx = 0;
    for peer in peers {
        let info = peer.info.lock().unwrap();
        if info.handshake_done {
            rx += info.msgs_rx;
            tx += info.msgs_tx;
        }
    }
    (rx, tx)
}
