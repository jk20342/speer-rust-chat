use crate::app::{AppState, NetLevel};
use crate::net::dial_peer_addr;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const ENTRY_TTL: Duration = Duration::from_secs(180);

#[derive(Clone)]
struct Entry {
    peer_id: String,
    addr: String,
    seen_at: Instant,
}

type Rooms = Arc<Mutex<HashMap<String, Vec<Entry>>>>;

pub fn rendezvous_server_thread(app: Arc<AppState>, bind: String) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let Ok(listener) = TcpListener::bind(&bind) else {
            app.emit_error(format!("rendezvous listen failed {bind}"));
            return;
        };
        if listener.set_nonblocking(true).is_err() {
            app.emit_error("rendezvous nonblocking failed");
            return;
        }
        app.netlog(NetLevel::Ok, format!("rendezvous listen {bind}"));
        let rooms = Arc::new(Mutex::new(HashMap::new()));
        while !app.quit.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let rooms = rooms.clone();
                    thread::spawn(move || handle_rendezvous_client(stream, rooms));
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(err) => {
                    app.netlog(NetLevel::Warn, format!("rendezvous accept failed {err}"));
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }
    })
}

pub fn rendezvous_client_thread(
    app: Arc<AppState>,
    server: String,
    room: String,
    public_addr: String,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut tick = 0u32;
        while !app.quit.load(Ordering::Relaxed) {
            if tick == 0 {
                match rendezvous_round(&server, &room, &app.identity.peer_id, &public_addr) {
                    Ok(peers) => {
                        app.netlog(
                            NetLevel::Info,
                            format!("rendezvous {room}: {} peer hints", peers.len()),
                        );
                        for (peer_id, addr) in peers {
                            if peer_id != app.identity.peer_id {
                                dial_peer_addr(
                                    app.clone(),
                                    "rendezvous",
                                    Some(&peer_id),
                                    &addr,
                                    false,
                                    false,
                                );
                            }
                        }
                    }
                    Err(err) => app.netlog(NetLevel::Warn, format!("rendezvous failed {err}")),
                }
            }
            tick = (tick + 1) % 20;
            thread::sleep(Duration::from_secs(1));
        }
    })
}

fn rendezvous_round(
    server: &str,
    room: &str,
    peer_id: &str,
    public_addr: &str,
) -> std::io::Result<Vec<(String, String)>> {
    let addr = server.to_socket_addrs()?.next().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "no rendezvous address")
    })?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    writeln!(
        stream,
        "REGISTER {} {} {}",
        token(room),
        token(peer_id),
        token(public_addr)
    )?;
    writeln!(stream, "LIST {}", token(room))?;
    writeln!(stream, "QUIT")?;

    let mut peers = Vec::new();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    while reader.read_line(&mut line)? != 0 {
        let trimmed = line.trim();
        if trimmed == "END" {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("PEER ") {
            let mut parts = rest.splitn(2, ' ');
            if let (Some(peer_id), Some(addr)) = (parts.next(), parts.next()) {
                peers.push((peer_id.to_string(), addr.to_string()));
            }
        }
        line.clear();
    }
    Ok(peers)
}

fn handle_rendezvous_client(stream: TcpStream, rooms: Rooms) {
    let mut writer = match stream.try_clone() {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) != 0 {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("REGISTER ") {
            let mut parts = rest.splitn(3, ' ');
            if let (Some(room), Some(peer_id), Some(addr)) =
                (parts.next(), parts.next(), parts.next())
            {
                register_peer(&rooms, room, peer_id, addr);
                let _ = writeln!(writer, "OK");
            }
        } else if let Some(room) = trimmed.strip_prefix("LIST ") {
            let peers = list_peers(&rooms, room);
            for peer in peers {
                let _ = writeln!(writer, "PEER {} {}", peer.peer_id, peer.addr);
            }
            let _ = writeln!(writer, "END");
        } else if trimmed == "QUIT" {
            break;
        }
        line.clear();
    }
}

fn register_peer(rooms: &Rooms, room: &str, peer_id: &str, addr: &str) {
    let mut rooms = rooms.lock().unwrap();
    let peers = rooms.entry(room.to_string()).or_default();
    let now = Instant::now();
    peers.retain(|entry| now.duration_since(entry.seen_at) < ENTRY_TTL && entry.peer_id != peer_id);
    peers.push(Entry {
        peer_id: peer_id.to_string(),
        addr: addr.to_string(),
        seen_at: now,
    });
}

fn list_peers(rooms: &Rooms, room: &str) -> Vec<Entry> {
    let mut rooms = rooms.lock().unwrap();
    let now = Instant::now();
    let peers = rooms.entry(room.to_string()).or_default();
    peers.retain(|entry| now.duration_since(entry.seen_at) < ENTRY_TTL);
    peers.clone()
}

fn token(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control() && !ch.is_whitespace())
        .collect()
}
