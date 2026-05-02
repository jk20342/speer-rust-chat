use crate::constants::{
    CHAT_TYPE_FILE_ACCEPT, CHAT_TYPE_FILE_CHUNK, CHAT_TYPE_FILE_DONE, CHAT_TYPE_FILE_META,
    MAX_TEXT_LEN,
};
use crate::ffi::{cstring, SessionPtr, StreamPtr};
use crate::AnyResult;
use speer::sys;
use std::ptr;

pub fn chat_frame_encode(kind: u32, nick: &str, text: &str) -> AnyResult<Vec<u8>> {
    let mut buf = vec![0u8; MAX_TEXT_LEN + 256];
    let mut writer = unsafe { std::mem::zeroed::<sys::speer_pb_writer_t>() };
    unsafe {
        sys::speer_pb_writer_init(&mut writer, buf.as_mut_ptr(), buf.len());
        if sys::speer_pb_write_int32_field(&mut writer, 1, kind as i32) != 0 {
            return Err("protobuf type write failed".into());
        }
        if !nick.is_empty() {
            let nick = cstring(nick);
            if sys::speer_pb_write_string_field(&mut writer, 2, nick.as_ptr()) != 0 {
                return Err("protobuf nick write failed".into());
            }
        }
        if !text.is_empty() {
            let text = cstring(text);
            if sys::speer_pb_write_string_field(&mut writer, 3, text.as_ptr()) != 0 {
                return Err("protobuf text write failed".into());
            }
        }
    }
    buf.truncate(writer.pos);
    Ok(buf)
}

pub fn chat_frame_decode(frame: &[u8]) -> AnyResult<(u32, String, String)> {
    let mut reader = unsafe { std::mem::zeroed::<sys::speer_pb_reader_t>() };
    unsafe {
        sys::speer_pb_reader_init(&mut reader, frame.as_ptr(), frame.len());
    }
    let mut kind = 0u32;
    let mut nick = String::new();
    let mut text = String::new();

    while reader.pos < reader.len {
        let mut field = 0u32;
        let mut wire = 0u32;
        if unsafe { sys::speer_pb_read_tag(&mut reader, &mut field, &mut wire) } != 0 {
            return Err("protobuf tag read failed".into());
        }
        if field == 1 && wire == sys::PB_WIRE_VARINT {
            let mut value = 0u64;
            if unsafe { sys::speer_pb_read_varint(&mut reader, &mut value) } != 0 {
                return Err("protobuf varint read failed".into());
            }
            kind = value as u32;
        } else if (field == 2 || field == 3) && wire == sys::PB_WIRE_LEN {
            let mut data = ptr::null();
            let mut len = 0usize;
            if unsafe { sys::speer_pb_read_bytes(&mut reader, &mut data, &mut len) } != 0 {
                return Err("protobuf bytes read failed".into());
            }
            let value = if data.is_null() {
                String::new()
            } else {
                let bytes = unsafe { std::slice::from_raw_parts(data, len) };
                String::from_utf8_lossy(bytes).to_string()
            };
            if field == 2 {
                nick = value;
            } else {
                text = value;
            }
        } else if unsafe { sys::speer_pb_skip(&mut reader, wire) } != 0 {
            return Err("protobuf skip failed".into());
        }
    }

    Ok((kind, nick, text))
}

pub fn send_chat_frame(
    session: SessionPtr,
    stream: StreamPtr,
    kind: u32,
    nick: &str,
    text: &str,
) -> AnyResult<usize> {
    let frame = chat_frame_encode(kind, nick, text)?;
    let rc = unsafe {
        sys::speer_libp2p_tcp_stream_send_frame(session.0, stream.0, frame.as_ptr(), frame.len())
    };
    if rc != 0 {
        return Err("send frame failed".into());
    }
    Ok(unsafe { sys::speer_uvarint_size(frame.len() as u64) + frame.len() })
}

pub fn recv_chat_frame(session: SessionPtr, stream: StreamPtr) -> AnyResult<(Vec<u8>, usize)> {
    let mut frame = vec![0u8; MAX_TEXT_LEN + 256];
    let mut len = 0usize;
    let rc = unsafe {
        sys::speer_libp2p_tcp_stream_recv_frame(
            session.0,
            stream.0,
            frame.as_mut_ptr(),
            frame.len(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err("recv frame failed".into());
    }
    frame.truncate(len);
    let wire_len = unsafe { sys::speer_uvarint_size(len as u64) + len };
    Ok((frame, wire_len))
}

pub fn is_file_frame(kind: u32) -> bool {
    matches!(
        kind,
        CHAT_TYPE_FILE_META | CHAT_TYPE_FILE_CHUNK | CHAT_TYPE_FILE_DONE | CHAT_TYPE_FILE_ACCEPT
    )
}
