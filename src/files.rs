use crate::app::{AppState, NetLevel, PeerHandle, RxFile, TxFile};
use crate::constants::{
    CHAT_TYPE_FILE_ACCEPT, CHAT_TYPE_FILE_CHUNK, CHAT_TYPE_FILE_DONE, CHAT_TYPE_FILE_META,
    FILE_CHUNK_BYTES,
};
use crate::util::{hex_decode, hex_encode, sanitize_file_name, unix_time_secs};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub fn handle_file_frame(app: &Arc<AppState>, peer: &PeerHandle, kind: u32, payload: &str) {
    match kind {
        CHAT_TYPE_FILE_META => receive_file_meta(app, peer, payload),
        CHAT_TYPE_FILE_CHUNK => receive_file_chunk(app, peer, payload),
        CHAT_TYPE_FILE_DONE => receive_file_done(app, peer, payload),
        CHAT_TYPE_FILE_ACCEPT => {
            if let Ok(id) = payload.parse::<u32>() {
                app.netlog(NetLevel::Ok, "file accepted by peer");
                send_file_to_peer(app, peer, id);
            }
        }
        _ => {}
    }
}

fn receive_file_meta(app: &Arc<AppState>, peer: &PeerHandle, payload: &str) {
    let mut parts = payload.splitn(3, '|');
    let Some(id) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
        app.netlog(NetLevel::Warn, "bad file metadata");
        return;
    };
    let Some(size) = parts.next().and_then(|s| s.parse::<u64>().ok()) else {
        app.netlog(NetLevel::Warn, "bad file metadata");
        return;
    };
    let Some(name) = parts.next() else {
        app.netlog(NetLevel::Warn, "bad file metadata");
        return;
    };

    if !app.can_receive_file() {
        app.emit_error("too many incoming files");
        return;
    }

    let clean_name = sanitize_file_name(name);
    let info = peer.info.lock().unwrap();
    let sender = info.remote_pid_full.clone();
    let display_sender = sanitize_file_name(if info.remote_nick.is_empty() {
        &info.remote_pid_short
    } else {
        &info.remote_nick
    });
    drop(info);

    let path = PathBuf::from("speer_received").join(format!("{display_sender}_{id}_{clean_name}"));
    app.files.lock().unwrap().rx.push(RxFile {
        id,
        expected: size,
        received: 0,
        sender,
        name: clean_name.clone(),
        path,
        file: None,
    });

    app.emit_system(format!(
        "incoming file {clean_name} ({size} bytes). type /accept {id} to receive"
    ));
    app.netlog(NetLevel::Warn, format!("file pending {clean_name} id {id}"));
}

fn receive_file_chunk(app: &Arc<AppState>, peer: &PeerHandle, payload: &str) {
    let Some((id_s, hex)) = payload.split_once('|') else {
        app.netlog(NetLevel::Warn, "bad file chunk");
        return;
    };
    let Ok(id) = id_s.parse::<u32>() else {
        app.netlog(NetLevel::Warn, "bad file chunk");
        return;
    };
    let Ok(chunk) = hex_decode(hex) else {
        app.netlog(NetLevel::Warn, "bad file chunk hex");
        return;
    };

    let sender = peer.info.lock().unwrap().remote_pid_full.clone();
    let mut files = app.files.lock().unwrap();
    let Some(rx) = files
        .rx
        .iter_mut()
        .find(|f| f.id == id && f.sender == sender)
    else {
        app.netlog(NetLevel::Warn, format!("chunk for unknown file {id}"));
        return;
    };
    let Some(file) = rx.file.as_mut() else {
        app.netlog(
            NetLevel::Warn,
            format!("ignored unaccepted chunk {}", rx.name),
        );
        return;
    };
    if file.write_all(&chunk).is_ok() {
        rx.received += chunk.len() as u64;
        app.netlog(
            NetLevel::Traffic,
            format!("file rx {} {}/{}B", rx.name, rx.received, rx.expected),
        );
    }
}

fn receive_file_done(app: &Arc<AppState>, peer: &PeerHandle, payload: &str) {
    let Ok(id) = payload.parse::<u32>() else {
        return;
    };
    let sender = peer.info.lock().unwrap().remote_pid_full.clone();
    let mut files = app.files.lock().unwrap();
    let Some(pos) = files
        .rx
        .iter()
        .position(|f| f.id == id && f.sender == sender)
    else {
        return;
    };
    let mut rx = files.rx.remove(pos);
    drop(rx.file.take());
    let ok = rx.received == rx.expected;
    app.emit_system(format!(
        "received file {} -> {} ({}/{} bytes){}",
        rx.name,
        rx.path.display(),
        rx.received,
        rx.expected,
        if ok { "" } else { " incomplete" }
    ));
    app.netlog(
        if ok { NetLevel::Ok } else { NetLevel::Warn },
        format!("file saved {}", rx.name),
    );
}

pub fn send_file_to_peer(app: &Arc<AppState>, peer: &PeerHandle, file_id: u32) {
    let tx = {
        let files = app.files.lock().unwrap();
        files.tx.iter().find(|f| f.id == file_id).cloned()
    };
    let Some(tx) = tx else {
        app.netlog(NetLevel::Warn, format!("accept for unknown file {file_id}"));
        return;
    };

    let Ok(mut file) = File::open(&tx.path) else {
        app.emit_error(format!("could not reopen {}", tx.name));
        return;
    };

    let mut sent = 0u64;
    let mut chunk = [0u8; FILE_CHUNK_BYTES];
    while let Ok(n) = file.read(&mut chunk) {
        if n == 0 {
            break;
        }
        sent += n as u64;
        peer.enqueue(
            CHAT_TYPE_FILE_CHUNK,
            format!("{}|{}", file_id, hex_encode(&chunk[..n])),
        );
        app.netlog(
            NetLevel::Traffic,
            format!("file tx {} {sent}/{}B", tx.name, tx.size),
        );
    }
    peer.enqueue(CHAT_TYPE_FILE_DONE, file_id.to_string());
    app.netlog(NetLevel::Ok, format!("file tx done {}", tx.name));
}

pub fn cmd_send_file(app: &Arc<AppState>, arg: &str) {
    let path = arg.trim().trim_matches('"');
    if path.is_empty() {
        app.emit_error("usage: /send <path>");
        return;
    }
    if app.connected_peers().is_empty() {
        app.emit_error("no connected peers for file transfer");
        return;
    }
    let path = Path::new(path);
    let Ok(meta) = fs::metadata(path) else {
        app.emit_error(format!("could not open file: {}", path.display()));
        return;
    };
    let file_id = app.next_file_id.fetch_add(1, Ordering::Relaxed) ^ unix_time_secs() as u32;
    let name = sanitize_file_name(path.file_name().and_then(|s| s.to_str()).unwrap_or("blob"));
    app.files.lock().unwrap().tx.push(TxFile {
        id: file_id,
        size: meta.len(),
        name: name.clone(),
        path: path.to_path_buf(),
    });
    app.broadcast(
        CHAT_TYPE_FILE_META,
        &format!("{file_id}|{}|{name}", meta.len()),
    );
    app.emit_system(format!(
        "offered file {name} ({} bytes), waiting for peer acceptance",
        meta.len()
    ));
    app.netlog(NetLevel::Ok, format!("file offered {name} id {file_id}"));
}

pub fn cmd_accept_file(app: &Arc<AppState>, wanted: Option<u32>) {
    if let Err(err) = fs::create_dir_all("speer_received") {
        app.emit_error(format!("could not create speer_received directory: {err}"));
        return;
    }

    let (sender, id, name, path) = {
        let mut files = app.files.lock().unwrap();
        let candidates: Vec<usize> = files
            .rx
            .iter()
            .enumerate()
            .filter(|(_, f)| f.file.is_none() && wanted.is_none_or(|id| f.id == id))
            .map(|(i, _)| i)
            .collect();
        if candidates.is_empty() || (wanted.is_none() && candidates.len() > 1) {
            app.emit_error(if wanted.is_none() && candidates.len() > 1 {
                "multiple pending files; use /accept <id>"
            } else {
                "no pending file to accept"
            });
            return;
        }
        let idx = candidates[0];
        let path = files.rx[idx].path.clone();
        let Ok(file) = File::create(&path) else {
            app.emit_error("could not open receive file");
            return;
        };
        files.rx[idx].file = Some(file);
        (
            files.rx[idx].sender.clone(),
            files.rx[idx].id,
            files.rx[idx].name.clone(),
            path,
        )
    };

    for peer in app.connected_peers() {
        if peer.info.lock().unwrap().remote_pid_full == sender {
            peer.enqueue(CHAT_TYPE_FILE_ACCEPT, id.to_string());
            break;
        }
    }
    app.emit_system(format!("accepted file {name} -> {}", path.display()));
    app.netlog(NetLevel::Ok, format!("file accept {name} id {id}"));
}
