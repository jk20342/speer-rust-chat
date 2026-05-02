use crate::constants::{MAX_PEERS, MAX_RX_FILES};
use crate::ffi::Identity;
use crate::util::color_idx;
use ratatui::style::Color;
use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: Color,
    pub panel: Color,
    pub fg: Color,
    pub dim: Color,
    pub border: Color,
    pub accent: Color,
    pub timestamp: Color,
    pub peers: [Color; 6],
}

fn rgb(v: u32) -> Color {
    Color::Rgb(
        ((v >> 16) & 0xff) as u8,
        ((v >> 8) & 0xff) as u8,
        (v & 0xff) as u8,
    )
}

pub fn theme(name: &str) -> Theme {
    match name {
        "midnight" => Theme {
            bg: rgb(0x050712),
            panel: rgb(0x0d1020),
            fg: rgb(0xf4f7ff),
            dim: rgb(0x7b8199),
            border: rgb(0x26314f),
            accent: rgb(0x35f7c8),
            timestamp: rgb(0x565d78),
            peers: [
                rgb(0xff4f87),
                rgb(0xb48cff),
                rgb(0x35f7c8),
                rgb(0x6ad7ff),
                rgb(0xffc857),
                rgb(0xff7ad9),
            ],
        },
        "original" => Theme {
            bg: rgb(0x1a1a2e),
            panel: rgb(0x252542),
            fg: rgb(0xe6e6fa),
            dim: rgb(0x6b6b8a),
            border: rgb(0x4b4b6b),
            accent: rgb(0xff8ab5),
            timestamp: rgb(0x7878a0),
            peers: [
                rgb(0xff8ab5),
                rgb(0xc7a8ff),
                rgb(0x90e1a0),
                rgb(0x88ccff),
                rgb(0xffb088),
                rgb(0xffe588),
            ],
        },
        _ => Theme {
            bg: rgb(0x070711),
            panel: rgb(0x111026),
            fg: rgb(0xf2efff),
            dim: rgb(0x8a84a6),
            border: rgb(0x9d2cff),
            accent: rgb(0xff2bd6),
            timestamp: rgb(0x6d668f),
            peers: [
                rgb(0xff2bd6),
                rgb(0x9d2cff),
                rgb(0x35f7c8),
                rgb(0x61d6ff),
                rgb(0xffc857),
                rgb(0xff7ab6),
            ],
        },
    }
}

#[derive(Clone, Copy)]
pub enum MsgKind {
    Chat,
    System,
    Join,
    Leave,
    Error,
}

pub struct Message {
    pub kind: MsgKind,
    pub timestamp: SystemTime,
    pub nick: String,
    pub text: String,
    pub color_idx: usize,
}

#[derive(Clone, Copy)]
pub enum NetLevel {
    Info,
    Ok,
    Warn,
    Error,
    Traffic,
}

pub struct NetEntry {
    pub timestamp: SystemTime,
    pub level: NetLevel,
    pub text: String,
}

pub struct RxFile {
    pub id: u32,
    pub expected: u64,
    pub received: u64,
    pub sender: String,
    pub name: String,
    pub path: PathBuf,
    pub file: Option<File>,
}

#[derive(Clone)]
pub struct TxFile {
    pub id: u32,
    pub size: u64,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone)]
pub struct OutMsg {
    pub kind: u32,
    pub text: String,
}

#[derive(Default)]
pub struct PeerInfo {
    pub initiator: bool,
    pub addr: String,
    pub remote_nick: String,
    pub remote_pid_short: String,
    pub remote_pid_full: String,
    pub connected_at: Option<Instant>,
    pub last_seen: Option<Instant>,
    pub msgs_rx: u64,
    pub msgs_tx: u64,
    pub bytes_rx: u64,
    pub bytes_tx: u64,
    pub handshake_done: bool,
    pub active: bool,
}

#[derive(Clone)]
pub struct PeerHandle {
    pub info: Arc<Mutex<PeerInfo>>,
    pub out: Arc<Mutex<VecDeque<OutMsg>>>,
    pub dead: Arc<AtomicBool>,
}

impl PeerHandle {
    pub fn new(initiator: bool, addr: String) -> Self {
        let info = PeerInfo {
            initiator,
            addr,
            active: true,
            ..PeerInfo::default()
        };
        Self {
            info: Arc::new(Mutex::new(info)),
            out: Arc::new(Mutex::new(VecDeque::new())),
            dead: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn enqueue(&self, kind: u32, text: impl Into<String>) {
        self.out.lock().unwrap().push_back(OutMsg {
            kind,
            text: text.into(),
        });
    }

    pub fn dequeue(&self) -> Option<OutMsg> {
        self.out.lock().unwrap().pop_front()
    }
}

pub struct FileState {
    pub rx: Vec<RxFile>,
    pub tx: Vec<TxFile>,
}

pub struct AppState {
    pub identity: Identity,
    nick: Mutex<String>,
    pub theme: Mutex<Theme>,
    pub history: Mutex<VecDeque<Message>>,
    pub netlog: Mutex<VecDeque<NetEntry>>,
    pub peers: Mutex<Vec<PeerHandle>>,
    pub files: Mutex<FileState>,
    pub attempted: Mutex<HashSet<String>>,
    pub quit: AtomicBool,
    pub next_file_id: AtomicU32,
    pub started_at: Instant,
    pub lan_ip: Mutex<String>,
    pub listen_port: AtomicU16,
}

impl AppState {
    pub fn new(identity: Identity, nick: String, theme_name: &str) -> Self {
        Self {
            identity,
            nick: Mutex::new(nick),
            theme: Mutex::new(theme(theme_name)),
            history: Mutex::new(VecDeque::with_capacity(1000)),
            netlog: Mutex::new(VecDeque::with_capacity(256)),
            peers: Mutex::new(Vec::new()),
            files: Mutex::new(FileState {
                rx: Vec::new(),
                tx: Vec::new(),
            }),
            attempted: Mutex::new(HashSet::new()),
            quit: AtomicBool::new(false),
            next_file_id: AtomicU32::new(1),
            started_at: Instant::now(),
            lan_ip: Mutex::new("127.0.0.1".to_string()),
            listen_port: AtomicU16::new(0),
        }
    }

    pub fn nick(&self) -> String {
        self.nick.lock().unwrap().clone()
    }

    pub fn emit_chat(&self, pid: &str, nick: &str, text: &str) {
        self.add_history(MsgKind::Chat, nick, pid, text);
    }

    pub fn emit_system(&self, text: impl AsRef<str>) {
        let text = text.as_ref();
        self.add_history(MsgKind::System, "", "", text);
        self.netlog(NetLevel::Info, text);
    }

    pub fn emit_error(&self, text: impl AsRef<str>) {
        let text = text.as_ref();
        self.add_history(MsgKind::Error, "", "", text);
        self.netlog(NetLevel::Error, text);
    }

    pub fn emit_join(&self, nick: &str, pid: &str) {
        self.add_history(MsgKind::Join, nick, pid, &format!("{nick} joined ({pid})"));
        self.netlog(NetLevel::Ok, format!("peer joined {nick}"));
    }

    pub fn emit_leave(&self, nick: &str) {
        self.add_history(MsgKind::Leave, nick, "", &format!("{nick} left"));
        self.netlog(NetLevel::Warn, format!("peer left {nick}"));
    }

    pub fn add_history(&self, kind: MsgKind, nick: &str, pid: &str, text: &str) {
        let mut history = self.history.lock().unwrap();
        if history.len() >= 1000 {
            history.pop_front();
        }
        history.push_back(Message {
            kind,
            timestamp: SystemTime::now(),
            nick: nick.to_string(),
            text: text.to_string(),
            color_idx: color_idx(pid),
        });
    }

    pub fn netlog(&self, level: NetLevel, text: impl AsRef<str>) {
        let mut netlog = self.netlog.lock().unwrap();
        if netlog.len() >= 256 {
            netlog.pop_front();
        }
        netlog.push_back(NetEntry {
            timestamp: SystemTime::now(),
            level,
            text: text.as_ref().to_string(),
        });
    }

    pub fn broadcast(&self, kind: u32, text: &str) {
        for peer in self.peers.lock().unwrap().clone() {
            let connected = peer.info.lock().unwrap().handshake_done;
            if connected && !peer.dead.load(Ordering::Relaxed) {
                peer.enqueue(kind, text);
            }
        }
    }

    pub fn connected_peers(&self) -> Vec<PeerHandle> {
        self.peers
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.info.lock().unwrap().handshake_done && !p.dead.load(Ordering::Relaxed))
            .cloned()
            .collect()
    }

    pub fn add_peer(&self, peer: PeerHandle) -> bool {
        let mut peers = self.peers.lock().unwrap();
        if peers.len() >= MAX_PEERS {
            false
        } else {
            peers.push(peer);
            true
        }
    }

    /// Release a peer slot when the worker exits (failed scan, failed handshake, or disconnect).
    pub fn remove_peer(&self, peer: &PeerHandle) {
        let mut peers = self.peers.lock().unwrap();
        peers.retain(|p| !std::sync::Arc::ptr_eq(&p.info, &peer.info));
    }

    pub fn can_receive_file(&self) -> bool {
        self.files.lock().unwrap().rx.len() < MAX_RX_FILES
    }
}
