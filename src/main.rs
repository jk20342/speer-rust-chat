mod app;
mod constants;
mod ffi;
mod files;
mod net;
mod protocol;
mod rendezvous;
mod tui;
mod util;

use crate::app::{theme, AppState, NetLevel};
use crate::constants::{CHAT_TYPE_BYE, CHAT_TYPE_MSG, MAX_NICK_LEN};
use crate::ffi::{discover_lan_ip, make_identity, random_instance_name, tcp_listen};
use crate::files::{cmd_accept_file, cmd_send_file};
use crate::net::{bootstrap_thread, dial_peer_addr, discovery_thread, parse_multiaddr};
use crate::rendezvous::{rendezvous_client_thread, rendezvous_server_thread};
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
    connect: Vec<String>,
    bootstrap: Vec<String>,
    rendezvous: Option<String>,
    rendezvous_listen: Option<String>,
    room: String,
    public_addr: Option<String>,
}

fn parse_args() -> Args {
    let mut args = Args {
        nick: "anon".to_string(),
        theme: "midnight".to_string(),
        port: 0,
        connect: Vec::new(),
        bootstrap: Vec::new(),
        rendezvous: None,
        rendezvous_listen: None,
        room: "default".to_string(),
        public_addr: None,
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
        } else if arg == "--connect" {
            if let Some(v) = it.next() {
                args.connect.push(v);
            }
        } else if let Some(v) = arg.strip_prefix("--connect=") {
            args.connect.push(v.to_string());
        } else if arg == "--bootstrap" {
            if let Some(v) = it.next() {
                args.bootstrap.push(v);
            }
        } else if let Some(v) = arg.strip_prefix("--bootstrap=") {
            args.bootstrap.push(v.to_string());
        } else if arg == "--rendezvous" {
            if let Some(v) = it.next() {
                args.rendezvous = Some(v);
            }
        } else if let Some(v) = arg.strip_prefix("--rendezvous=") {
            args.rendezvous = Some(v.to_string());
        } else if arg == "--rendezvous-listen" {
            if let Some(v) = it.next() {
                args.rendezvous_listen = Some(v);
            }
        } else if let Some(v) = arg.strip_prefix("--rendezvous-listen=") {
            args.rendezvous_listen = Some(v.to_string());
        } else if arg == "--room" {
            if let Some(v) = it.next() {
                args.room = v;
            }
        } else if let Some(v) = arg.strip_prefix("--room=") {
            args.room = v.to_string();
        } else if arg == "--public-addr" {
            if let Some(v) = it.next() {
                args.public_addr = Some(v);
            }
        } else if let Some(v) = arg.strip_prefix("--public-addr=") {
            args.public_addr = Some(v.to_string());
        } else if !arg.starts_with('-') {
            args.nick = arg.chars().take(MAX_NICK_LEN - 1).collect();
        }
    }
    args
}

fn rendezvous_token(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control() && !ch.is_whitespace())
        .collect()
}

fn is_private_or_loopback_host(host: &str) -> bool {
    if host == "localhost" || host == "127.0.0.1" || host == "::1" {
        return true;
    }
    let octets: Vec<_> = host
        .split('.')
        .filter_map(|part| part.parse::<u8>().ok())
        .collect();
    match octets.as_slice() {
        [10, _, _, _] | [127, _, _, _] | [192, 168, _, _] => true,
        [172, second, _, _] => (16..=31).contains(second),
        _ => false,
    }
}

fn public_addr_warning(public_addr: &str) -> Option<String> {
    let Some((host, _)) = parse_multiaddr(public_addr) else {
        return Some(format!(
            "public addr is not dialable: {public_addr}; use host:port or /ip4|/dns/<host>/tcp/<port>/p2p/<peer-id>"
        ));
    };
    if is_private_or_loopback_host(&host) {
        return Some(format!(
            "public addr uses private host {host}; peers on another network probably need a public DNS/IP plus port forwarding"
        ));
    }
    None
}

fn handle_command(app: &Arc<AppState>, input: &str, public_addr: &str) {
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
        app.emit_system("stack: mDNS/connect/bootstrap/rendezvous -> TCP -> Noise XX -> Yamux -> /speer/chat/1.0.0");
    } else if input == "/id" || input == "/me" {
        app.emit_system(format!("nick: {}", app.nick()));
        app.emit_system(format!("peer id: {}", app.identity.peer_id));
        app.emit_system(format!(
            "multiaddr: /ip4/{}/tcp/{}/p2p/{}",
            app.lan_ip.lock().unwrap(),
            app.listen_port.load(Ordering::Relaxed),
            app.identity.peer_id
        ));
        app.emit_system(format!("public addr: {public_addr}"));
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
        app.emit_system("Commands: /connect <addr> /send <path> /accept [id] /status /inspect /id /peers /clear /log clear /theme modern|midnight|original /quit");
        app.emit_system("Startup flags: --connect <addr> --bootstrap <addr> --rendezvous <host:port> --room <name> --public-addr <addr>");
    } else if let Some(arg) = input.strip_prefix("/connect ") {
        let addr = arg.trim();
        if addr.is_empty() {
            app.emit_error("usage: /connect <host:port|multiaddr>");
        } else {
            dial_peer_addr(app.clone(), "manual", None, addr, false, false);
        }
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
    let default_public_addr = format!(
        "/ip4/{}/tcp/{}/p2p/{}",
        app.lan_ip.lock().unwrap(),
        port,
        app.identity.peer_id
    );
    let public_addr = args.public_addr.clone().unwrap_or(default_public_addr);

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
    if let Some(server) = &args.rendezvous {
        let sanitized_room = rendezvous_token(&args.room);
        app.netlog(
            NetLevel::Info,
            format!("rendezvous server {server} room {sanitized_room}"),
        );
        if sanitized_room.is_empty() {
            app.emit_error("rendezvous room cannot be empty");
        } else if sanitized_room != args.room {
            app.netlog(
                NetLevel::Warn,
                format!("rendezvous room sanitized to {sanitized_room}"),
            );
        }
        if args.public_addr.is_none() {
            app.netlog(
                NetLevel::Warn,
                "no --public-addr set; advertising local LAN address",
            );
        }
        if let Some(warning) = public_addr_warning(&public_addr) {
            app.netlog(NetLevel::Warn, warning);
        }
    }

    let discovery = discovery_thread(app.clone(), listen_fd, random_instance_name());
    let mut background = Vec::new();
    if !args.bootstrap.is_empty() {
        background.push(bootstrap_thread(app.clone(), args.bootstrap.clone()));
    }
    if let Some(bind) = args.rendezvous_listen.clone() {
        background.push(rendezvous_server_thread(app.clone(), bind));
    }
    if let Some(server) = args.rendezvous.clone() {
        background.push(rendezvous_client_thread(
            app.clone(),
            server,
            args.room.clone(),
            public_addr.clone(),
        ));
    }
    for addr in &args.connect {
        dial_peer_addr(app.clone(), "manual", None, addr, false, false);
    }

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
                handle_command(&app, &line, &public_addr);
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
    for handle in background {
        let _ = handle.join();
    }
    Ok(())
}
