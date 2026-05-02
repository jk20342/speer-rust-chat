use std::net::{Ipv4Addr, ToSocketAddrs};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub trait IfEmpty {
    fn if_empty(self, fallback: &str) -> String;
}

impl IfEmpty for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_string()
        } else {
            self
        }
    }
}

pub fn color_idx(s: &str) -> usize {
    let mut h = 2166136261u32;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    (h as usize) % 6
}

pub fn truncate_pid(pid: &str) -> String {
    if pid.len() <= 14 {
        pid.to_string()
    } else {
        format!("{}..{}", &pid[..6], &pid[pid.len() - 6..])
    }
}

pub fn sanitize_file_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else if ch == ' ' {
            out.push('_');
        }
    }
    if out.is_empty() {
        "blob".to_string()
    } else {
        out
    }
}

pub fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

pub fn hex_decode(s: &str) -> Result<Vec<u8>, ()> {
    if !s.len().is_multiple_of(2) {
        return Err(());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let hi = (bytes[i] as char).to_digit(16).ok_or(())?;
        let lo = (bytes[i + 1] as char).to_digit(16).ok_or(())?;
        out.push(((hi << 4) | lo) as u8);
    }
    Ok(out)
}

pub fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

pub fn fmt_time(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

pub fn fmt_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs / 60) % 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

pub fn unix_time_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn discover_public_ipv4_http(timeout: Duration) -> Option<String> {
    try_fetch_ipv4_via_http("api.ipify.org:80", "api.ipify.org", "/", timeout)
        .or_else(|| try_fetch_ipv4_via_http("icanhazip.com:80", "icanhazip.com", "/", timeout))
}

fn try_fetch_ipv4_via_http(
    connect_addr: &str,
    host_header: &str,
    path: &str,
    timeout: Duration,
) -> Option<String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let addr = connect_addr.to_socket_addrs().ok()?.next()?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout).ok()?;
    let _ = stream.set_read_timeout(Some(timeout));
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\nUser-Agent: speer-chat\r\n\r\n",
        path = path,
        host_header = host_header,
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut buf = [0u8; 512];
    let n = stream.read(&mut buf).ok()?;
    let text = std::str::from_utf8(&buf[..n]).ok()?;
    let body = text.split("\r\n\r\n").nth(1)?.trim();
    let line = body.lines().next()?.trim();
    if line.parse::<Ipv4Addr>().is_ok() {
        Some(line.to_string())
    } else {
        None
    }
}
