use crate::constants::MAX_NICK_LEN;
use crate::AnyResult;
use speer::sys;
use std::alloc::{alloc_zeroed, handle_alloc_error, Layout};
use std::ffi::{c_char, CString};
use std::net::UdpSocket;
use std::ptr;

#[derive(Clone, Copy)]
pub struct SessionPtr(pub *mut sys::speer_libp2p_tcp_session_t);
unsafe impl Send for SessionPtr {}
unsafe impl Sync for SessionPtr {}

#[derive(Clone, Copy)]
pub struct StreamPtr(pub *mut sys::speer_yamux_stream_t);
unsafe impl Send for StreamPtr {}
unsafe impl Sync for StreamPtr {}

pub struct Identity {
    pub static_pub: [u8; 32],
    pub static_priv: [u8; 32],
    pub ed_pub: [u8; 32],
    pub ed_seed: [u8; 32],
    pub peer_id: String,
}

pub unsafe fn boxed_zeroed<T>() -> Box<T> {
    let layout = Layout::new::<T>();
    let ptr = unsafe { alloc_zeroed(layout) as *mut T };
    if ptr.is_null() {
        handle_alloc_error(layout);
    }
    unsafe { Box::from_raw(ptr) }
}

pub fn cstring(s: &str) -> CString {
    CString::new(s.replace('\0', "")).expect("nul stripped")
}

pub fn c_char_array_to_string(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).to_string()
}

pub fn random_bytes<const N: usize>() -> AnyResult<[u8; N]> {
    let mut out = [0u8; N];
    let rc = unsafe { sys::speer_random_bytes_or_fail(out.as_mut_ptr(), out.len()) };
    if rc != 0 {
        return Err("speer_random_bytes_or_fail failed".into());
    }
    Ok(out)
}

pub fn make_identity() -> AnyResult<Identity> {
    let static_priv = random_bytes::<32>()?;
    let mut static_pub = [0u8; 32];
    unsafe {
        sys::speer_x25519_base(static_pub.as_mut_ptr(), static_priv.as_ptr());
    }

    let mut ed_seed = random_bytes::<32>()?;
    let seed_copy = ed_seed;
    let mut ed_pub = [0u8; 32];
    unsafe {
        sys::speer_ed25519_keypair(
            ed_pub.as_mut_ptr(),
            ed_seed.as_mut_ptr(),
            seed_copy.as_ptr(),
        );
    }

    let mut pkproto = [0u8; 64];
    let mut pkproto_len = 0usize;
    let rc = unsafe {
        sys::speer_libp2p_pubkey_proto_encode(
            pkproto.as_mut_ptr(),
            pkproto.len(),
            sys::speer_libp2p_keytype_t_SPEER_LIBP2P_KEY_ED25519,
            ed_pub.as_ptr(),
            ed_pub.len(),
            &mut pkproto_len,
        )
    };
    if rc != 0 {
        return Err("pubkey proto encode failed".into());
    }

    let mut pid = [0u8; 64];
    let mut pid_len = 0usize;
    let rc = unsafe {
        sys::speer_peer_id_from_pubkey_bytes(
            pid.as_mut_ptr(),
            pid.len(),
            pkproto.as_ptr(),
            pkproto_len,
            &mut pid_len,
        )
    };
    if rc != 0 {
        return Err("peer id generation failed".into());
    }

    let mut b58 = [0 as c_char; 64];
    let rc =
        unsafe { sys::speer_peer_id_to_b58(b58.as_mut_ptr(), b58.len(), pid.as_ptr(), pid_len) };
    if rc != 0 {
        return Err("peer id base58 failed".into());
    }

    Ok(Identity {
        static_pub,
        static_priv,
        ed_pub,
        ed_seed,
        peer_id: c_char_array_to_string(&b58),
    })
}

pub fn identity_raw(identity: &Identity) -> sys::speer_libp2p_identity_t {
    sys::speer_libp2p_identity_t {
        static_pub: identity.static_pub.as_ptr(),
        static_priv: identity.static_priv.as_ptr(),
        keytype: sys::speer_libp2p_keytype_t_SPEER_LIBP2P_KEY_ED25519,
        libp2p_pub: identity.ed_pub.as_ptr(),
        libp2p_pub_len: identity.ed_pub.len(),
        libp2p_priv: identity.ed_seed.as_ptr(),
        libp2p_priv_len: identity.ed_seed.len(),
    }
}

pub fn discover_lan_ip() -> String {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            let _ = s.connect("1.1.1.1:53");
            s.local_addr()
        })
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

pub fn tcp_listen(preferred: u16) -> AnyResult<(i32, u16)> {
    let ports: Vec<u16> = if preferred != 0 {
        vec![preferred]
    } else {
        (4001..4100).collect()
    };

    for port in ports {
        let mut fd = -1;
        let rc = unsafe { sys::speer_tcp_listen(&mut fd, ptr::null(), port) };
        if rc == 0 && fd >= 0 {
            return Ok((fd, port));
        }
    }
    Err("tcp listen failed".into())
}

pub fn tcp_dial(host: &str, port: u16) -> Option<i32> {
    let host = cstring(host);
    let mut fd = -1;
    let rc = unsafe { sys::speer_tcp_dial_timeout(&mut fd, host.as_ptr(), port, 3000) };
    if rc == 0 && fd >= 0 {
        Some(fd)
    } else {
        None
    }
}

pub fn random_instance_name() -> String {
    static ALPHA: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rb = [0u8; 32];
    unsafe {
        sys::speer_random_bytes(rb.as_mut_ptr(), rb.len());
    }
    rb.iter()
        .map(|b| ALPHA[*b as usize % ALPHA.len()] as char)
        .take(MAX_NICK_LEN)
        .collect()
}
