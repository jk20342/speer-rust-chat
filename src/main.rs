mod app;
mod constants;
mod ffi;
mod files;
mod net;
mod protocol;
mod tui;
mod util;

use crate::app::{theme, AppState, NetLevel};
use crate::constants::{CHAT_TYPE_BYE, CHAT_TYPE_MSG, MAX_NICK_LEN};
use crate::ffi::{discover_lan_ip, make_identity, random_instance_name, tcp_listen};
use crate::files::{cmd_accept_file, cmd_send_file};
use crate::net::discovery_thread;
use crate::tui::{collect_msg_stats, poll_input, render, InputState, TerminalGuard, UiAction};
use crate::util::{fmt_duration, IfEmpty};
use speer::sys;
use std::env;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

pub type AnyResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Default)]
struct Args {
    nick: String,
    theme: String,
    port: u16,
}

fn parse_args() -> Args {
    let mut args = Args {
        nick: "anon".to_string(),
        theme: "modern".to_string(),
        port: 0,
    };
    let mut it = env::args().skip(1);
    while let Some(arg) = it.next() {
        if arg == "--nick" {
            if let Some(v) = it.next() {
                args.nick = v.chars().take(MAX_NICK_LEN - 1).collect();
            }
        } else if let Some(v) = arg.strip_prefix("--nick=") {
            args.nick = v.chars().take(MAX_NICK_LEN - 1).collect();
        } else if arg == "--theme" {
            if let Some(v) = it.next() {
                args.theme = v;
            }
        } else if let Some(v) = arg.strip_prefix("--theme=") {
            args.theme = v.to_string();
        } else if arg == "--port" {
            if let Some(v) = it.next() {
                args.port = v.parse().unwrap_or(0);
            }
        } else if let Some(v) = arg.strip_prefix("--port=") {
            args.port = v.parse().unwrap_or(0);
        } else if !arg.starts_with('-') {
            args.nick = arg.chars().take(MAX_NICK_LEN - 1).collect();
        }
    }
    args
}

fn handle_command(app: &Arc<AppState>, input: &str) {
    if input == "/quit" || input == "/exit" {
        app.quit.store(true, Ordering::Relaxed);
    } else if input == "/peers" || input == "/who" {
        let peers = app.connected_peers();
        app.emit_system(format!(
            "{} peer{} connected",
            peers.len(),
            if peers.len() == 1 { "" } else { "s" }
        ));
        for peer in peers {
            let info = peer.info.lock().unwrap();
            app.emit_system(format!(
                "{}  {}  {}  rx{} tx{}",
                info.remote_nick.clone().if_empty("?"),
                info.remote_pid_short,
                info.addr,
                info.msgs_rx,
                info.msgs_tx
            ));
        }
    } else if input == "/status" {
        let (rx, tx) = collect_msg_stats(app);
        app.emit_system(format!(
            "status: {}:{}  uptime {}  peers {}  rx {} msg  tx {} msg",
            app.lan_ip.lock().unwrap(),
            app.listen_port.load(Ordering::Relaxed),
            fmt_duration(app.started_at.elapsed()),
            app.connected_peers().len(),
            rx,
            tx
        ));
        app.emit_system("stack: mDNS discovery -> TCP -> Noise XX -> Yamux -> /speer/chat/1.0.0");
    } else if input == "/id" || input == "/me" {
        app.emit_system(format!("nick: {}", app.nick()));
        app.emit_system(format!("peer id: {}", app.identity.peer_id));
        app.emit_system(format!(
            "multiaddr: /ip4/{}/tcp/{}/p2p/{}",
            app.lan_ip.lock().unwrap(),
            app.listen_port.load(Ordering::Relaxed),
            app.identity.peer_id
        ));
    } else if input == "/inspect" || input == "/diag" {
        let peers = app.connected_peers();
        if peers.is_empty() {
            app.emit_system("inspect: no connected peers yet");
        }
        for (i, peer) in peers.iter().enumerate() {
            let info = peer.info.lock().unwrap();
            app.emit_system(format!(
                "peer {}: {}  {}",
                i + 1,
                info.remote_nick.clone().if_empty("?"),
                info.remote_pid_full
                    .clone()
                    .if_empty(&info.remote_pid_short)
            ));
            app.emit_system(format!(
                "  addr {}  role {}  rx {}/{}B  tx {}/{}B",
                info.addr,
                if info.initiator { "dialer" } else { "listener" },
                info.msgs_rx,
                info.bytes_rx,
                info.msgs_tx,
                info.bytes_tx
            ));
        }
    } else if input == "/clear" {
        app.history.lock().unwrap().clear();
        app.emit_system("timeline cleared");
    } else if input == "/log clear" {
        app.netlog.lock().unwrap().clear();
        app.netlog(NetLevel::Ok, "network console cleared");
    } else if input == "/help" {
        app.emit_system("Commands: /send <path> /accept [id] /status /inspect /id /peers /clear /log clear /theme modern|midnight|original /quit");
    } else if let Some(arg) = input
        .strip_prefix("/send ")
        .or_else(|| input.strip_prefix("send "))
    {
        cmd_send_file(app, arg);
    } else if input == "/accept" {
        cmd_accept_file(app, None);
    } else if let Some(arg) = input.strip_prefix("/accept ") {
        cmd_accept_file(app, arg.trim().parse().ok());
    } else if let Some(arg) = input.strip_prefix("/theme ") {
        *app.theme.lock().unwrap() = theme(arg.trim());
        app.emit_system("Theme changed");
    } else if input.starts_with('/') {
        app.emit_error("Unknown command");
    } else {
        app.broadcast(CHAT_TYPE_MSG, input);
        app.emit_chat(&app.identity.peer_id, &app.nick(), input);
    }
}

fn main() -> AnyResult<()> {
    let args = parse_args();
    let identity = make_identity()?;
    let app = Arc::new(AppState::new(identity, args.nick, &args.theme));

    let lan_ip = discover_lan_ip();
    *app.lan_ip.lock().unwrap() = lan_ip;

    let (listen_fd, port) = tcp_listen(args.port)?;
    app.listen_port.store(port, Ordering::Relaxed);

    app.emit_system("Welcome to speer-chat. Type /help for commands.");
    app.netlog(
        NetLevel::Ok,
        format!("identity ready {}", app.identity.peer_id),
    );
    app.netlog(
        NetLevel::Ok,
        format!("tcp listen {}:{port}", app.lan_ip.lock().unwrap()),
    );
    app.netlog(
        NetLevel::Ok,
        format!("mDNS service {}", constants::CHAT_SERVICE_TYPE),
    );

    let discovery = discovery_thread(app.clone(), listen_fd, random_instance_name());

    let mut guard = TerminalGuard::enter()?;
    let mut input = InputState::new();
    let mut last_render = Instant::now();
    render(guard.terminal(), &app, &input)?;

    while !app.quit.load(Ordering::Relaxed) {
        let mut should_render = last_render.elapsed() >= Duration::from_millis(250);
        match poll_input(&mut input)? {
            UiAction::None => {}
            UiAction::Redraw => should_render = true,
            UiAction::Quit => app.quit.store(true, Ordering::Relaxed),
            UiAction::Submit(line) => {
                handle_command(&app, &line);
                should_render = true;
            }
        }
        if should_render {
            render(guard.terminal(), &app, &input)?;
            last_render = Instant::now();
        }
    }

    app.broadcast(CHAT_TYPE_BYE, "");
    thread::sleep(Duration::from_millis(200));
    unsafe {
        sys::speer_tcp_close(listen_fd);
    }
    app.quit.store(true, Ordering::Relaxed);
    let _ = discovery.join();
    Ok(())
}
