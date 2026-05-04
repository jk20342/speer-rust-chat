use crate::app::{AppState, NetLevel, PeerHandle};
use crate::constants::{
    CHAT_PROTO, CHAT_SERVICE_TYPE, CHAT_TYPE_BYE, CHAT_TYPE_HELLO, CHAT_TYPE_MSG,
    HANDSHAKE_TIMEOUT_MS,
};
use crate::ffi::{
    boxed_zeroed, c_char_array_to_string, cstring, identity_raw, tcp_dial, SessionPtr, StreamPtr,
};
use crate::files::handle_file_frame;
use crate::protocol::{chat_frame_decode, is_file_frame, recv_chat_frame, send_chat_frame};
use crate::util::truncate_pid;
use crate::AnyResult;
use speer::sys;
use std::ffi::{c_char, c_void, CStr};
use std::ptr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

pub fn start_peer(app: Arc<AppState>, initiator: bool, fd: i32, addr: String) {
    let peer = PeerHandle::new(initiator, addr);
    if !app.add_peer(peer.clone()) {
        unsafe { sys::speer_tcp_close(fd) };
        app.emit_error("too many peers");
        return;
    }

    thread::spawn(move || {
        let result = peer_worker(app.clone(), peer.clone(), fd);
        match result {
            Ok(()) => {}
            Err(ref err) => {
                let handshake_done = peer.info.lock().unwrap().handshake_done;
                if handshake_done {
                    app.emit_error(format!("peer error: {err}"));
                } else {
                    app.netlog(NetLevel::Warn, format!("incoming connection closed: {err}"));
                }
            }
        }
        peer.dead.store(true, Ordering::Relaxed);
        peer.info.lock().unwrap().active = false;
        app.remove_peer(&peer);
    });
}

pub fn dial_peer_addr(
    app: Arc<AppState>,
    source: &str,
    peer_id: Option<&str>,
    addr: &str,
    tie_break: bool,
    remember_attempt: bool,
) -> bool {
    if let Some(peer_id) = peer_id {
        if peer_id.is_empty() || peer_id == app.identity.peer_id {
            return false;
        }
        if app
            .connected_peers()
            .iter()
            .any(|p| p.info.lock().unwrap().remote_pid_full == peer_id)
        {
            return false;
        }
        if tie_break && app.identity.peer_id.as_str() > peer_id {
            return false;
        }
        if remember_attempt {
            let key = format!("peer:{peer_id}");
            let mut attempted = app.attempted.lock().unwrap();
            if !attempted.insert(key) {
                return false;
            }
        }
    }

    let Some((host, port)) = parse_multiaddr(addr) else {
        app.netlog(NetLevel::Warn, format!("{source} bad addr {addr}"));
        return false;
    };
    let dial_addr = format!("{host}:{port}");
    if app
        .connected_peers()
        .iter()
        .any(|p| p.info.lock().unwrap().addr == dial_addr)
    {
        return false;
    }

    if remember_attempt && peer_id.is_none() {
        let key = format!("addr:{dial_addr}");
        let mut attempted = app.attempted.lock().unwrap();
        if !attempted.insert(key) {
            return false;
        }
    }

    app.netlog(NetLevel::Info, format!("{source} dial {addr}"));
    if let Some(fd) = tcp_dial(&host, port) {
        start_peer(app, true, fd, dial_addr);
        true
    } else {
        app.netlog(NetLevel::Warn, format!("dial failed {host}:{port}"));
        false
    }
}

fn peer_worker(app: Arc<AppState>, peer: PeerHandle, fd: i32) -> AnyResult<()> {
    unsafe {
        sys::speer_tcp_set_io_timeout(fd, HANDSHAKE_TIMEOUT_MS);
    }

    let mut session_box = unsafe { boxed_zeroed::<sys::speer_libp2p_tcp_session_t>() };
    let session = SessionPtr(session_box.as_mut() as *mut _);
    let identity = identity_raw(&app.identity);
    let initiator = peer.info.lock().unwrap().initiator;
    let rc = unsafe {
        if initiator {
            app.netlog(
                NetLevel::Info,
                format!("dial tcp {}", peer.info.lock().unwrap().addr),
            );
            sys::speer_libp2p_tcp_session_init_dialer(session.0, fd, &identity)
        } else {
            app.netlog(
                NetLevel::Info,
                format!("accept tcp {}", peer.info.lock().unwrap().addr),
            );
            sys::speer_libp2p_tcp_session_init_listener(session.0, fd, &identity)
        }
    };
    if rc != 0 {
        return Err("tcp/noise/yamux session init failed".into());
    }

    let remote_pid = unsafe { c_char_array_to_string(&(*session.0).remote_peer_id_b58) };
    if remote_pid == app.identity.peer_id {
        unsafe { sys::speer_libp2p_tcp_session_close(session.0) };
        return Ok(());
    }

    {
        let mut info = peer.info.lock().unwrap();
        info.remote_pid_full = remote_pid.clone();
        info.remote_pid_short = truncate_pid(&remote_pid);
    }
    app.netlog(
        NetLevel::Ok,
        format!("peer id {}", truncate_pid(&remote_pid)),
    );

    let chat_proto = cstring(CHAT_PROTO);
    let mut chat_stream = ptr::null_mut();
    let rc = unsafe {
        if initiator {
            sys::speer_libp2p_tcp_open_protocol_stream(
                session.0,
                chat_proto.as_ptr(),
                &mut chat_stream,
            )
        } else {
            let protocols = [chat_proto.as_ptr()];
            let mut selected = 0usize;
            sys::speer_libp2p_tcp_accept_protocol_stream(
                session.0,
                protocols.as_ptr(),
                protocols.len(),
                &mut selected,
                &mut chat_stream,
                HANDSHAKE_TIMEOUT_MS,
                50,
            )
        }
    };
    if rc != 0 || chat_stream.is_null() {
        unsafe { sys::speer_libp2p_tcp_session_close(session.0) };
        return Err("chat protocol negotiation failed".into());
    }

    let stream = StreamPtr(chat_stream);
    {
        let mut info = peer.info.lock().unwrap();
        info.handshake_done = true;
        info.connected_at = Some(Instant::now());
        info.last_seen = info.connected_at;
    }

    app.emit_join("(unknown)", &truncate_pid(&remote_pid));
    app.netlog(NetLevel::Ok, "opened chat stream");

    let nick = app.nick();
    if let Ok(bytes) = send_chat_frame(session, stream, CHAT_TYPE_HELLO, &nick, "") {
        peer.info.lock().unwrap().bytes_tx += bytes as u64;
        app.netlog(NetLevel::Traffic, format!("tx hello {bytes}B"));
    }

    unsafe {
        sys::speer_tcp_set_io_timeout(fd, 0);
    }

    let reader_app = app.clone();
    let reader_peer = peer.clone();
    let reader = thread::spawn(move || {
        peer_reader(reader_app, reader_peer, session, stream);
    });

    while !app.quit.load(Ordering::Relaxed) && !peer.dead.load(Ordering::Relaxed) {
        while let Some(msg) = peer.dequeue() {
            let nick = app.nick();
            match send_chat_frame(session, stream, msg.kind, &nick, &msg.text) {
                Ok(bytes) => {
                    let mut info = peer.info.lock().unwrap();
                    info.bytes_tx += bytes as u64;
                    info.last_seen = Some(Instant::now());
                    if msg.kind == CHAT_TYPE_MSG {
                        info.msgs_tx += 1;
                    }
                    app.netlog(NetLevel::Traffic, format!("tx frame {bytes}B"));
                }
                Err(_) => {
                    peer.dead.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }
        thread::sleep(Duration::from_millis(50));
    }

    peer.dead.store(true, Ordering::Relaxed);
    unsafe {
        sys::speer_tcp_close((*session.0).fd);
        (*session.0).fd = -1;
    }
    let _ = reader.join();
    unsafe {
        sys::speer_libp2p_tcp_session_close(session.0);
    }
    drop(session_box);

    let nick = {
        let info = peer.info.lock().unwrap();
        if info.remote_nick.is_empty() {
            info.remote_pid_short.clone()
        } else {
            info.remote_nick.clone()
        }
    };
    app.emit_leave(&nick);
    Ok(())
}

fn peer_reader(app: Arc<AppState>, peer: PeerHandle, session: SessionPtr, stream: StreamPtr) {
    while !app.quit.load(Ordering::Relaxed) && !peer.dead.load(Ordering::Relaxed) {
        let (frame, wire_len) = match recv_chat_frame(session, stream) {
            Ok(value) => value,
            Err(_) => break,
        };
        let (kind, nick, text) = match chat_frame_decode(&frame) {
            Ok(value) => value,
            Err(_) => continue,
        };

        {
            let mut info = peer.info.lock().unwrap();
            info.bytes_rx += wire_len as u64;
            info.last_seen = Some(Instant::now());
            if !nick.is_empty() {
                info.remote_nick = nick.clone();
            }
        }

        match kind {
            CHAT_TYPE_MSG => {
                let (pid, display) = {
                    let mut info = peer.info.lock().unwrap();
                    info.msgs_rx += 1;
                    (
                        info.remote_pid_full.clone(),
                        if info.remote_nick.is_empty() {
                            info.remote_pid_short.clone()
                        } else {
                            info.remote_nick.clone()
                        },
                    )
                };
                app.emit_chat(&pid, &display, &text);
                app.netlog(NetLevel::Traffic, format!("rx chat {wire_len}B"));
            }
            CHAT_TYPE_HELLO => {
                let display = {
                    let info = peer.info.lock().unwrap();
                    if info.remote_nick.is_empty() {
                        info.remote_pid_short.clone()
                    } else {
                        info.remote_nick.clone()
                    }
                };
                app.emit_join(
                    &display,
                    &truncate_pid(&peer.info.lock().unwrap().remote_pid_full),
                );
            }
            CHAT_TYPE_BYE => break,
            kind if is_file_frame(kind) => handle_file_frame(&app, &peer, kind, &text),
            _ => {}
        }
    }
    peer.dead.store(true, Ordering::Relaxed);
}

pub fn parse_multiaddr(multiaddr: &str) -> Option<(String, u16)> {
    let value = multiaddr.trim();
    if !value.starts_with('/') {
        let (host, port) = value.rsplit_once(':')?;
        if host.is_empty() {
            return None;
        }
        return Some((
            host.trim_matches(['[', ']']).to_string(),
            port.parse().ok()?,
        ));
    }

    let tcp_marker = "/tcp/";
    let tcp_start = value.find(tcp_marker)?;
    let host = parse_multiaddr_host(&value[..tcp_start])?;
    let port_start = tcp_start + tcp_marker.len();
    let port_end = value[port_start..]
        .find('/')
        .map(|i| port_start + i)
        .unwrap_or(value.len());
    let port = value[port_start..port_end].parse().ok()?;
    Some((host, port))
}

fn parse_multiaddr_host(prefix: &str) -> Option<String> {
    for marker in ["/ip4/", "/ip6/", "/dns/", "/dns4/", "/dns6/"] {
        if let Some(host) = prefix.strip_prefix(marker) {
            if !host.is_empty() {
                return Some(host.to_string());
            }
        }
    }
    None
}

struct DiscoveryCtx {
    app: Arc<AppState>,
}

unsafe extern "C" fn on_mdns_discover(
    user: *mut c_void,
    peer_id: *const c_char,
    multiaddr: *const c_char,
) {
    if user.is_null() || peer_id.is_null() || multiaddr.is_null() {
        return;
    }
    let ctx = unsafe { &*(user as *const DiscoveryCtx) };
    let peer_id = unsafe { CStr::from_ptr(peer_id) }
        .to_string_lossy()
        .to_string();
    let multiaddr = unsafe { CStr::from_ptr(multiaddr) }
        .to_string_lossy()
        .to_string();

    dial_peer_addr(
        ctx.app.clone(),
        "mDNS",
        Some(&peer_id),
        &multiaddr,
        true,
        true,
    );
}

pub fn bootstrap_thread(app: Arc<AppState>, peers: Vec<String>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut tick = 0u32;
        while !app.quit.load(Ordering::Relaxed) {
            if tick == 0 {
                for peer in &peers {
                    dial_peer_addr(app.clone(), "bootstrap", None, peer, false, false);
                }
            }
            tick = (tick + 1) % 30;
            thread::sleep(Duration::from_secs(1));
        }
    })
}

pub fn discovery_thread(
    app: Arc<AppState>,
    listen_fd: i32,
    instance_name: String,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut mctx = unsafe { boxed_zeroed::<sys::mdns_ctx_t>() };
        if unsafe { sys::mdns_init(mctx.as_mut()) } != 0 {
            app.emit_error("mdns init failed");
            return;
        }

        let lan_ip = app.lan_ip.lock().unwrap().clone();
        let port = app.listen_port.load(Ordering::Relaxed);
        let multiaddr = format!("/ip4/{lan_ip}/tcp/{port}/p2p/{}", app.identity.peer_id);
        let txt = format!("dnsaddr={multiaddr}");
        let mut txt_data = Vec::with_capacity(txt.len() + 1);
        txt_data.push(txt.len().min(255) as u8);
        txt_data.extend_from_slice(txt.as_bytes());

        let instance = cstring(&instance_name);
        let service = cstring(CHAT_SERVICE_TYPE);
        let rc = unsafe {
            sys::mdns_register_service(
                mctx.as_mut(),
                instance.as_ptr(),
                service.as_ptr(),
                port,
                txt_data.as_ptr(),
                txt_data.len(),
            )
        };
        if rc != 0 {
            app.emit_error("mdns register failed");
            unsafe { sys::mdns_free(mctx.as_mut()) };
            return;
        }

        let mut ctx = Box::new(DiscoveryCtx { app: app.clone() });
        unsafe {
            sys::mdns_set_discovery_callback(
                mctx.as_mut(),
                Some(on_mdns_discover),
                ctx.as_mut() as *mut _ as *mut c_void,
            );
        }

        let query = cstring(&format!("{CHAT_SERVICE_TYPE}.local"));
        unsafe {
            sys::mdns_announce(mctx.as_mut());
            sys::mdns_query(mctx.as_mut(), query.as_ptr());
            sys::speer_tcp_set_nonblocking(listen_fd, 1);
        }

        let mut last_maint = Instant::now();
        while !app.quit.load(Ordering::Relaxed) {
            let mut fd = -1;
            let mut peer_addr = [0 as c_char; 64];
            let rc = unsafe {
                sys::speer_tcp_accept(listen_fd, &mut fd, peer_addr.as_mut_ptr(), peer_addr.len())
            };
            if rc == 0 && fd >= 0 {
                let addr = crate::ffi::c_char_array_to_string(&peer_addr);
                app.netlog(NetLevel::Info, format!("tcp accept {addr}"));
                start_peer(app.clone(), false, fd, addr);
            }

            if last_maint.elapsed() >= Duration::from_secs(1) {
                unsafe {
                    sys::mdns_announce(mctx.as_mut());
                    sys::mdns_query(mctx.as_mut(), query.as_ptr());
                }
                last_maint = Instant::now();
            }
            unsafe {
                sys::mdns_poll(mctx.as_mut(), 100);
            }
        }

        unsafe {
            sys::mdns_unregister_service(mctx.as_mut(), instance.as_ptr());
            sys::mdns_free(mctx.as_mut());
        }
    })
}

#[cfg(test)]
mod tests {
    use super::parse_multiaddr;

    #[test]
    fn parses_ip4_multiaddr() {
        assert_eq!(
            parse_multiaddr("/ip4/127.0.0.1/tcp/4001/p2p/peer"),
            Some(("127.0.0.1".to_string(), 4001))
        );
    }

    #[test]
    fn parses_dns_multiaddr() {
        assert_eq!(
            parse_multiaddr("/dns/example.com/tcp/4001"),
            Some(("example.com".to_string(), 4001))
        );
        assert_eq!(
            parse_multiaddr("/dns4/bootstrap.example/tcp/4002/p2p/peer"),
            Some(("bootstrap.example".to_string(), 4002))
        );
    }

    #[test]
    fn parses_plain_host_port() {
        assert_eq!(
            parse_multiaddr("example.com:4001"),
            Some(("example.com".to_string(), 4001))
        );
        assert_eq!(
            parse_multiaddr("127.0.0.1:4002"),
            Some(("127.0.0.1".to_string(), 4002))
        );
    }
}
